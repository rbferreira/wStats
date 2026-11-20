//! The dropdown: one hidden webview window that is moved above whichever strip item
//! was clicked. Showing is two-step: Rust tells the page which module to render, the
//! page reports its height (`popup_size`), and only then is the window sized and shown,
//! so it never flashes at the wrong size.

use crate::config::ModuleId;
use crate::stats::Control;
use crate::strip;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::utils::config::WindowEffectsConfig;
use tauri::window::Effect;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

pub const WIDTH_CSS: f64 = 300.0;

/// Arguments for every webview of the app. They share one WebView2 environment, so
/// they must all be created with the same set. Tauri's defaults are kept; the GPU
/// process is dropped because the popups are simple enough to composite on the CPU,
/// and it costs tens of MB just by existing.
pub const BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-gpu --disable-gpu-compositing";

/// How long the popup stays loaded after closing when memory saving is on.
const UNLOAD_AFTER: Duration = Duration::from_secs(60);

/// Creates the (hidden) popup window. Must not run on the main thread outside of
/// setup: building a webview blocks until the main thread has created it.
pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let win = WebviewWindowBuilder::new(app, "popup", WebviewUrl::App("index.html".into()))
        .title("wStats")
        .inner_size(WIDTH_CSS, 480.0)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .focused(false)
        .shadow(true)
        .additional_browser_args(BROWSER_ARGS)
        .effects(WindowEffectsConfig { effects: vec![Effect::Acrylic], ..Default::default() })
        .build()?;
    // Starts hidden: ask WebView2 to keep its footprint small until first use.
    memory_target(&win, true);
    Ok(win)
}

/// Tells WebView2 how hard to hold on to memory: `Low` while the popup is hidden makes
/// it drop caches and trim its working set; `Normal` again before it is shown.
fn memory_target(win: &WebviewWindow, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use windows::core::Interface;
    let _ = win.with_webview(move |wv| unsafe {
        let Ok(core) = wv.controller().CoreWebView2() else { return };
        if let Ok(v19) = core.cast::<ICoreWebView2_19>() {
            let level = if low { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL };
            let _ = v19.SetMemoryUsageTargetLevel(level);
        }
    });
}
const GAP_DIP: f64 = 8.0;
/// A click that lands within this long after the popup closed itself on blur is the same
/// click that blurred it: treat it as "close", not "open again".
const REOPEN_GUARD: Duration = Duration::from_millis(350);

#[derive(Default)]
struct PopupState {
    module: Option<ModuleId>,
    anchor: RECT,
    visible: bool,
    pending: bool,
    height_css: f64,
    hidden_at: Option<Instant>,
    hidden_module: Option<ModuleId>,
}

pub struct Popup {
    app: AppHandle,
    control: Arc<Control>,
    strip: Arc<strip::Shared>,
    state: Mutex<PopupState>,
    save_memory: AtomicBool,
}

#[derive(Serialize, Clone)]
struct ShowPayload {
    module: ModuleId,
}

impl Popup {
    pub fn new(app: AppHandle, control: Arc<Control>, strip: Arc<strip::Shared>) -> Arc<Self> {
        Arc::new(Self {
            app,
            control,
            strip,
            state: Mutex::new(PopupState { height_css: 480.0, ..Default::default() }),
            save_memory: AtomicBool::new(false),
        })
    }

    /// With memory saving on, the webview is closed a minute after the popup is, and
    /// rebuilt on the next click; off, it is (re)created right away and kept.
    pub fn set_save_memory(self: &Arc<Self>, on: bool) {
        if self.save_memory.swap(on, Ordering::Relaxed) == on {
            return;
        }
        if on {
            let hidden_at = {
                let s = self.state.lock();
                if s.visible || s.pending {
                    return;
                }
                s.hidden_at
            };
            self.schedule_unload(hidden_at);
        } else if self.window().is_none() {
            let app = self.app.clone();
            std::thread::spawn(move || create(&app));
        }
    }

    /// The module the popup is open on, for a page that loaded after `popup:show` went
    /// out. Visible counts too: the window may already be up, still empty, waiting for it.
    pub fn pending_module(&self) -> Option<ModuleId> {
        let s = self.state.lock();
        if s.pending || s.visible { s.module } else { None }
    }

    fn schedule_unload(self: &Arc<Self>, hidden_at: Option<Instant>) {
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(UNLOAD_AFTER);
            {
                let s = me.state.lock();
                // Reopened (or closed again later) in the meantime: that close owns the timer.
                if s.visible || s.pending || s.hidden_at != hidden_at {
                    return;
                }
            }
            if me.save_memory.load(Ordering::Relaxed) {
                if let Some(win) = me.window() {
                    let _ = win.destroy();
                }
            }
        });
    }

    fn window(&self) -> Option<WebviewWindow> {
        self.app.get_webview_window("popup")
    }

    pub fn is_open(&self) -> bool {
        let s = self.state.lock();
        s.visible || s.pending
    }

    /// Strip click on `module`, whose item occupies `anchor` (screen pixels).
    pub fn toggle(self: &Arc<Self>, module: ModuleId, anchor: RECT) {
        {
            let mut s = self.state.lock();
            if s.visible && s.module == Some(module) {
                drop(s);
                self.hide();
                return;
            }
            if !s.visible
                && s.hidden_module == Some(module)
                && s.hidden_at.is_some_and(|t| t.elapsed() < REOPEN_GUARD)
            {
                return;
            }
            s.module = Some(module);
            s.anchor = anchor;
            s.pending = true;
        }
        *self.strip.active.lock() = Some(module);
        self.strip.refresh();
        self.control.detailed.store(true, Ordering::Relaxed);
        // Unloaded to save memory: rebuild it. The new page asks for the pending module
        // and reports its size once it is rendered; showing before that is an empty box.
        if self.window().is_none() {
            let app = self.app.clone();
            std::thread::spawn(move || create(&app));
            return;
        }
        let _ = self.app.emit_to("popup", "popup:show", ShowPayload { module });

        // If the page never answers (still loading), show at the last known height.
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            if me.state.lock().pending {
                me.show_now();
            }
        });
    }

    /// The page reports its content height in CSS pixels.
    pub fn on_size(&self, height_css: f64) {
        let pending = {
            let mut s = self.state.lock();
            s.height_css = height_css;
            s.pending
        };
        if pending || self.state.lock().visible {
            self.show_now();
        }
    }

    fn show_now(&self) {
        let Some(win) = self.window() else { return };
        let (anchor, height_css) = {
            let mut s = self.state.lock();
            s.pending = false;
            s.visible = true;
            (s.anchor, s.height_css)
        };
        let (x, y, w, h) = geometry(anchor, height_css);
        memory_target(&win, false);
        let _ = win.set_size(PhysicalSize::new(w, h));
        let _ = win.set_position(PhysicalPosition::new(x, y));
        if !win.is_visible().unwrap_or(false) {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }

    pub fn hide(self: &Arc<Self>) {
        let hidden_at = {
            let mut s = self.state.lock();
            if !s.visible && !s.pending {
                return;
            }
            s.visible = false;
            s.pending = false;
            s.hidden_at = Some(Instant::now());
            s.hidden_module = s.module;
            s.hidden_at
        };
        if self.save_memory.load(Ordering::Relaxed) {
            self.schedule_unload(hidden_at);
        }
        if let Some(win) = self.window() {
            let _ = win.hide();
            memory_target(&win, true);
        }
        *self.strip.active.lock() = None;
        self.control.detailed.store(false, Ordering::Relaxed);
        self.strip.refresh();
    }
}

/// Position and size in physical pixels: centred over the item, above the taskbar
/// (below it when the taskbar is at the top), kept inside the monitor's work area.
fn geometry(anchor: RECT, height_css: f64) -> (i32, i32, u32, u32) {
    let center = POINT { x: (anchor.left + anchor.right) / 2, y: (anchor.top + anchor.bottom) / 2 };
    let (work, scale) = unsafe {
        let mon = MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (info.rcWork, dx as f64 / 96.0)
    };
    place(anchor, work, scale, height_css)
}

/// `max` then `min` rather than `clamp`, which panics when a small or unknown work area
/// leaves the upper bound under the lower one.
fn place(anchor: RECT, work: RECT, scale: f64, height_css: f64) -> (i32, i32, u32, u32) {
    let center_x = (anchor.left + anchor.right) / 2;
    let gap = (GAP_DIP * scale).round() as i32;
    let height_css = if height_css.is_finite() { height_css.max(1.0) } else { 1.0 };
    let w = (WIDTH_CSS * scale).round() as i32;
    let h = ((height_css * scale).round() as i32).min(work.bottom - work.top - 2 * gap).max(1);
    let x = (center_x - w / 2).min(work.right - gap - w).max(work.left + gap);
    let y = if anchor.top >= work.bottom - gap { anchor.top - h - gap } else { anchor.bottom + gap };
    let y = y.min(work.bottom - gap - h).max(work.top + gap);
    (x, y, w as u32, h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_stays_on_screen() {
        // An item at the far right of a 2880x1800 screen with an 84px taskbar.
        let anchor = RECT { left: 2700, top: 1716, right: 2800, bottom: 1800 };
        let (x, y, w, h) = geometry(anchor, 400.0);
        assert!(x >= 0 && y >= 0 && w > 0 && h > 0);
    }

    #[test]
    fn a_work_area_too_small_or_unknown_does_not_panic() {
        let anchor = RECT { left: 100, top: 0, right: 200, bottom: 48 };
        // GetMonitorInfoW failed: all zeros.
        let (_, _, w, h) = place(anchor, RECT::default(), 1.0, 400.0);
        assert!(w > 0 && h > 0);
        // 110 % scaling and content taller than the screen.
        let work = RECT { left: 0, top: 48, right: 1280, bottom: 720 };
        let (_, y, _, h) = place(anchor, work, 1.1, 5000.0);
        assert!(y >= work.top && y + h as i32 <= work.bottom);
        place(anchor, work, 1.1, f64::NAN);
    }
}
