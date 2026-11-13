// SPDX-License-Identifier: GPL-3.0-only
//! usbip-win2 CLI runner, console-session lookup and the USB/IP server's peer check.

use std::collections::BTreeSet;
use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::NO_ERROR;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_CONNECTIONS,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;

use super::services;
use crate::error::PadError;
use crate::usbip::{
    attach_argv, detach_argv, parse_attach_port, ConsoleSession, PeerAuth, ToolProbe, UsbipTool, DEFAULT_PORT,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(15);
/// Driver service installed by usbip-win2.
pub const UDE_SERVICE: &str = "usbip2_ude";

/// Pids of `usbip.exe` children this service is running right now.
type ToolPids = Arc<Mutex<BTreeSet<u32>>>;

pub struct WinUsbipTool {
    pub exe: PathBuf,
    running: ToolPids,
}

impl Default for WinUsbipTool {
    fn default() -> Self {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
        Self::at(PathBuf::from(pf).join("USBip").join("usbip.exe"))
    }
}

/// Removes a child pid from the running set when the run ends.
struct PidGuard(ToolPids, u32);

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Ok(mut s) = self.0.lock() {
            s.remove(&self.1);
        }
    }
}

impl WinUsbipTool {
    pub fn at(exe: PathBuf) -> Self {
        WinUsbipTool { exe, running: Arc::new(Mutex::new(BTreeSet::new())) }
    }

    /// Peer check for the USB/IP server that accepts this tool's own children.
    pub fn peer_auth(&self) -> WinPeerAuth {
        WinPeerAuth { tool_pids: self.running.clone() }
    }

    fn run(&self, args: &[String]) -> Result<String, PadError> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(CREATE_NO_WINDOW);
        // Shared spawn lock: no password pipe of seat_session::remote is inheritable meanwhile.
        let mut child = seat_core::spawn::spawn_locked(&mut cmd)
            .map_err(|e| PadError::Usbip(format!("cannot start usbip.exe: {e}")))?;
        if let Ok(mut s) = self.running.lock() {
            s.insert(child.id());
        }
        let _pid = PidGuard(self.running.clone(), child.id());
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() > TOOL_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PadError::Usbip(format!("usbip.exe {} timed out", args[0])));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(PadError::Usbip(e.to_string())),
            }
        };
        let mut out = String::new();
        if let Some(mut o) = child.stdout.take() {
            let _ = o.read_to_string(&mut out);
        }
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut out);
        }
        if !status.success() {
            return Err(PadError::Usbip(format!("usbip.exe {} failed: {}", args[0], out.trim())));
        }
        Ok(out)
    }
}

impl UsbipTool for WinUsbipTool {
    fn probe(&self) -> ToolProbe {
        let f = services::service_facts(UDE_SERVICE);
        let exe = self.exe.to_string_lossy().into_owned();
        ToolProbe {
            installed: f.exists && self.exe.exists(),
            running: f.running,
            version: services::file_version(&exe),
        }
    }

    fn attach(&self, host: &str, port: u16, busid: &str) -> Result<u32, PadError> {
        if port != DEFAULT_PORT {
            return Err(PadError::Usbip(format!(
                "usbip-win2 CLI documents no port option; server must listen on {DEFAULT_PORT}, not {port}"
            )));
        }
        let out = self.run(&attach_argv(host, busid))?;
        parse_attach_port(&out).ok_or_else(|| PadError::Usbip(format!("unexpected attach output: {}", out.trim())))
    }

    fn detach(&self, port: u32) -> Result<(), PadError> {
        self.run(&detach_argv(port)).map(|_| ())
    }
}

/// System process: kernel sockets (the `usbip2_ude` driver's WSK client) show this owner.
pub const SYSTEM_PID: u32 = 4;

/// Allows a loopback client only when the owner pid of its socket is System or a
/// `usbip.exe` child this service is running (`HYPOTHESIS`: usbip-win2 imports from the
/// kernel driver, so the owner is System; check on the first live test).
pub struct WinPeerAuth {
    tool_pids: ToolPids,
}

/// Owner pid of the IPv4 TCP connection `local -> remote`, from the owner-pid table.
pub fn tcp_owner_pid(local: SocketAddr, remote: SocketAddr) -> Option<u32> {
    let (IpAddr::V4(la), IpAddr::V4(ra)) = (local.ip(), remote.ip()) else { return None };
    let mut size = 0u32;
    // SAFETY: size query with a null buffer.
    unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_CONNECTIONS, 0) };
    for _ in 0..4 {
        // u32 elements keep the buffer aligned for the table.
        let mut buf = vec![0u32; (size as usize).div_ceil(4) + 64];
        let mut len = (buf.len() * 4) as u32;
        // SAFETY: buffer of `len` bytes.
        let rc = unsafe {
            GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut len, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_CONNECTIONS, 0)
        };
        if rc != NO_ERROR {
            size = len;
            continue;
        }
        // SAFETY: the API filled a MIB_TCPTABLE_OWNER_PID with dwNumEntries rows.
        let rows = unsafe {
            let t = buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
            let n = (*t).dwNumEntries as usize;
            let max = (len as usize).saturating_sub(4) / std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
            std::slice::from_raw_parts((*t).table.as_ptr(), n.min(max))
        };
        let port = |p: u32| u16::from_be(p as u16);
        let addr = |a: u32| std::net::Ipv4Addr::from(a.to_ne_bytes());
        return rows
            .iter()
            .find(|r| {
                addr(r.dwLocalAddr) == la
                    && port(r.dwLocalPort) == local.port()
                    && addr(r.dwRemoteAddr) == ra
                    && port(r.dwRemotePort) == remote.port()
            })
            .map(|r| r.dwOwningPid);
    }
    None
}

impl PeerAuth for WinPeerAuth {
    fn authorize(&self, server: SocketAddr, client: SocketAddr) -> Result<(), String> {
        // The client's row: local = client address, remote = our listener.
        let pid = tcp_owner_pid(client, server).ok_or("owner pid not found")?;
        if pid == SYSTEM_PID {
            return Ok(());
        }
        if self.tool_pids.lock().map(|s| s.contains(&pid)).unwrap_or(false) {
            return Ok(());
        }
        Err(format!("owner pid {pid} is neither System nor our usbip.exe"))
    }
}

pub struct WinConsoleSession;

impl ConsoleSession for WinConsoleSession {
    fn active_console_session(&self) -> u32 {
        // SAFETY: trivial FFI.
        unsafe { WTSGetActiveConsoleSessionId() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_port_rejected_without_running_anything() {
        let t = WinUsbipTool::at(PathBuf::from("C:\\definitely\\missing\\usbip.exe"));
        assert!(t.attach("127.0.0.1", 1234, "1-1").is_err());
        assert!(!t.probe().installed);
        assert!(t.detach(1).is_err(), "missing exe");
    }

    /// Reads the TCP table for a loopback pair this test owns (no system change).
    #[test]
    fn peer_auth_finds_own_pid() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = l.local_addr().unwrap();
        let c = std::net::TcpStream::connect(server).unwrap();
        let (_s, client) = l.accept().unwrap();
        assert_eq!(c.local_addr().unwrap(), client);
        assert_eq!(tcp_owner_pid(client, server), Some(std::process::id()));
        let t = WinUsbipTool::at(PathBuf::from("x"));
        assert!(t.peer_auth().authorize(server, client).is_err(), "our own pid is not a tool child");
        t.running.lock().unwrap().insert(std::process::id());
        assert!(t.peer_auth().authorize(server, client).is_ok());
    }
}
