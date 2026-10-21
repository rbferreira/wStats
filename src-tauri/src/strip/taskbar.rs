//! Where the strip can go. On Windows 11 the taskbar buttons are XAML: `ReBarWindow32`
//! and `MSTaskSwWClass` no longer describe where the app icons end, and `TrayNotifyWnd`
//! does not include the "show hidden icons" chevron. UI Automation does expose the real
//! buttons, so we ask it (~20 ms, every second).

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::Accessibility::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Obstacle {
    pub left: i32,
    pub right: i32,
    /// The Widgets button: the strip may sit right after it (left placement) or hop to
    /// its left (it sits next to the tray when icons are left-aligned).
    pub hop: bool,
}

/// The free gap on the left of the bar: after the Widgets button (if it is on the left)
/// and before the first thing to its right (Start, when icons are centred).
pub fn fit_left(obstacles: &[Obstacle], bar_left: i32, bar_right: i32, tray_left: i32, margin: i32) -> (i32, i32) {
    let mid = (bar_left + bar_right) / 2;
    let start = obstacles.iter().filter(|o| o.hop && o.left < mid).map(|o| o.right).max().unwrap_or(bar_left);
    let limit = obstacles.iter().filter(|o| !o.hop && o.left >= start).map(|o| o.left).min().unwrap_or(tray_left);
    (start + margin, (limit - start - 2 * margin).max(0))
}

#[derive(Debug, Default, Clone)]
pub struct Scan {
    /// Left edge of the notification area, chevron included.
    pub tray_left: Option<i32>,
    pub obstacles: Vec<Obstacle>,
}

/// Scans on its own thread. UI Automation sends messages to the taskbar's child windows,
/// the strip window among them, so scanning from the thread that owns that window
/// deadlocks: it would be waiting on itself. One thread scans every taskbar in turn:
/// with one thread per taskbar, some of them never got a scan back.
pub struct Scanner {
    /// Taskbars to scan (as integers: HWND is not `Send`) and their rectangles.
    targets: Arc<Mutex<Vec<(isize, RECT)>>>,
    latest: Arc<Mutex<HashMap<isize, Scan>>>,
}

impl Scanner {
    pub fn new() -> Self {
        let targets = Arc::new(Mutex::new(Vec::new()));
        let latest = Arc::new(Mutex::new(HashMap::new()));
        let (weak_targets, weak_latest) = (Arc::downgrade(&targets), Arc::downgrade(&latest));
        let _ = std::thread::Builder::new().name("uia".into()).spawn(move || worker(weak_targets, weak_latest));
        Self { targets, latest }
    }

    /// The most recent scan of `taskbar`, or `None` until the first one is in. Never
    /// blocks; asking for a new taskbar adds it to the next scans.
    pub fn scan(&self, taskbar: HWND, taskbar_rect: RECT) -> Option<Scan> {
        let key = taskbar.0 as isize;
        let mut targets = self.targets.lock();
        match targets.iter_mut().find(|t| t.0 == key) {
            Some(t) => t.1 = taskbar_rect,
            None => targets.push((key, taskbar_rect)),
        }
        drop(targets);
        self.latest.lock().get(&key).cloned()
    }

    /// Stops scanning the taskbars not in `keep`.
    pub fn retain(&self, keep: &[HWND]) {
        let keep: Vec<isize> = keep.iter().map(|h| h.0 as isize).collect();
        self.targets.lock().retain(|t| keep.contains(&t.0));
        self.latest.lock().retain(|k, _| keep.contains(k));
    }
}

/// Scans every second until the [`Scanner`] is dropped.
fn worker(targets: Weak<Mutex<Vec<(isize, RECT)>>>, latest: Weak<Mutex<HashMap<isize, Scan>>>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Ok(uia) = Uia::new() else { return };
    loop {
        let Some(list) = targets.upgrade().map(|t| t.lock().clone()) else { return };
        for (hwnd, rect) in list {
            let scan = uia.scan(HWND(hwnd as _), rect);
            let Some(latest) = latest.upgrade() else { return };
            // Skip the result if the taskbar was dropped while scanning.
            if targets.upgrade().is_some_and(|t| t.lock().iter().any(|t| t.0 == hwnd)) {
                match scan {
                    Some(scan) => latest.lock().insert(hwnd, scan),
                    None => latest.lock().remove(&hwnd),
                };
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

struct Uia {
    uia: IUIAutomation,
    cache: IUIAutomationCacheRequest,
}

impl Uia {
    /// Needs COM initialised on the calling thread.
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let cache = uia.CreateCacheRequest()?;
            cache.AddProperty(UIA_ClassNamePropertyId)?;
            cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
            cache.AddProperty(UIA_AutomationIdPropertyId)?;
            Ok(Self { uia, cache })
        }
    }

    fn scan(&self, taskbar: HWND, taskbar_rect: RECT) -> Option<Scan> {
        unsafe {
            let root = self.uia.ElementFromHandle(taskbar).ok()?;
            let all = root.FindAllBuildCache(TreeScope_Descendants, &self.uia.CreateTrueCondition().ok()?, &self.cache).ok()?;
            let width = taskbar_rect.right - taskbar_rect.left;
            let mut scan = Scan::default();
            for i in 0..all.Length().ok()? {
                let Ok(el) = all.GetElement(i) else { continue };
                let class = el.CachedClassName().map(|s| s.to_string()).unwrap_or_default();
                let Ok(r) = el.CachedBoundingRectangle() else { continue };
                // Containers span the whole bar; zero-size elements are collapsed.
                if r.right <= r.left || r.right - r.left > width / 2 || class.starts_with("wStats") {
                    continue;
                }
                if class.starts_with("SystemTray.") {
                    scan.tray_left = Some(scan.tray_left.map_or(r.left, |t: i32| t.min(r.left)));
                } else {
                    let id = el.CachedAutomationId().map(|s| s.to_string()).unwrap_or_default();
                    scan.obstacles.push(Obstacle { left: r.left, right: r.right, hop: id.contains("Widgets") });
                }
            }
            Some(scan)
        }
    }
}

/// Picks the strip's right edge: against the tray, unless something hoppable sits there
/// with too little room, in which case it moves to that thing's left. Returns
/// `(right_edge, room)`, room being the free width left of `right_edge`.
pub fn fit(tray_left: i32, obstacles: &[Obstacle], bar_left: i32, needed: i32, margin: i32) -> (i32, i32) {
    let mut anchor = tray_left;
    loop {
        let blocker = obstacles.iter().filter(|o| o.right <= anchor).max_by_key(|o| o.right);
        let free_from = blocker.map_or(bar_left, |b| b.right);
        let room = anchor - free_from - 2 * margin;
        match blocker {
            Some(b) if room < needed && b.hop => anchor = b.left,
            _ => return (anchor - margin, room.max(0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ob(left: i32, right: i32, hop: bool) -> Obstacle {
        Obstacle { left, right, hop }
    }

    #[test]
    fn sits_between_last_app_and_tray() {
        let apps = [ob(746, 825, false), ob(2057, 2134, false)];
        assert_eq!(fit(2374, &apps, 0, 200, 4), (2370, 2374 - 2134 - 8));
    }

    #[test]
    fn hops_over_widgets_next_to_the_tray() {
        let items = [ob(0, 900, false), ob(2100, 2366, true)];
        let (right, room) = fit(2374, &items, 0, 400, 4);
        assert_eq!(right, 2096);
        assert_eq!(room, 2100 - 900 - 8);
    }

    #[test]
    fn stays_put_when_widgets_leave_enough_room() {
        let items = [ob(2100, 2366, true)];
        assert_eq!(fit(2400, &items, 0, 20, 4), (2396, 2400 - 2366 - 8));
    }

    #[test]
    fn left_gap_runs_from_widgets_to_start() {
        // The layout measured on the dev machine: widgets, Start, task view, apps.
        let items = [ob(11, 277, true), ob(746, 825, false), ob(825, 902, false), ob(902, 979, false)];
        assert_eq!(fit_left(&items, 0, 2880, 2374, 4), (281, 746 - 277 - 8));
    }

    #[test]
    fn left_gap_without_widgets_starts_at_the_edge() {
        let items = [ob(746, 825, false)];
        assert_eq!(fit_left(&items, 0, 2880, 2374, 4), (4, 746 - 8));
    }

    #[test]
    fn left_aligned_icons_leave_no_left_gap() {
        let items = [ob(0, 79, false), ob(79, 156, false)];
        assert_eq!(fit_left(&items, 0, 2880, 2374, 4).1, 0);
    }

    #[test]
    fn empty_taskbar_uses_everything_left_of_the_tray() {
        assert_eq!(fit(1000, &[], 0, 50, 4), (996, 992));
    }
}
