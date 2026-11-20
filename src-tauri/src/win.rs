//! Small Win32 helpers shared across modules.

use windows::Win32::System::Registry::*;
use windows::core::HSTRING;

pub fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

pub fn reg_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    rc.is_ok().then(|| wide_to_string(&buf))
}

pub fn reg_dword(root: HKEY, subkey: &str, value: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    let rc = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(subkey),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut v as *mut u32).cast()),
            Some(&mut size),
        )
    };
    rc.is_ok().then_some(v)
}

/// Records that the user turned "Start with Windows" off, where the installer looks
/// before writing the entry again on an update. Turning it on removes the record.
pub fn remember_autostart(enabled: bool) {
    let (key, value) = (HSTRING::from(r"Software\wStats"), HSTRING::from("Autostart"));
    unsafe {
        if enabled {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &value);
        } else {
            let off = 0u32;
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, &key, &value, REG_DWORD.0, Some((&off as *const u32).cast()), 4);
        }
    }
}

/// Whether the taskbar uses the light theme (`SystemUsesLightTheme`).
pub fn taskbar_is_light() -> bool {
    reg_dword(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "SystemUsesLightTheme",
    ) == Some(1)
}

/// Minimal file logger: `%LOCALAPPDATA%\wStats\wstats.log`, truncated at startup.
pub struct FileLog(std::sync::Mutex<std::fs::File>);

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info || m.target().starts_with("wstats")
    }

    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            use std::io::Write;
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            let _ = writeln!(self.0.lock().unwrap(), "{t:.3} {:5} {}: {}", r.level(), r.target(), r.args());
        }
    }

    fn flush(&self) {}
}

pub fn init_log() {
    let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("wStats")) else { return };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(f) = std::fs::File::create(dir.join("wstats.log")) {
        if log::set_boxed_logger(Box::new(FileLog(std::sync::Mutex::new(f)))).is_ok() {
            log::set_max_level(log::LevelFilter::Debug);
        }
    }
}

/// Brings `hwnd` to the front even though wStats is a background app. Windows refuses
/// `SetForegroundWindow` to a process that does not own the foreground, so we briefly
/// share input state with the window that does, which lifts that restriction.
pub fn force_foreground(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };
    unsafe {
        let _ = ShowWindow(hwnd, if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
        let me = GetCurrentThreadId();
        let front = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let attached = front != 0 && front != me && AttachThreadInput(me, front, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(me, front, false);
        }
    }
}
