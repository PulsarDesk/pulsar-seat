// SPDX-License-Identifier: GPL-3.0-only
//! Inline-hook backends behind one small interface, used only by [`super::hook`]:
//!
//! - x86 / x64: `retour::RawDetour` (BSD-2-Clause), unchanged since round 1;
//! - native ARM64: Microsoft Detours (MIT, vendored in `shim/third_party/detours`, built by
//!   `build.rs`), called over FFI. One Detours transaction per hook.
//!
//! Protocol (both backends): [`Prepared::new`] allocates everything (trampoline, Detours
//! operation record) while no thread is suspended and returns the trampoline address, which
//! the caller publishes before the jump is written; [`Prepared::enable`] writes the jump and
//! is the only part that runs with the other threads frozen ([`super::threads`]). A prepared
//! hook that is dropped without a successful `enable` leaves the target untouched.

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod imp {
    pub struct Prepared(retour::RawDetour);

    impl Prepared {
        /// # Safety
        /// `target` is a function entry; `detour` has exactly its signature.
        pub unsafe fn new(target: usize, detour: *const ()) -> Result<Self, String> {
            // SAFETY: forwarded from the caller.
            unsafe { retour::RawDetour::new(target as *const (), detour) }.map(Prepared).map_err(|e| e.to_string())
        }
        pub fn trampoline(&self) -> usize {
            self.0.trampoline() as *const () as usize
        }
        /// # Safety
        /// Patches the target prologue; the caller has the other threads suspended.
        pub unsafe fn enable(self) -> Result<(), String> {
            // SAFETY: forwarded from the caller.
            unsafe { self.0.enable() }.map_err(|e| e.to_string())?;
            // The detour must live as long as the process (the module is pinned).
            std::mem::forget(self);
            Ok(())
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use std::ffi::c_void;
    use std::sync::{Mutex, MutexGuard};

    // detours.h (vendored): LONG WINAPI ... ; WINAPI is the default convention on ARM64.
    unsafe extern "system" {
        fn DetourTransactionBegin() -> i32;
        fn DetourTransactionAbort() -> i32;
        fn DetourTransactionCommit() -> i32;
        fn DetourAttachEx(
            pp_pointer: *mut *mut c_void,
            p_detour: *mut c_void,
            pp_real_trampoline: *mut *mut c_void,
            pp_real_target: *mut *mut c_void,
            pp_real_detour: *mut *mut c_void,
        ) -> i32;
    }

    /// Detours keeps one pending transaction per process; ours never overlap.
    static TRANSACTION: Mutex<()> = Mutex::new(());

    pub struct Prepared {
        /// `*ppPointer` of the attach: Detours writes the trampoline into it at commit, so it
        /// lives on the heap until then.
        pointer: Box<*mut c_void>,
        tramp: usize,
        open: bool,
        _lock: MutexGuard<'static, ()>,
    }

    impl Prepared {
        /// # Safety
        /// `target` is a function entry; `detour` has exactly its signature.
        pub unsafe fn new(target: usize, detour: *const ()) -> Result<Self, String> {
            let lock = TRANSACTION.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: no transaction of ours is pending (lock held).
            let st = unsafe { DetourTransactionBegin() };
            if st != 0 {
                return Err(format!("DetourTransactionBegin {st}"));
            }
            let mut p = Prepared { pointer: Box::new(target as *mut c_void), tramp: 0, open: true, _lock: lock };
            let mut real_tramp: *mut c_void = std::ptr::null_mut();
            // SAFETY: `pointer` stays valid until commit / abort; Detours allocates the
            // trampoline here (no thread is suspended yet).
            let st = unsafe {
                DetourAttachEx(&mut *p.pointer, detour as *mut c_void, &mut real_tramp, std::ptr::null_mut(), std::ptr::null_mut())
            };
            if st != 0 || real_tramp.is_null() {
                return Err(format!("DetourAttachEx {st}")); // Drop aborts the transaction
            }
            // The trampoline code (`rbCode`) is the first field of DETOUR_TRAMPOLINE: the
            // address Detours stores into `*ppPointer` at commit on ARM64.
            p.tramp = real_tramp as usize;
            Ok(p)
        }
        pub fn trampoline(&self) -> usize {
            self.tramp
        }
        /// # Safety
        /// Patches the target prologue; the caller has the other threads suspended.
        pub unsafe fn enable(mut self) -> Result<(), String> {
            self.open = false;
            // SAFETY: our pending transaction; on error Detours aborts it itself.
            let st = unsafe { DetourTransactionCommit() };
            if st != 0 {
                return Err(format!("DetourTransactionCommit {st}"));
            }
            debug_assert_eq!(*self.pointer as usize, self.tramp);
            Ok(())
        }
    }

    impl Drop for Prepared {
        fn drop(&mut self) {
            if self.open {
                // SAFETY: our pending transaction; nothing was written to the target.
                unsafe { DetourTransactionAbort() };
            }
        }
    }
}

pub use imp::Prepared;

#[cfg(test)]
mod tests {
    //! Runs the real backend in the test process: `ntdll!NtCreateFile` (a short syscall
    //! stub, the hardest prologue we hook) gets a counting pass-through detour, with the
    //! other test threads frozen exactly as the module does it.
    use super::Prepared;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static TRAMP: AtomicUsize = AtomicUsize::new(0);

    type NtCreateFileFn = unsafe extern "system" fn(
        *mut *mut c_void,
        u32,
        *const c_void,
        *mut c_void,
        *const i64,
        u32,
        u32,
        u32,
        u32,
        *const c_void,
        u32,
    ) -> i32;

    #[allow(clippy::too_many_arguments)]
    unsafe extern "system" fn counting(
        h: *mut *mut c_void,
        access: u32,
        attrs: *const c_void,
        iosb: *mut c_void,
        size: *const i64,
        fattr: u32,
        share: u32,
        disp: u32,
        opts: u32,
        ea: *const c_void,
        ea_len: u32,
    ) -> i32 {
        CALLS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: TRAMP was published before the jump was written and has this signature.
        let next: NtCreateFileFn = unsafe { std::mem::transmute(TRAMP.load(Ordering::Acquire)) };
        // SAFETY: arguments passed through unchanged.
        unsafe { next(h, access, attrs, iosb, size, fattr, share, disp, opts, ea, ea_len) }
    }

    #[test]
    fn backend_hooks_ntcreatefile_in_this_process() {
        let target = super::super::hook::export("ntdll.dll", "NtCreateFile").expect("ntdll!NtCreateFile");
        // SAFETY: `counting` has NtCreateFile's signature (winternl.h).
        let p = unsafe { Prepared::new(target, counting as *const ()) }.expect("prepare");
        assert_ne!(p.trampoline(), 0);
        TRAMP.store(p.trampoline(), Ordering::Release);
        // SAFETY: patch with the other threads frozen, as `hook::patch` does.
        let r = super::super::threads::with_others_frozen(target, true, || unsafe { p.enable() });
        assert_eq!(r, Some(Ok(())));
        let before = CALLS.load(Ordering::Relaxed);
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        drop(std::fs::File::open(path).expect("open through the detour"));
        assert!(CALLS.load(Ordering::Relaxed) > before, "CreateFileW reached NtCreateFile through the detour");
        assert!(std::fs::read(path).is_ok_and(|b| !b.is_empty()));
    }
}
