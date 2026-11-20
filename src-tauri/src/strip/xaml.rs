//! Native taskbar integration: wStats' element lives *inside* the taskbar's XAML tree,
//! so the taskbar lays out around it. Mirrors `tap/protocol.h`.
//!
//! wStats keeps rendering in Rust; the code in explorer.exe (tap/core.cpp) only shows
//! the pixels we publish here and posts pointer input back to the host window.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Memory::*;
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
use windows::core::{GUID, HRESULT, HSTRING, PCWSTR, s, w};

const MAPPING_NAME: &str = "Local\\wStats.Strip.v1";
const RELOAD_EVENT: &str = "Local\\wStats.TapReload.v1";
const MAGIC: u32 = 0x5054_5377;
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 4096;
const MAX_PIXELS: usize = 4096 * 512;
const MAPPING_SIZE: usize = HEADER_SIZE + MAX_PIXELS * 4;
pub const MSG_POINTER: u32 = 0x8000 + 20;
const CLSID_TAP: GUID = GUID::from_u128(0x02282dab_1933_4e15_af04_8c5a60d07fe7);

pub const POINTER_MOVE: usize = 1;
pub const POINTER_LEAVE: usize = 2;
pub const POINTER_LEFT: usize = 3;
pub const POINTER_RIGHT: usize = 4;

#[repr(C, packed(4))]
struct Header {
    magic: u32,
    version: u32,
    seq: i32,
    width: u32,
    height: u32,
    position: u32,
    host_hwnd: u64,
    core_path: [u16; 260],
    heartbeat: i32,
    tap_pid: u32,
    elem_x: i32,
    elem_y: i32,
    attached: u32,
    // Appended in the same version: which monitor's taskbar hosts the element.
    mon_left: i32,
    mon_top: i32,
    mon_right: i32,
    mon_bottom: i32,
    all_monitors: u32,
    render_scale_1000: u32,
    click_scale_1000: u32,
    core_loads: i32,
}

const _: () = assert!(size_of::<Header>() <= HEADER_SIZE);

/// An attach asked of Explorer and not answered yet.
pub struct Attach {
    /// Core loads counted before asking: a higher count is the new core.
    loads: i32,
    until: Instant,
    /// Asked of a stub that was already there (a reload), not injected.
    reloaded: bool,
}

pub struct Surface {
    map: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    reload: HANDLE,
    seq: i32,
    /// Whether the frame Explorer has is the empty one.
    empty: bool,
}

// The mapping is only touched from the strip thread.
unsafe impl Send for Surface {}

/// Raw pointer to a header field (the header is packed and shared; use unaligned access).
/// Must be used inside `unsafe`.
macro_rules! field {
    ($self:ident . $f:ident) => {
        std::ptr::addr_of_mut!((*$self.header()).$f)
    };
}

impl Surface {
    pub fn create(host: HWND, left: bool) -> windows::core::Result<Self> {
        unsafe {
            let map = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, MAPPING_SIZE as u32, &HSTRING::from(MAPPING_NAME))?;
            let view = MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_SIZE);
            if view.Value.is_null() {
                let _ = CloseHandle(map);
                return Err(windows::core::Error::from_thread());
            }
            let reload = CreateEventW(None, false, false, &HSTRING::from(RELOAD_EVENT))?;
            let me = Self { map, view, reload, seq: 0, empty: false };
            // An existing mapping (wStats restarted, TAP still in Explorer) keeps its
            // TAP-written fields; only ours are (re)initialised.
            me.seq_init();
            field!(me.magic).write_unaligned(MAGIC);
            field!(me.version).write_unaligned(VERSION);
            field!(me.host_hwnd).write_unaligned(host.0 as u64);
            me.set_position(left);
            Ok(me)
        }
    }

    fn header(&self) -> *mut Header {
        self.view.Value.cast()
    }

    fn seq_init(&self) {
        let cur = unsafe { field!(self.seq).read_unaligned() };
        // Keep the sequence monotonic across restarts so the TAP sees our first frame as new.
        unsafe { field!(self.seq).write_unaligned(cur.wrapping_add(2) & !1) };
    }

    pub fn set_position(&self, left: bool) {
        unsafe { field!(self.position).write_unaligned(if left { 0 } else { 1 }) };
    }

    /// Publishes a frame (BGRA, premultiplied, top-down) with a seqlock.
    pub fn publish(&mut self, pixels: &[u8], width: u32, height: u32) {
        let n = width as usize * height as usize;
        if n > MAX_PIXELS || pixels.len() < n * 4 {
            return;
        }
        self.empty = n == 0;
        unsafe {
            let seq = field!(self.seq).read_unaligned();
            let seq = if seq & 1 == 1 { seq.wrapping_add(1) } else { seq };
            field!(self.seq).write_unaligned(seq.wrapping_add(1));
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            field!(self.width).write_unaligned(width);
            field!(self.height).write_unaligned(height);
            let dst = (self.view.Value as *mut u8).add(HEADER_SIZE);
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), dst, n * 4);
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            field!(self.seq).write_unaligned(seq.wrapping_add(2));
            self.seq = seq.wrapping_add(2);
        }
    }

    /// Publishes a frame of no size: the strip has nothing to show.
    pub fn clear(&mut self) {
        if !self.empty {
            self.publish(&[], 0, 0);
        }
    }

    /// The monitor whose taskbar should host the element.
    pub fn set_monitor(&self, r: windows::Win32::Foundation::RECT) {
        unsafe {
            field!(self.mon_left).write_unaligned(r.left);
            field!(self.mon_top).write_unaligned(r.top);
            field!(self.mon_right).write_unaligned(r.right);
            field!(self.mon_bottom).write_unaligned(r.bottom);
        }
    }

    /// Show the element on every taskbar.
    pub fn set_all_monitors(&self, all: bool) {
        unsafe { field!(self.all_monitors).write_unaligned(all as u32) };
    }

    /// The DPI scale the pixels are rendered at.
    pub fn set_render_scale(&self, scale: f32) {
        unsafe { field!(self.render_scale_1000).write_unaligned((scale * 1000.0).round() as u32) };
    }

    /// DPI scale of the taskbar last clicked (1.0 until the first click).
    pub fn click_scale(&self) -> f32 {
        let v = unsafe { field!(self.click_scale_1000).read_unaligned() };
        if v == 0 { 1.0 } else { v as f32 / 1000.0 }
    }

    /// How many times a core has been loaded in Explorer.
    pub fn core_loads(&self) -> i32 {
        unsafe { field!(self.core_loads).read_unaligned() }
    }

    /// Whether the attach has an answer yet: `Some(true)` once a core loaded after it was
    /// asked for has put the element on a taskbar, `Some(false)` when Explorer took too
    /// long, `None` while there is still time. Never waits: the strip's thread has
    /// windows to serve, and a silent Explorer used to freeze them for ten seconds.
    pub fn poll_attach(&self, a: &Attach) -> Option<bool> {
        if self.core_loads() != a.loads && self.attached() > 0 {
            if a.reloaded {
                log::info!("xaml: core reloaded in Explorer");
            }
            return Some(true);
        }
        if Instant::now() < a.until {
            return None;
        }
        if a.reloaded {
            unsafe {
                let _ = ResetEvent(self.reload);
            }
            log::warn!("xaml: the stub in Explorer did not confirm the reload (loads {} -> {}, attached {})", a.loads, self.core_loads(), self.attached());
        }
        Some(false)
    }

    pub fn heartbeat(&self) -> i32 {
        unsafe { field!(self.heartbeat).read_unaligned() }
    }

    pub fn attached(&self) -> u32 {
        unsafe { field!(self.attached).read_unaligned() }
    }

    /// Screen position of the element's top-left, as last reported by a click.
    pub fn element_origin(&self) -> (i32, i32) {
        unsafe { (field!(self.elem_x).read_unaligned(), field!(self.elem_y).read_unaligned()) }
    }

    /// Gets our code running inside the taskbar's Explorer: reloads the core when the stub
    /// is already there, injects the stub otherwise. The element shows up a moment later;
    /// `poll_attach` tells when.
    pub fn begin_attach(&self) -> Result<Attach, String> {
        let (stub, core) = install_dlls()?;
        let wide: Vec<u16> = core.as_os_str().encode_wide_lossy();
        unsafe {
            let dst = field!(self.core_path) as *mut u16;
            std::ptr::write_bytes(dst, 0, 260);
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len().min(259));
        }
        let pid = explorer_pid()?;
        if stub_loaded(pid) {
            // A stub left behind by an earlier wStats may be idle (its core stopped with
            // that process) but it still serves the reload event. Never inject a second
            // one: each injection is another watcher in Explorer, and the copies pile up.
            // The reload detaches the old core on every taskbar's thread first, so it can
            // take a few seconds.
            let loads = self.core_loads();
            unsafe { SetEvent(self.reload).map_err(|e| e.to_string())? };
            log::info!("xaml: asked the stub in Explorer to load {}", core.display());
            return Ok(Attach { loads, until: Instant::now() + Duration::from_secs(10), reloaded: true });
        }
        log::info!("xaml: injecting the stub into Explorer ({pid})");
        let loads = self.core_loads();
        inject(&stub, pid)?;
        Ok(Attach { loads, until: Instant::now() + Duration::from_secs(5), reloaded: false })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            // A core that loads late, or is still there, holds the mapping open and would
            // go on showing the last frame beside the fallback window. No host window is
            // its sign that wStats left: it puts the taskbar back.
            field!(self.host_hwnd).write_unaligned(0);
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.map);
            let _ = CloseHandle(self.reload);
        }
    }
}

trait EncodeWide {
    fn encode_wide_lossy(&self) -> Vec<u16>;
}

impl EncodeWide for std::ffi::OsStr {
    fn encode_wide_lossy(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().collect()
    }
}

/// Modification time of the core DLL wStats would install, so debug builds can hot-reload
/// the code inside Explorer whenever `tap/build.cmd` produces a new one.
pub fn core_source_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(dll_source_dir()?.join("wStatsCore.dll")).ok()?.modified().ok()
}

/// Where the built DLLs are: next to the executable when installed, `tap/out` in a
/// development checkout.
fn dll_source_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [exe_dir.clone(), exe_dir.join("tap"), PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tap/out")]
        .into_iter()
        .find(|d| d.join("wStatsTAP.dll").exists() && d.join("wStatsCore.dll").exists())
}

/// Copies the DLLs to %LOCALAPPDATA%\wStats\tap. Explorer keeps a loaded DLL locked, so
/// the core gets a content-addressed name: a new build loads alongside the old one
/// instead of failing to overwrite it.
fn install_dlls() -> Result<(PathBuf, PathBuf), String> {
    let src = dll_source_dir().ok_or("wStatsTAP.dll / wStatsCore.dll not found")?;
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).ok_or("no LOCALAPPDATA")?;
    let dir = base.join("wStats").join("tap");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let stub = dir.join("wStatsTAP.dll");
    let stub_src = std::fs::read(src.join("wStatsTAP.dll")).map_err(|e| e.to_string())?;
    if std::fs::read(&stub).ok().as_deref() != Some(stub_src.as_slice()) {
        // Fails harmlessly when an older stub is loaded; the stub's interface is stable.
        let _ = std::fs::write(&stub, &stub_src);
    }

    let core_src = std::fs::read(src.join("wStatsCore.dll")).map_err(|e| e.to_string())?;
    let core = dir.join(format!("wStatsCore-{:016x}.dll", fnv1a(&core_src)));
    if !core.exists() {
        std::fs::write(&core, &core_src).map_err(|e| e.to_string())?;
    }
    cleanup_old_cores(&dir, &core);
    Ok((stub, core))
}

fn cleanup_old_cores(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with("wStatsCore-") && p != keep {
            let _ = std::fs::remove_file(&p); // still-loaded ones stay locked; fine
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3))
}

type InitializeXamlDiagnosticsEx = unsafe extern "system" fn(PCWSTR, u32, PCWSTR, PCWSTR, GUID, PCWSTR) -> HRESULT;

/// The Explorer process that owns the taskbar.
fn explorer_pid() -> Result<u32, String> {
    unsafe {
        let taskbar = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).map_err(|e| e.to_string())?;
        let mut pid = 0u32;
        GetWindowThreadProcessId(taskbar, Some(&mut pid));
        if pid == 0 { Err("no taskbar process".into()) } else { Ok(pid) }
    }
}

/// Whether the stub is already loaded in Explorer (it never unloads).
fn stub_loaded(pid: u32) -> bool {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::ProcessStatus::{EnumProcessModulesEx, GetModuleBaseNameW, LIST_MODULES_ALL};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) else { return false };
        let mut modules = vec![HMODULE::default(); 2048];
        let mut needed = 0u32;
        let size = (modules.len() * size_of::<HMODULE>()) as u32;
        let ok = EnumProcessModulesEx(process, modules.as_mut_ptr(), size, &mut needed, LIST_MODULES_ALL).is_ok();
        let n = (needed as usize / size_of::<HMODULE>()).min(modules.len());
        let found = ok
            && modules[..n].iter().any(|&m| {
                let mut name = [0u16; 64];
                let len = GetModuleBaseNameW(process, Some(m), &mut name) as usize;
                String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("wStatsTAP.dll")
            });
        let _ = CloseHandle(process);
        found
    }
}

fn inject(stub: &Path, pid: u32) -> Result<(), String> {
    unsafe {
        let xaml = LoadLibraryExW(w!("Windows.UI.Xaml.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).map_err(|e| e.to_string())?;
        let init: InitializeXamlDiagnosticsEx =
            std::mem::transmute(GetProcAddress(xaml, s!("InitializeXamlDiagnosticsEx")).ok_or("no InitializeXamlDiagnosticsEx")?);
        let stub = HSTRING::from(stub.as_os_str());
        let mut last = HRESULT(0);
        // Each attempt needs an unused endpoint name; ERROR_NOT_FOUND means "taken".
        for i in 1..=100 {
            let endpoint = HSTRING::from(format!("VisualDiagConnection{i}"));
            last = init(PCWSTR(endpoint.as_ptr()), pid, w!(""), PCWSTR(stub.as_ptr()), CLSID_TAP, w!(""));
            if last != HRESULT::from_win32(1168) {
                break;
            }
        }
        last.ok().map_err(|e| format!("InitializeXamlDiagnosticsEx: {e}"))
    }
}
