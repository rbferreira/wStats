//! PawnIO: a signed kernel driver that runs small signed modules (namazso/PawnIO). We
//! use it through PawnIOLib.dll from its install directory, so nothing of ours runs in
//! the kernel. The modules we load are embedded below (LGPL-2.1, see pawnio/COPYING).

use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW};
use windows::core::{HRESULT, HSTRING, s};

pub const INTEL_MSR: &[u8] = include_bytes!("../pawnio/IntelMSR.bin");
pub const AMD_FAMILY_17: &[u8] = include_bytes!("../pawnio/AMDFamily17.bin");
pub const LPC_IO: &[u8] = include_bytes!("../pawnio/LpcIO.bin");

type OpenFn = unsafe extern "system" fn(*mut HANDLE) -> HRESULT;
type LoadFn = unsafe extern "system" fn(HANDLE, *const u8, usize) -> HRESULT;
type ExecuteFn = unsafe extern "system" fn(HANDLE, *const u8, *const u64, usize, *mut u64, usize, *mut usize) -> HRESULT;
type CloseFn = unsafe extern "system" fn(HANDLE) -> HRESULT;

/// PawnIOLib.dll, if PawnIO is installed. The DLL stays loaded for the process' life.
#[derive(Clone, Copy)]
pub struct Lib {
    open: OpenFn,
    load: LoadFn,
    execute: ExecuteFn,
    close: CloseFn,
}

impl Lib {
    pub fn load() -> Result<Self, String> {
        let dir = std::env::var_os("ProgramFiles").map(std::path::PathBuf::from).ok_or("no ProgramFiles")?;
        let path = dir.join("PawnIO").join("PawnIOLib.dll");
        if !path.exists() {
            return Err("PawnIO is not installed".into());
        }
        unsafe {
            let lib: HMODULE = LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
                .map_err(|e| format!("PawnIOLib.dll: {e}"))?;
            let get = |name: windows::core::PCSTR| GetProcAddress(lib, name).ok_or_else(|| "PawnIOLib.dll: missing export".to_string());
            Ok(Self {
                open: std::mem::transmute::<unsafe extern "system" fn() -> isize, OpenFn>(get(s!("pawnio_open"))?),
                load: std::mem::transmute::<unsafe extern "system" fn() -> isize, LoadFn>(get(s!("pawnio_load"))?),
                execute: std::mem::transmute::<unsafe extern "system" fn() -> isize, ExecuteFn>(get(s!("pawnio_execute"))?),
                close: std::mem::transmute::<unsafe extern "system" fn() -> isize, CloseFn>(get(s!("pawnio_close"))?),
            })
        }
    }

    /// Opens an executor and loads `blob` into it. Fails when the module refuses this
    /// machine (wrong vendor or family): that is its own safety check.
    pub fn module(&self, blob: &[u8]) -> Result<Module, String> {
        unsafe {
            let mut h = HANDLE::default();
            (self.open)(&mut h).ok().map_err(|e| format!("pawnio_open: {e}"))?;
            if let Err(e) = (self.load)(h, blob.as_ptr(), blob.len()).ok() {
                let _ = (self.close)(h);
                return Err(format!("pawnio_load: {e}"));
            }
            Ok(Module { lib: *self, handle: h })
        }
    }
}

pub struct Module {
    lib: Lib,
    handle: HANDLE,
}

// The executor handle is used from one thread at a time.
unsafe impl Send for Module {}

impl Module {
    /// Calls `function` (a NUL-terminated name) with `input`, returning `N` outputs.
    pub fn call<const N: usize>(&self, function: &[u8], input: &[u64]) -> Result<[u64; N], String> {
        debug_assert!(function.ends_with(b"\0"));
        let mut out = [0u64; N];
        let mut got = 0usize;
        unsafe {
            (self.lib.execute)(self.handle, function.as_ptr(), input.as_ptr(), input.len(), out.as_mut_ptr(), N, &mut got)
                .ok()
                .map_err(|e| format!("{}: {e}", String::from_utf8_lossy(&function[..function.len() - 1])))?;
        }
        Ok(out)
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.lib.close)(self.handle);
        }
    }
}
