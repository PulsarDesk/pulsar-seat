// SPDX-License-Identifier: GPL-3.0-only
//! Windows part of the launcher.

use seat_shim_core::launch::{Arch, LaunchArgs, LaunchPlan, attach_exit, build_cmdline, delegate_args, pe_machine, plan_launch};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{CloseHandle, FreeLibrary, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::LibraryLoader::{DONT_RESOLVE_DLL_REFERENCES, GetModuleHandleW, GetProcAddress, LoadLibraryExW};
use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, CreateRemoteThread, GetExitCodeProcess,
    GetExitCodeThread, INFINITE, OpenProcess, PROCESS_CREATE_THREAD, PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
    QueryFullProcessImageNameW, ResumeThread, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};

const REMOTE_TIMEOUT_MS: u32 = 15_000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn own_arch() -> Arch {
    Arch::current()
}

/// The PC is ARM64 (also true for this x64 / x86 launcher running under emulation there).
fn native_is_arm64() -> bool {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
    let (mut process, mut native) = (0u16, 0u16);
    // SAFETY: pseudo handle of this process, valid out pointers.
    let ok = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } != 0;
    ok && native == seat_shim_core::launch::IMAGE_FILE_MACHINE_ARM64
}

/// Resolves a bare exe name through `PATH` (with `.exe` appended when missing).
fn resolve_exe(p: &Path) -> PathBuf {
    if p.is_file() || p.components().count() > 1 {
        return p.to_path_buf();
    }
    let name = if p.extension().is_none() { p.with_extension("exe") } else { p.to_path_buf() };
    std::env::var_os("PATH")
        .and_then(|path| std::env::split_paths(&path).map(|d| d.join(&name)).find(|c| c.is_file()))
        .unwrap_or_else(|| p.to_path_buf())
}

fn read_head(p: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut b = vec![0u8; 4096];
    let n = f.read(&mut b).ok()?;
    b.truncate(n);
    Some(b)
}

struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: handle owned by this guard.
            unsafe { CloseHandle(self.0) };
        }
    }
}

pub fn run(a: &LaunchArgs) -> i32 {
    if let Some(pid) = a.attach {
        return attach(pid, a);
    }
    let exe = resolve_exe(&a.exe);
    let machine = read_head(&exe).as_deref().and_then(pe_machine);
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    match plan_launch(own_arch(), machine, &dir, a) {
        LaunchPlan::Delegate { launcher } => match std::process::Command::new(&launcher).args(delegate_args(a)).status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("shim_blocked: cannot start {}: {e}", launcher.display());
                start(&exe, a, None)
            }
        },
        LaunchPlan::NoInject { reason } => {
            if reason != "no_inject" {
                eprintln!("shim_unsupported: {reason}; starting without isolation");
            }
            start(&exe, a, None)
        }
        LaunchPlan::InjectHere { module } => start(&exe, a, Some(&module)),
    }
}

/// Image path of a process (`QueryFullProcessImageNameW`).
fn process_image(process: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: buffer valid for `len` characters.
    if unsafe { QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// `--attach <pid>` (round 4, kiosk shell): the creator started the process suspended and
/// resumes it itself after we exit. Never resumes, never kills: on a timeout the exit code
/// tells the creator to kill and restart it without the module. No `__COMPAT_LAYER` (the
/// child's environment is already fixed). HYPOTHESIS: never run on hardware.
fn attach(pid: u32, a: &LaunchArgs) -> i32 {
    let access = PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION
        | PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE;
    // SAFETY: plain call; the handle is owned by the guard.
    let process = Owned(unsafe { OpenProcess(access, 0, pid) });
    if process.0.is_null() {
        eprintln!("shim_blocked: OpenProcess {pid}: {}", std::io::Error::last_os_error());
        return attach_exit::BLOCKED;
    }
    let Some(exe) = process_image(process.0) else {
        eprintln!("shim_blocked: image of {pid}: {}", std::io::Error::last_os_error());
        return attach_exit::BLOCKED;
    };
    let machine = read_head(&exe).as_deref().and_then(pe_machine);
    let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    match plan_launch(own_arch(), machine, &dir, a) {
        LaunchPlan::Delegate { launcher } => match std::process::Command::new(&launcher).args(delegate_args(a)).status() {
            Ok(s) => s.code().unwrap_or(attach_exit::BLOCKED),
            Err(e) => {
                eprintln!("shim_blocked: cannot start {}: {e}", launcher.display());
                attach_exit::BLOCKED
            }
        },
        LaunchPlan::NoInject { reason } => {
            eprintln!("shim_unsupported: {reason}");
            attach_exit::BLOCKED
        }
        LaunchPlan::InjectHere { module } => match inject(process.0, pid, &module) {
            Ok(Injected::Active) => attach_exit::ACTIVE,
            Ok(Injected::Inactive) => attach_exit::INACTIVE,
            Ok(Injected::TimedOut) => {
                eprintln!("shim_blocked: remote thread timed out");
                attach_exit::TIMED_OUT
            }
            Err(e) => {
                eprintln!("shim_blocked: {e}");
                attach_exit::BLOCKED
            }
        },
    }
}

/// The system-directory copy AppHelp loads for a process of our architecture (`System32`
/// for the native 64-bit build, `SysWOW64` for x86 under WOW64). On an ARM64 PC the
/// `System32` copy is the ARM64 build, which an x64 process cannot load: the x64 launcher
/// there gets no copy (and its children no layer); it injects its own x64 module instead.
fn system_module_copy() -> Option<PathBuf> {
    if own_arch() == Arch::X64 && native_is_arm64() {
        return None;
    }
    use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetSystemWow64DirectoryW};
    let mut buf = vec![0u16; 520];
    // SAFETY: buffer valid for its length.
    let n = unsafe {
        if cfg!(target_pointer_width = "64") {
            GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32)
        } else {
            match GetSystemWow64DirectoryW(buf.as_mut_ptr(), buf.len() as u32) {
                0 => GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32),
                n => n,
            }
        }
    } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let name = if cfg!(target_pointer_width = "64") { seat_shim_core::MODULE_X64 } else { seat_shim_core::MODULE_X86 };
    let p = PathBuf::from(String::from_utf16_lossy(&buf[..n])).join(name);
    p.is_file().then_some(p)
}

/// Our database is registered and its module copy is present: give the child our layer in
/// its own environment and return the copy AppHelp will load in it. The launcher then
/// injects that same file (`LoadLibraryW` returns the copy AppHelp already mapped), so the
/// child never holds two copies of the module (review fix 2026-11-12).
fn add_layer_to_env() -> Option<PathBuf> {
    let key = format!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\InstalledSDB\{}", seat_shim_core::SHIM_DB_GUID);
    let installed = seat_shim_core::win::read_value(&key, "DatabaseDescription").is_some()
        || seat_shim_core::win::read_value(&key, "DatabasePath").is_some();
    if !installed {
        return None;
    }
    let copy = system_module_copy()?;
    let cur = std::env::var("__COMPAT_LAYER").unwrap_or_default();
    if !cur.split_whitespace().any(|t| t.eq_ignore_ascii_case(seat_shim_core::LAYER_NAME)) {
        let new = if cur.trim().is_empty() {
            seat_shim_core::LAYER_NAME.to_string()
        } else {
            format!("{cur} {}", seat_shim_core::LAYER_NAME)
        };
        // SAFETY: single-threaded at this point; only the child's inherited block is affected.
        unsafe { std::env::set_var("__COMPAT_LAYER", new) };
    }
    Some(copy)
}

/// Undoes [`add_layer_to_env`] (a restart without the module must not get it via AppHelp).
fn restore_layer_env(prev: Option<std::ffi::OsString>) {
    // SAFETY: single-threaded launcher.
    unsafe {
        match prev {
            Some(v) => std::env::set_var("__COMPAT_LAYER", v),
            None => std::env::remove_var("__COMPAT_LAYER"),
        }
    }
}

fn start(exe: &Path, a: &LaunchArgs, module: Option<&Path>) -> i32 {
    let prev_layer = std::env::var_os("__COMPAT_LAYER");
    let module: Option<PathBuf> = module.map(|m| add_layer_to_env().unwrap_or_else(|| m.to_path_buf()));
    let mut cmd = wide(&build_cmdline(exe, &a.args));
    let app = wide_path(exe);
    // SAFETY: zeroed STARTUPINFOW with cb set is the documented minimum.
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: plain out structure.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = CREATE_UNICODE_ENVIRONMENT | if module.is_some() { CREATE_SUSPENDED } else { 0 };
    // SAFETY: all pointers valid; inherits our token, session, environment and directory.
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        eprintln!("launch_failed: {}: {}", exe.display(), std::io::Error::last_os_error());
        return 1;
    }
    let process = Owned(pi.hProcess);
    let thread = Owned(pi.hThread);
    if let Some(m) = &module {
        match inject(process.0, pi.dwProcessId, m) {
            Ok(Injected::Active) => {}
            Ok(Injected::Inactive) => eprintln!("shim_inactive: module loaded, no hooks active for this process"),
            Ok(Injected::TimedOut) => {
                // A remote thread of ours may still run (LoadLibraryW, or PulsarSplitInit
                // still patching prologues): never resume next to it. Kill the suspended
                // child and start it again without the module.
                eprintln!("shim_blocked: remote thread timed out; restarting without isolation");
                // SAFETY: our own suspended child.
                unsafe {
                    TerminateProcess(process.0, 1);
                    WaitForSingleObject(process.0, 5_000);
                }
                drop(thread);
                drop(process);
                restore_layer_env(prev_layer);
                return start(exe, a, None);
            }
            Err(e) => eprintln!("shim_blocked: {e}; running without isolation"),
        }
        // SAFETY: main thread of the suspended child.
        unsafe { ResumeThread(thread.0) };
    }
    if !a.wait {
        return 0;
    }
    let mut code = 0u32;
    // SAFETY: valid process handle.
    unsafe {
        WaitForSingleObject(process.0, INFINITE);
        GetExitCodeProcess(process.0, &mut code);
    }
    code as i32
}

enum Remote {
    Done(u32),
    /// The thread is still running in the target.
    TimedOut,
}

/// `Ok(true)` / `Ok(false)` of the old interface, plus a timeout the caller must handle.
enum Injected {
    Active,
    Inactive,
    TimedOut,
}

/// Runs `start(param)` in the target and waits; returns the thread's exit code.
fn remote_call(process: HANDLE, start: usize, param: *const c_void) -> Result<Remote, String> {
    // SAFETY: `start` is a function address valid in the target (same system DLL base or
    // computed from the module base); the thread start signature matches.
    unsafe {
        let routine: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(start);
        let t = Owned(CreateRemoteThread(process, std::ptr::null(), 0, Some(routine), param, 0, std::ptr::null_mut()));
        if t.0.is_null() {
            return Err(format!("CreateRemoteThread: {}", std::io::Error::last_os_error()));
        }
        if WaitForSingleObject(t.0, REMOTE_TIMEOUT_MS) != WAIT_OBJECT_0 {
            return Ok(Remote::TimedOut);
        }
        let mut code = 0u32;
        GetExitCodeThread(t.0, &mut code);
        Ok(Remote::Done(code))
    }
}

/// Case-insensitive, `\\?\` prefix ignored.
fn same_path(a: &str, b: &Path) -> bool {
    let strip = |s: &str| s.strip_prefix(r"\\?\").unwrap_or(s).replace('/', "\\").to_lowercase();
    strip(a) == strip(&b.to_string_lossy())
}

/// Base of the module mapped from exactly `path` in the target (full path, not base name:
/// another copy of the module may be loaded too).
fn remote_module_base(pid: u32, path: &Path) -> Option<usize> {
    // SAFETY: snapshot handle closed by the guard; entry has dwSize set.
    unsafe {
        let snap = Owned(CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid));
        if snap.0 == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut me: MODULEENTRY32W = std::mem::zeroed();
        me.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
        let mut ok = Module32FirstW(snap.0, &mut me);
        while ok != 0 {
            let end = me.szExePath.iter().position(|c| *c == 0).unwrap_or(me.szExePath.len());
            if same_path(&String::from_utf16_lossy(&me.szExePath[..end]), path) {
                return Some(me.modBaseAddr as usize);
            }
            ok = Module32NextW(snap.0, &mut me);
        }
        None
    }
}

/// RVA of `PulsarSplitInit`, from a local, non-executing map of the module.
fn init_rva(module: &Path) -> Option<usize> {
    let w = wide_path(module);
    // SAFETY: DONT_RESOLVE_DLL_REFERENCES maps the image without running DllMain.
    unsafe {
        let h = LoadLibraryExW(w.as_ptr(), std::ptr::null_mut(), DONT_RESOLVE_DLL_REFERENCES);
        if h.is_null() {
            return None;
        }
        let p = GetProcAddress(h, c"PulsarSplitInit".as_ptr().cast()).map(|f| f as usize);
        let base = h as usize;
        FreeLibrary(h);
        p.map(|p| p - base)
    }
}

/// Loads the module and runs `PulsarSplitInit` in the suspended target.
fn inject(process: HANDLE, pid: u32, module: &Path) -> Result<Injected, String> {
    let module = std::fs::canonicalize(module).map_err(|e| format!("{}: {e}", module.display()))?;
    // `canonicalize` returns a `\\?\` path; keep the plain form for LoadLibraryW.
    let module = match module.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC") => PathBuf::from(plain),
        _ => module,
    };
    let path = wide_path(&module);
    let bytes = path.len() * 2;
    // SAFETY: remote allocation freed below; LoadLibraryW lives at the same address in the
    // target (system DLLs share their base per boot and bitness).
    unsafe {
        let mem = VirtualAllocEx(process, std::ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if mem.is_null() {
            return Err(format!("VirtualAllocEx: {}", std::io::Error::last_os_error()));
        }
        let res = (|| {
            if WriteProcessMemory(process, mem, path.as_ptr().cast(), bytes, std::ptr::null_mut()) == 0 {
                return Err(format!("WriteProcessMemory: {}", std::io::Error::last_os_error()));
            }
            let k32 = GetModuleHandleW(wide("kernel32.dll").as_ptr());
            let load = GetProcAddress(k32, c"LoadLibraryW".as_ptr().cast()).ok_or("LoadLibraryW missing")?;
            // The exit code (a truncated HMODULE on x64) is not used: success is decided
            // by finding the module below.
            remote_call(process, load as usize, mem)
        })();
        match res {
            // LoadLibraryW may still read the path: leak the buffer, never free it under it.
            Ok(Remote::TimedOut) => return Ok(Injected::TimedOut),
            Ok(Remote::Done(_)) => {
                VirtualFreeEx(process, mem, 0, MEM_RELEASE);
            }
            Err(e) => {
                VirtualFreeEx(process, mem, 0, MEM_RELEASE);
                return Err(e);
            }
        }
    }
    let base = remote_module_base(pid, &module)
        .ok_or("LoadLibraryW failed in the target (blocked by policy or missing)")?;
    let rva = init_rva(&module).ok_or("PulsarSplitInit export missing")?;
    Ok(match remote_call(process, base + rva, std::ptr::null())? {
        Remote::Done(1) => Injected::Active,
        Remote::Done(_) => Injected::Inactive,
        Remote::TimedOut => Injected::TimedOut,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_paths_compare_in_full() {
        let p = Path::new(r"C:\Program Files\Pulsar Split\shim\x64\PulsarSplitShim.dll");
        assert!(same_path(r"c:\program files\pulsar split\shim\x64\pulsarsplitshim.dll", p));
        assert!(same_path(r"\\?\C:\Program Files\Pulsar Split\shim\x64\PulsarSplitShim.dll", p));
        assert!(!same_path(r"C:\Windows\System32\PulsarSplitShim.dll", p));
    }
}
