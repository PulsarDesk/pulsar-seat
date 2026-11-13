// SPDX-License-Identifier: GPL-3.0-only
//! Windows implementation of [`crate::remote::control::Platform`] (from Pulsar Split).
//!
//! Local user enumeration, user checks and the console user live here; everything seat
//! related is delegated to the [`Engine`] (`crate::remote::engine`). The remote setting
//! `start_with_windows` no longer changes the service start type: there is one service
//! (Pulsar Seat's), installed as automatic.

use crate::remote::control::{Platform, SeatEvent};
use crate::remote::engine::Engine;
use crate::win::security::wide;
use seat_core::remote::streamconf::SeatFiles;
use seat_core::remote::validate::UserFacts;
use seat_proto::remote::{
    ErrorCode, RemoteSettings, HostInfo, IsolationStatus, LocalUser, PairState, PairedClient, ProtoError, RemoteSeatRecord,
    SeatRuntime, Secret, ShimTargetDto, StreamBackendInfo,
};
use std::path::PathBuf;
use std::ptr;
use windows_sys::Win32::NetworkManagement::NetManagement::{
    FILTER_NORMAL_ACCOUNT, NetApiBufferFree, NetUserEnum, NetUserGetInfo, UF_ACCOUNTDISABLE, UF_LOCKOUT,
    UF_PASSWORD_EXPIRED, USER_INFO_1,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW, WTSUserName,
};

const NERR_SUCCESS: u32 = 0;
const ERROR_MORE_DATA: u32 = 234;
const NERR_USER_NOT_FOUND: u32 = 2221;

pub struct WindowsPlatform {
    /// Phase 1 per-seat config folder (`config\<seat>.conf`); only cleaned up now, the
    /// stream package writes `seats\<seat-id>\` (contracts §7.3).
    pub config_dir: PathBuf,
    /// `None` when the packages could not be wired (logged at start): v1 requests still
    /// work, seat start and v2 features answer `unsupported`.
    pub engine: Option<Engine>,
    /// Last applied global settings (requests that need them without a store read).
    pub global: RemoteSettings,
}

fn pwstr_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: caller passes a NUL-terminated wide string owned by the system.
    unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

fn no_engine() -> ProtoError {
    ProtoError::new(ErrorCode::Unsupported, "unsupported: seat runtime is not available (see the service log)")
        .with_reason("unsupported")
}

impl WindowsPlatform {
    fn engine(&self) -> Result<&Engine, ProtoError> {
        self.engine.as_ref().ok_or_else(no_engine)
    }
}

impl Platform for WindowsPlatform {
    fn start_seat(&mut self, seat: &RemoteSeatRecord, global: &RemoteSettings, password: Option<Secret>) -> Result<(), ProtoError> {
        self.global = global.clone();
        self.engine()?.start_seat(seat, global, password)
    }

    fn stop_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        self.engine()?.stop_seat(seat)
    }

    fn remove_seat_files(&mut self, seat: &RemoteSeatRecord) {
        // Phase 1 layout leftovers.
        let files = SeatFiles::new(&self.config_dir, &seat.name);
        for p in files.all() {
            match std::fs::remove_file(p) {
                Ok(()) => crate::log::info(&format!("removed {}", p.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => crate::log::warn(&format!("could not remove {}: {e}", p.display())),
            }
        }
        if let Some(e) = &self.engine {
            e.remove_seat_files(seat);
        }
    }

    fn submit_pin(&mut self, seat: &RemoteSeatRecord, pin: &Secret, client: &str, request_id: Option<u32>) -> Result<(), ProtoError> {
        self.engine()?.submit_pin(seat, pin, client, request_id)
    }

    fn apply_global(&mut self, old: &RemoteSettings, new: &RemoteSettings) -> Result<(), ProtoError> {
        if let Some(e) = &self.engine {
            e.apply_isolation(old, new)?;
            e.set_pad_preference(new.pad_backend);
            if old.bluetooth_console_only != new.bluetooth_console_only {
                e.set_bluetooth_console_only(new.bluetooth_console_only);
            }
        }
        self.global = new.clone();
        Ok(())
    }

    fn client_name_for(&self, seat_id: &str, taken: &[String]) -> String {
        seat_session::remote::discover::unique_client_name(seat_id, taken)
    }

    fn listening_ports(&self) -> Vec<u16> {
        use seat_stream::ports::ListenerProbe;
        match seat_stream::win::IpHelperProbe.listening() {
            Ok(v) => v.into_iter().map(|(p, _)| p).collect(),
            Err(e) => {
                crate::log::warn(&format!("listening ports not read: {e}"));
                Vec::new()
            }
        }
    }

    fn take_reports(&mut self) -> Vec<(String, SeatEvent)> {
        self.engine.as_ref().map(Engine::take_reports).unwrap_or_default()
    }

    fn test_logon(&self, user: &str, password: &Secret) -> Result<Result<(), u32>, ProtoError> {
        Ok(seat_session::remote::win::accounts::test_logon(user, password))
    }

    fn win32_message(&self, code: u32) -> String {
        std::io::Error::from_raw_os_error(code as i32).to_string()
    }

    fn log_tail(&mut self, seat: Option<&RemoteSeatRecord>, n: usize) -> Result<Vec<String>, ProtoError> {
        match seat {
            None => {
                let root = self.config_dir.parent().map(PathBuf::from).unwrap_or_else(|| self.config_dir.clone());
                Ok(crate::remote::tail_file(&root.join("logs").join("service.log"), n))
            }
            Some(s) => Ok(self.engine()?.seat_log_tail(s, n)),
        }
    }

    fn reboot_host(&mut self) -> Result<(), ProtoError> {
        crate::log::info("restart requested by the operator");
        reboot().map_err(|e| ProtoError::new(ErrorCode::Internal, format!("restart: {e}")))
    }

    fn enable_unlock_account(&mut self, user: &str, enable: bool, unlock: bool) -> Result<(), ProtoError> {
        seat_session::remote::win::accounts::enable_unlock(user, enable, unlock)
            .map_err(|e| ProtoError::new(ErrorCode::InvalidUser, format!("account: {e}")).with_reason("account"))?;
        crate::log::info(&format!("account '{user}': enable={enable} unlock={unlock} (operator confirmed)"));
        Ok(())
    }

    fn passive(&self) -> bool {
        self.engine.as_ref().is_some_and(Engine::passive)
    }

    fn host_class(&self) -> Option<String> {
        self.engine.as_ref().and_then(Engine::host_class)
    }

    fn host_info(&mut self, global: &RemoteSettings) -> Result<HostInfo, ProtoError> {
        Ok(self.engine()?.host_info(global))
    }

    fn set_multi_session(&mut self, enabled: bool) -> Result<(), ProtoError> {
        self.engine()?.set_multi_session(enabled)?;
        self.global.multi_session = enabled;
        Ok(())
    }

    fn termwrap_status(&mut self) -> Result<serde_json::Value, ProtoError> {
        self.engine()?.termwrap_status()
    }

    fn set_termwrap(&mut self, enabled: bool, wrapper: Option<&str>, ack: u32) -> Result<serde_json::Value, ProtoError> {
        self.engine()?.set_termwrap(enabled, wrapper, ack)
    }

    fn seat_runtime(&mut self, seat: &RemoteSeatRecord) -> Result<SeatRuntime, ProtoError> {
        Ok(self.engine()?.seat_runtime(seat))
    }

    fn render_adapters(&mut self) -> Result<Vec<serde_json::Value>, ProtoError> {
        self.engine()?.render_adapters()
    }

    fn pad_backends(&mut self) -> Result<Vec<serde_json::Value>, ProtoError> {
        Ok(self.engine()?.pad_backends())
    }

    fn pair_state(&mut self, seat: &RemoteSeatRecord) -> Result<PairState, ProtoError> {
        Ok(self.engine()?.pair_state(seat))
    }

    fn cancel_pair(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        self.engine()?.cancel_pair(seat)
    }

    fn paired_clients(&mut self, seat: &RemoteSeatRecord) -> Result<Vec<PairedClient>, ProtoError> {
        self.engine()?.paired_clients(seat)
    }

    fn unpair_client(&mut self, seat: &RemoteSeatRecord, uuid: &str) -> Result<(), ProtoError> {
        self.engine()?.unpair_client(seat, uuid)
    }

    fn unpair_all(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        self.engine()?.unpair_all(seat)
    }

    fn regenerate_identity(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        self.engine()?.regenerate_identity(seat)
    }

    fn get_apps(&mut self, seat: &RemoteSeatRecord) -> Result<Vec<serde_json::Value>, ProtoError> {
        self.engine()?.get_apps(seat)
    }

    fn set_apps(&mut self, seat: &RemoteSeatRecord, apps: Vec<serde_json::Value>) -> Result<Vec<serde_json::Value>, ProtoError> {
        let g = self.global.clone();
        self.engine()?.set_apps(seat, apps, &g)
    }

    fn isolation_status(&mut self) -> Result<IsolationStatus, ProtoError> {
        self.engine()?.isolation_status()
    }

    fn set_shim_targets(&mut self, targets: Vec<ShimTargetDto>, _global: &RemoteSettings) -> Result<IsolationStatus, ProtoError> {
        self.engine()?.set_shim_targets(&targets)
    }

    fn set_app_masking(&mut self, exe_path: &str, enabled: bool) -> Result<IsolationStatus, ProtoError> {
        self.engine()?.set_app_masking(exe_path, enabled)
    }

    fn set_dxvk(&mut self, game_exe: &str, enabled: bool, dlls: &[String]) -> Result<(), ProtoError> {
        self.engine()?.set_dxvk(game_exe, enabled, dlls)
    }

    fn dxvk_status(&mut self) -> Result<seat_proto::remote::DxvkStatus, ProtoError> {
        self.engine()?.dxvk_status()
    }

    fn user_picture(&mut self, user: &str) -> Result<seat_proto::remote::UserPicture, ProtoError> {
        Ok(crate::remote::win::picture::read(user))
    }

    fn cleanup_audio_endpoints(&mut self) -> Result<seat_proto::remote::AudioCleanup, ProtoError> {
        self.engine()?.cleanup_audio_endpoints()
    }

    fn stream_backends(&mut self) -> Result<Vec<StreamBackendInfo>, ProtoError> {
        Ok(self.engine()?.stream_backends())
    }

    fn answer_peer_request(&mut self, seat: &RemoteSeatRecord, request_id: u32, allow: bool, view_only: bool) -> Result<(), ProtoError> {
        self.engine()?.answer_peer_request(seat, request_id, allow, view_only)
    }

    fn set_connect_password(&mut self, seat: &RemoteSeatRecord, password: Option<&Secret>) -> Result<(), ProtoError> {
        self.engine()?.set_connect_password(seat, password)
    }

    fn seat_otp(&mut self, seat: &RemoteSeatRecord, rotate: bool) -> Result<Secret, ProtoError> {
        self.engine()?.seat_otp(seat, rotate)
    }

    fn answer_relay_auth(&mut self, seat: &RemoteSeatRecord, password: &Secret, totp: Option<&Secret>) -> Result<(), ProtoError> {
        self.engine()?.answer_relay_auth(seat, password, totp)
    }

    fn list_local_users(&self) -> Result<Vec<LocalUser>, ProtoError> {
        let mut out = Vec::new();
        let mut resume = 0u32;
        loop {
            let mut buf: *mut u8 = ptr::null_mut();
            let (mut read, mut total) = (0u32, 0u32);
            // SAFETY: out pointers valid; buffer freed with NetApiBufferFree.
            let rc = unsafe {
                NetUserEnum(ptr::null(), 1, FILTER_NORMAL_ACCOUNT, &mut buf, u32::MAX, &mut read, &mut total, &mut resume)
            };
            if rc != NERR_SUCCESS && rc != ERROR_MORE_DATA {
                return Err(ProtoError::new(ErrorCode::Internal, format!("NetUserEnum failed: {rc}")));
            }
            if !buf.is_null() {
                // SAFETY: the system returned `read` USER_INFO_1 entries.
                let entries = unsafe { std::slice::from_raw_parts(buf as *const USER_INFO_1, read as usize) };
                for e in entries {
                    out.push(LocalUser {
                        name: pwstr_to_string(e.usri1_name),
                        enabled: e.usri1_flags & UF_ACCOUNTDISABLE == 0,
                    });
                }
                // SAFETY: allocated by NetUserEnum.
                unsafe { NetApiBufferFree(buf as _) };
            }
            if rc != ERROR_MORE_DATA {
                break;
            }
        }
        out.sort_by_key(|u| u.name.to_lowercase());
        Ok(out)
    }

    fn user_facts(&self, user: &str) -> UserFacts {
        if user.is_empty() {
            return UserFacts::default();
        }
        let name = wide(user);
        let mut buf: *mut u8 = ptr::null_mut();
        // SAFETY: valid name; buffer freed below.
        let rc = unsafe { NetUserGetInfo(ptr::null(), name.as_ptr(), 1, &mut buf) };
        if rc != NERR_SUCCESS || buf.is_null() {
            if rc != NERR_USER_NOT_FOUND {
                crate::log::warn(&format!("NetUserGetInfo failed: {rc}"));
            }
            return UserFacts::default();
        }
        // SAFETY: level 1 buffer.
        let flags = unsafe { (*(buf as *const USER_INFO_1)).usri1_flags };
        // SAFETY: allocated by NetUserGetInfo.
        unsafe { NetApiBufferFree(buf as _) };
        UserFacts {
            exists: true,
            enabled: flags & UF_ACCOUNTDISABLE == 0,
            locked_out: flags & UF_LOCKOUT != 0,
            password_expired: flags & UF_PASSWORD_EXPIRED != 0,
        }
    }

    fn console_user(&self) -> Option<String> {
        // SAFETY: plain query; buffer freed with WTSFreeMemory.
        unsafe {
            let sid = WTSGetActiveConsoleSessionId();
            if sid == u32::MAX {
                return None;
            }
            let mut buf: *mut u16 = ptr::null_mut();
            let mut len = 0u32;
            if WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, sid, WTSUserName, &mut buf, &mut len) == 0 {
                return None;
            }
            let name = pwstr_to_string(buf);
            WTSFreeMemory(buf as _);
            (!name.is_empty()).then_some(name)
        }
    }
}

/// Restart after a 30 s grace (logged-on users see the notice). Enables
/// `SeShutdownPrivilege` on the service token first.
fn reboot() -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LUID};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Shutdown::{
        InitiateShutdownW, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION, SHTDN_REASON_MINOR_RECONFIG,
        SHUTDOWN_RESTART,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: plain token calls on our own process; handle closed below.
    unsafe {
        let mut tok: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut tok) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut luid = LUID { LowPart: 0, HighPart: 0 };
        let ok = LookupPrivilegeValueW(ptr::null(), SE_SHUTDOWN_NAME, &mut luid);
        if ok != 0 {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            AdjustTokenPrivileges(tok, 0, &tp, 0, ptr::null_mut(), ptr::null_mut());
        }
        CloseHandle(tok);
        let msg = wide("Pulsar Seat: işlem yamalama değişikliği için Windows yeniden başlatılıyor.");
        let rc = InitiateShutdownW(
            ptr::null(),
            msg.as_ptr(),
            30,
            SHUTDOWN_RESTART,
            SHTDN_REASON_MAJOR_APPLICATION | SHTDN_REASON_MINOR_RECONFIG | SHTDN_REASON_FLAG_PLANNED,
        );
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare() -> WindowsPlatform {
        WindowsPlatform { config_dir: std::env::temp_dir(), engine: None, global: RemoteSettings::default() }
    }

    #[test]
    fn local_users_can_be_listed() {
        let p = bare();
        let users = p.list_local_users().unwrap();
        assert!(!users.is_empty(), "every Windows machine has at least one local account");
        assert_eq!(p.user_facts("pulsar-split-no-such-user-x"), UserFacts::default());
    }

    #[test]
    fn without_engine_seat_start_is_unsupported_and_cleanup_is_safe() {
        let dir = std::env::temp_dir().join(format!("pulsar-split-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t1.conf"), "x").unwrap();
        let mut p = WindowsPlatform { config_dir: dir.clone(), ..bare() };
        let seat = RemoteSeatRecord { name: "t1".into(), display_name: "T 1".into(), port: 47989, ..Default::default() };
        let e = p.start_seat(&seat, &RemoteSettings::default(), None).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        p.remove_seat_files(&seat);
        assert!(!dir.join("t1.conf").exists());
        let n = p.client_name_for("0f8fad5b-d9cb-469f-a165-70867728950e", &[]);
        assert!(n.starts_with("PSS") && n.len() == 11, "{n}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
