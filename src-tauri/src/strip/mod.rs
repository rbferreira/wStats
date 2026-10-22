//! The taskbar strip.
//!
//! Preferred mode, **XAML**: our element lives inside the taskbar's own XAML tree (see
//! `xaml.rs` and `tap/`), so the taskbar lays its icons out around it, exactly as it does
//! for the Widgets button. We render here and publish pixels; Explorer shows them.
//!
//! Fallback mode, **window**: a layered child window of the taskbar (one per taskbar with
//! "all monitors"), placed in the free gap found through UI Automation (`taskbar.rs`). Used only when the XAML route is
//! unavailable (e.g. ExplorerPatcher's Windows 10 taskbar).
//!
//! Everything runs on one thread with a hidden top-level *host* window, which receives
//! broadcasts (`TaskbarCreated`, theme/DPI changes), the refresh timer and, in XAML mode,
//! the pointer messages posted from Explorer.

mod icons;
mod render;

pub use render::{Rgba, preview_png};
mod taskbar;
mod xaml;

use crate::config::{Look, ModuleId, Position};
use crate::stats::Shared as StoreHandle;
use parking_lot::{Mutex, RwLock};
use render::{Frame, Renderer, Row, Slot};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const WM_APP_REFRESH: u32 = WM_APP + 1;
/// `Settings::monitor` value meaning "every monitor".
pub const ALL_MONITORS: &str = "*";
const TIMER_PLACE: usize = 1;
/// After an Explorer restart its XAML needs a moment before it accepts the TAP.
const REATTACH_EVERY: Duration = Duration::from_secs(3);
const MAX_ATTACH_FAILURES: u32 = 5;
/// While on the fallback window, how often to try the taskbar's XAML again: starts at
/// the first, doubles after each miss up to the second.
const RETRY_XAML: (Duration, Duration) = (Duration::from_secs(60), Duration::from_secs(15 * 60));

/// What the strip tells the rest of the app.
pub enum Event {
    /// An item was clicked; `rect` is the item in screen pixels.
    Click { module: ModuleId, rect: RECT },
    /// Right click, at screen coordinates.
    Menu { x: i32, y: i32 },
}

/// State the app shares with the strip thread.
pub struct Shared {
    host: AtomicIsize,
    /// Module whose popup is open: drawn with a highlighted pill.
    pub active: Mutex<Option<ModuleId>>,
    pub modules: RwLock<Vec<ModuleId>>,
    pub position: RwLock<Position>,
    /// Monitor (GDI device name) whose taskbar hosts the strip; `None` = primary,
    /// [`ALL_MONITORS`] = every taskbar.
    pub monitor: RwLock<Option<String>>,
    /// Chart colours (primary, secondary).
    pub colors: RwLock<(Rgba, Rgba)>,
    /// How the items are drawn (label, graphics, compact).
    pub look: RwLock<Look>,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            host: AtomicIsize::new(0),
            active: Mutex::new(None),
            modules: RwLock::new(Vec::new()),
            position: RwLock::new(Position::Left),
            monitor: RwLock::new(None),
            colors: RwLock::new((Rgba::BLUE, Rgba::GREEN)),
            look: RwLock::new(Look::default()),
        }
    }
}

impl Shared {
    /// Asks the strip to repaint (new sample, popup opened/closed, settings changed).
    pub fn refresh(&self) {
        let host = self.host.load(Ordering::Acquire);
        if host != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(host as _)), WM_APP_REFRESH, WPARAM(0), LPARAM(0));
            }
        }
    }
}

enum Mode {
    Xaml { surface: xaml::Surface, last_beat: i32, beat_at: Instant, failures: u32, core_mtime: Option<std::time::SystemTime> },
    /// One window per taskbar shown on; the scanner finds the free gap on each.
    Window { bars: Vec<Bar>, scanner: Option<taskbar::Scanner> },
}

/// The fallback window on one taskbar. Each has its own layout and renderer: the
/// taskbars can be on monitors with different DPIs.
struct Bar {
    taskbar: HWND,
    hwnd: HWND,
    /// A real child of the taskbar (hides with it); otherwise a topmost popup.
    embedded: bool,
    renderer: Renderer,
    slots: Vec<Slot>,
    width: i32,
    height: i32,
    scale: f32,
    hover: Option<usize>,
    tracking: bool,
}

impl Drop for Bar {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

/// What every frame shares, whichever taskbar it is drawn for.
#[derive(Clone, Copy)]
struct Style {
    light: bool,
    active: Option<ModuleId>,
    primary: Rgba,
    secondary: Rgba,
    look: Look,
}

impl Style {
    fn frame(self, slots: &[Slot], width: i32, height: i32, scale: f32, hover: Option<usize>) -> Frame {
        Frame {
            slots: slots.to_vec(),
            width,
            height,
            scale,
            light: self.light,
            hover,
            active: self.active,
            primary: self.primary,
            secondary: self.secondary,
            look: self.look,
        }
    }
}

struct State {
    store: StoreHandle,
    shared: Arc<Shared>,
    on_event: Box<dyn Fn(Event)>,
    renderer: Renderer,
    host: HWND,
    mode: Mode,
    taskbar: HWND,
    slots: Vec<Slot>,
    width: i32,
    height: i32,
    scale: f32,
    light: bool,
    hover: Option<usize>,
    xaml_retry_at: Instant,
    xaml_retry_every: Duration,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub fn spawn(store: StoreHandle, shared: Arc<Shared>, on_event: impl Fn(Event) + Send + 'static) {
    std::thread::Builder::new()
        .name("strip".into())
        .spawn(move || {
            if let Err(e) = run(store, shared, Box::new(on_event)) {
                log::error!("strip thread failed: {e:?}");
            }
        })
        .expect("spawn strip thread");
}

fn run(store: StoreHandle, shared: Arc<Shared>, on_event: Box<dyn Fn(Event)>) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?.into();
        let classes: [(PCWSTR, WNDPROC); 2] = [(w!("wStatsHost"), Some(host_proc)), (w!("wStatsStrip"), Some(strip_proc))];
        for (name, proc) in classes {
            let class = WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: instance,
                lpszClassName: name,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: HBRUSH::default(),
                ..Default::default()
            };
            RegisterClassW(&class);
        }
        let host = CreateWindowExW(WS_EX_TOOLWINDOW, w!("wStatsHost"), w!("wStats"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance), None)?;

        let mut state = State {
            store,
            shared: shared.clone(),
            on_event,
            renderer: Renderer::new()?,
            host,
            mode: Mode::Window { bars: Vec::new(), scanner: None },
            taskbar: HWND::default(),
            slots: Vec::new(),
            width: 0,
            height: 0,
            scale: 1.0,
            light: crate::win::taskbar_is_light(),
            hover: None,
            xaml_retry_at: Instant::now(),
            xaml_retry_every: RETRY_XAML.0,
        };
        state.start();
        STATE.with(|s| *s.borrow_mut() = Some(state));
        shared.host.store(host.0 as isize, Ordering::Release);
        SetTimer(Some(host), TIMER_PLACE, 1000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// Runs `f` on the thread's state unless it is already borrowed (re-entrant message).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn find(parent: Option<HWND>, class: PCWSTR) -> Option<HWND> {
    unsafe { FindWindowExW(parent, None, class, PCWSTR::null()).ok() }.filter(|h| !h.is_invalid())
}

/// Every taskbar window: the primary monitor's is `Shell_TrayWnd`, every other monitor
/// has a `Shell_SecondaryTrayWnd`.
fn taskbars() -> Vec<HWND> {
    let mut all: Vec<HWND> = find(None, w!("Shell_TrayWnd")).into_iter().collect();
    let mut after: Option<HWND> = None;
    while let Ok(h) = unsafe { FindWindowExW(None, after, w!("Shell_SecondaryTrayWnd"), PCWSTR::null()) } {
        if h.is_invalid() {
            break;
        }
        all.push(h);
        after = Some(h);
    }
    all
}

/// The taskbar window on the monitor covering `monitor`.
fn taskbar_on(monitor: RECT) -> Option<HWND> {
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect, MonitorFromWindow};
    let target = unsafe { MonitorFromRect(&monitor, MONITOR_DEFAULTTONULL) };
    taskbars().into_iter().find(|h| unsafe { MonitorFromWindow(*h, MONITOR_DEFAULTTONULL) } == target)
}

/// The slot under (x, y): compact columns hold two, one over the other.
fn slot_at(slots: &[Slot], height: i32, x: i32, y: i32) -> Option<usize> {
    let top = y < height / 2;
    slots.iter().position(|s| {
        x >= s.x
            && x < s.x + s.w
            && match s.row {
                Row::Full => true,
                Row::Top => top,
                Row::Bottom => !top,
            }
    })
}

fn bar_of(mode: &mut Mode, hwnd: HWND) -> Option<&mut Bar> {
    let Mode::Window { bars, .. } = mode else { return None };
    bars.iter_mut().find(|b| b.hwnd == hwnd)
}

fn rect_of(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(h, &mut r);
    }
    r
}

impl State {
    /// Picks the mode: XAML when Explorer accepts our element, the window otherwise.
    fn start(&mut self) {
        self.measure();
        // WSTATS_STRIP=window skips the XAML route, to exercise the fallback.
        let forced = std::env::var("WSTATS_STRIP").is_ok_and(|v| v == "window");
        if forced || !self.try_xaml() {
            self.fallback_to_window();
        }
    }

    /// Switches to XAML mode and attaches; false (mode left as XAML) when it fails.
    fn try_xaml(&mut self) -> bool {
        let left = *self.shared.position.read() == Position::Left;
        match xaml::Surface::create(self.host, left) {
            Ok(surface) => {
                self.mode = Mode::Xaml { surface, last_beat: 0, beat_at: Instant::now(), failures: 0, core_mtime: xaml::core_source_mtime() };
                self.layout();
                self.paint();
                self.attach_xaml()
            }
            Err(e) => {
                log::warn!("shared memory: {e:?}");
                false
            }
        }
    }

    /// On the fallback window: Explorer may accept the element now (it was restarted, or
    /// a stuck reload cleared). Keeps the window unless the switch works.
    fn retry_xaml(&mut self) {
        if !matches!(self.mode, Mode::Window { .. }) {
            return;
        }
        let window = std::mem::replace(&mut self.mode, Mode::Window { bars: Vec::new(), scanner: None });
        if self.try_xaml() {
            drop(window); // destroys the windows
            self.xaml_retry_every = RETRY_XAML.0;
            self.update(true);
        } else {
            self.mode = window;
            self.xaml_retry_every = (self.xaml_retry_every * 2).min(RETRY_XAML.1);
        }
    }

    /// Injects or reloads the TAP and waits until Explorer shows our element.
    fn attach_xaml(&mut self) -> bool {
        let Mode::Xaml { surface, last_beat, beat_at, failures, .. } = &mut self.mode else { return false };
        match surface.attach() {
            Ok(true) => {
                *last_beat = surface.heartbeat();
                *beat_at = Instant::now();
                *failures = 0;
                log::info!("strip: hosted in the taskbar's XAML");
                true
            }
            Ok(false) => {
                log::warn!("xaml: no element (heartbeat {}, attached {})", surface.heartbeat(), surface.attached());
                *failures += 1;
                false
            }
            Err(e) => {
                log::warn!("xaml attach: {e}");
                *failures += 1;
                false
            }
        }
    }

    fn fallback_to_window(&mut self) {
        log::info!("strip: using a layered window");
        self.xaml_retry_at = Instant::now();
        self.mode = Mode::Window { bars: Vec::new(), scanner: Some(taskbar::Scanner::new()) };
        self.update(true);
    }

    /// Taskbar (of the chosen monitor, else the primary), DPI scale and height.
    fn measure(&mut self) -> Option<RECT> {
        let choice = self.shared.monitor.read().clone();
        let all = choice.as_deref() == Some(ALL_MONITORS);
        // With "all monitors" the pixels are rendered for the primary taskbar; the others
        // show them at the same size in DIPs.
        let monitor = crate::monitors::resolve(if all { None } else { choice.as_deref() });
        let taskbar = monitor.as_ref().and_then(|m| taskbar_on(m.rect)).or_else(|| find(None, w!("Shell_TrayWnd")))?;
        self.taskbar = taskbar;
        let tb = rect_of(taskbar);
        self.scale = unsafe { GetDpiForWindow(taskbar) }.max(96) as f32 / 96.0;
        self.height = tb.bottom - tb.top;
        if let Mode::Xaml { surface, .. } = &self.mode {
            if let Some(m) = &monitor {
                surface.set_monitor(m.rect);
            }
            surface.set_all_monitors(all);
            surface.set_render_scale(self.scale);
        }
        Some(tb)
    }

    fn modules(&self) -> Vec<ModuleId> {
        let mut modules = self.shared.modules.read().clone();
        let (has_battery, has_npu) = {
            let s = &self.store.read().snapshot;
            (s.battery.is_some(), s.npu.is_some())
        };
        modules.retain(|m| (*m != ModuleId::Battery || has_battery) && (*m != ModuleId::Npu || has_npu));
        modules
    }

    fn layout(&mut self) {
        (self.slots, self.width) = render::layout(&self.modules(), *self.shared.look.read(), self.scale);
    }

    /// Lays out, places (window mode) and paints.
    fn update(&mut self, relayout: bool) {
        if self.measure().is_none() {
            return;
        }
        match &self.mode {
            Mode::Xaml { .. } => {
                if relayout {
                    self.layout();
                    if let Mode::Xaml { surface, .. } = &self.mode {
                        surface.set_position(*self.shared.position.read() == Position::Left);
                    }
                }
            }
            Mode::Window { .. } => self.sync_bars(relayout),
        }
        self.paint();
    }

    /// Timer: watches the TAP's heartbeat (XAML) or the free gap (window).
    fn tick(&mut self) {
        if let Mode::Xaml { surface, last_beat, beat_at, failures, core_mtime } = &mut self.mode {
            // Debug builds: a rebuilt core is swapped into Explorer without restarting it.
            if cfg!(debug_assertions) {
                let mtime = xaml::core_source_mtime();
                if mtime != *core_mtime {
                    *core_mtime = mtime;
                    log::info!("strip: core DLL changed, reloading it in Explorer");
                    self.attach_xaml();
                    return;
                }
            }
            let beat = surface.heartbeat();
            if beat != *last_beat {
                *last_beat = beat;
                *beat_at = Instant::now();
            } else if beat_at.elapsed() > REATTACH_EVERY {
                // Explorer restarted (or our code there died): bring it back.
                *beat_at = Instant::now();
                if *failures >= MAX_ATTACH_FAILURES {
                    self.fallback_to_window();
                    return;
                }
                self.attach_xaml();
            }
        } else if self.xaml_retry_at.elapsed() > self.xaml_retry_every && std::env::var_os("WSTATS_STRIP").is_none() {
            self.retry_xaml();
            self.xaml_retry_at = Instant::now();
        }
        self.update(true);
    }

    fn style(&self) -> Style {
        let colors = *self.shared.colors.read();
        Style { light: self.light, active: *self.shared.active.lock(), primary: colors.0, secondary: colors.1, look: *self.shared.look.read() }
    }

    fn paint(&mut self) {
        let style = self.style();
        match &mut self.mode {
            Mode::Xaml { surface, .. } => {
                if self.width == 0 {
                    return;
                }
                let frame = style.frame(&self.slots, self.width, self.height, self.scale, self.hover);
                if let Err(e) = self.renderer.paint(&frame, &self.store.read()) {
                    log::warn!("strip paint failed: {e:?}");
                    return;
                }
                if let Some((px, w, h)) = self.renderer.pixels() {
                    surface.publish(px, w, h);
                }
            }
            Mode::Window { bars, .. } => {
                for bar in bars {
                    bar.paint(&self.store, style);
                }
            }
        }
    }

    fn slot_at(&self, x: i32, y: i32) -> Option<usize> {
        slot_at(&self.slots, self.height, x, y)
    }

    fn set_hover(&mut self, hover: Option<usize>) {
        if hover != self.hover {
            self.hover = hover;
            self.paint();
        }
    }

    // ---- window mode ---------------------------------------------------------------

    /// One window on the chosen taskbar, or on every visible one with "all monitors".
    /// A window whose taskbar went away (Explorer restarted, monitor unplugged or no
    /// longer chosen) is destroyed with its bar.
    fn sync_bars(&mut self, relayout: bool) {
        let all = self.shared.monitor.read().as_deref() == Some(ALL_MONITORS);
        let wanted: Vec<HWND> =
            if all { taskbars().into_iter().filter(|h| unsafe { IsWindowVisible(*h).as_bool() }).collect() } else { vec![self.taskbar] };
        let modules = self.modules();
        let look = *self.shared.look.read();
        let left = *self.shared.position.read() == Position::Left;
        let Mode::Window { bars, scanner: Some(scanner) } = &mut self.mode else { return };
        scanner.retain(&wanted);
        bars.retain(|b| wanted.contains(&b.taskbar) && unsafe { IsWindow(Some(b.taskbar)).as_bool() });
        for &taskbar in &wanted {
            if !bars.iter().any(|b| b.taskbar == taskbar) {
                match Bar::new(taskbar) {
                    Ok(bar) => bars.push(bar),
                    Err(e) => log::warn!("strip window renderer: {e:?}"),
                }
            }
        }
        for bar in bars {
            let alive = !bar.hwnd.is_invalid() && unsafe { IsWindow(Some(bar.hwnd)).as_bool() };
            if !alive {
                bar.create_window();
            }
            if relayout || !alive {
                bar.place(scanner, modules.clone(), look, left);
            }
        }
    }

    /// Fires an event after releasing the state borrow: handlers may pump messages.
    fn emit(event: Event) {
        STATE.with(|st| {
            if let Ok(g) = st.try_borrow() {
                if let Some(s) = g.as_ref() {
                    (s.on_event)(event);
                }
            }
        });
    }
}

impl Bar {
    fn new(taskbar: HWND) -> windows::core::Result<Self> {
        Ok(Self {
            taskbar,
            hwnd: HWND::default(),
            embedded: false,
            renderer: Renderer::new()?,
            slots: Vec::new(),
            width: 0,
            height: 0,
            scale: 1.0,
            hover: None,
            tracking: false,
        })
    }

    fn create_window(&mut self) {
        unsafe {
            if !self.hwnd.is_invalid() {
                let _ = DestroyWindow(self.hwnd);
            }
            let instance = GetModuleHandleW(None).ok().map(Into::into);
            let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            // A real child of the taskbar hides with it. Needs the Windows 8+ manifest entry.
            let child = CreateWindowExW(ex, w!("wStatsStrip"), w!(""), WS_CHILD | WS_CLIPSIBLINGS, 0, 0, 1, 1, Some(self.taskbar), None, instance, None);
            (self.hwnd, self.embedded) = match child {
                Ok(h) => (h, true),
                Err(e) => {
                    log::warn!("cannot parent strip to the taskbar ({e:?}); using a topmost window");
                    let h = CreateWindowExW(ex | WS_EX_TOPMOST, w!("wStatsStrip"), w!(""), WS_POPUP, 0, 0, 1, 1, None, None, instance, None)
                        .expect("strip window");
                    (h, false)
                }
            };
        }
        self.hover = None;
        self.tracking = false;
    }

    /// Puts the window in the free gap (left or right, per settings) and drops trailing
    /// items that do not fit. It never covers a taskbar button.
    fn place(&mut self, scanner: &taskbar::Scanner, mut modules: Vec<ModuleId>, look: Look, left: bool) {
        let tb = rect_of(self.taskbar);
        self.scale = unsafe { GetDpiForWindow(self.taskbar) }.max(96) as f32 / 96.0;
        self.height = tb.bottom - tb.top;
        let s = self.scale;
        let margin = (4.0 * s) as i32;

        // Hidden until the first scan says where the free space is.
        let Some(scan) = scanner.scan(self.taskbar, tb) else {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        };
        let tray_left = scan
            .tray_left
            .or_else(|| find(Some(self.taskbar), w!("TrayNotifyWnd")).map(|h| rect_of(h).left))
            .unwrap_or(tb.right - (88.0 * s) as i32);

        let (_, full) = render::layout(&modules, look, s);
        let smallest = (40.0 * s) as i32;
        let (left_x, left_room) = taskbar::fit_left(&scan.obstacles, tb.left, tb.right, tray_left, margin);
        let use_left = left && left_room >= smallest;
        let room = if use_left { left_room } else { taskbar::fit(tray_left, &scan.obstacles, tb.left, full, margin).1 };

        let (mut slots, mut width) = render::layout(&modules, look, s);
        while width > room && !modules.is_empty() {
            modules.pop();
            (slots, width) = render::layout(&modules, look, s);
        }
        self.slots = slots;
        self.width = width;

        let x = if use_left { left_x } else { taskbar::fit(tray_left, &scan.obstacles, tb.left, width, margin).0 - width };
        let (x, y) = if self.embedded { (x - tb.left, 0) } else { (x, tb.top) };
        let visible = if width > 0 { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        unsafe {
            let insert = if self.embedded { HWND_TOP } else { HWND_TOPMOST };
            let _ = SetWindowPos(self.hwnd, Some(insert), x, y, width.max(1), self.height, SWP_NOACTIVATE | visible);
        }
    }

    fn paint(&mut self, store: &StoreHandle, style: Style) {
        if self.width == 0 || self.hwnd.is_invalid() {
            return;
        }
        let frame = style.frame(&self.slots, self.width, self.height, self.scale, self.hover);
        if let Err(e) = self.renderer.paint(&frame, &store.read()) {
            log::warn!("strip paint failed: {e:?}");
            return;
        }
        let _ = self.renderer.present_layered(self.hwnd);
    }

    /// Screen rect of slot `i`, given the window's screen origin.
    fn slot_rect(&self, i: usize, origin: (i32, i32)) -> Option<(ModuleId, RECT)> {
        let s = self.slots.get(i)?;
        let rect = RECT { left: origin.0 + s.x, top: origin.1, right: origin.0 + s.x + s.w, bottom: origin.1 + self.height };
        Some((s.module, rect))
    }
}

static TASKBAR_CREATED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let taskbar_created = *TASKBAR_CREATED.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
    match msg {
        WM_APP_REFRESH => {
            with_state(|s| s.update(false));
            LRESULT(0)
        }
        WM_TIMER => {
            with_state(|s| s.tick());
            LRESULT(0)
        }
        xaml::MSG_POINTER => {
            let (x, y) = point(lp);
            if wp.0 != xaml::POINTER_MOVE {
                log::debug!("pointer event {} at x={x}", wp.0);
            }
            let event = with_state(|s| match wp.0 {
                xaml::POINTER_MOVE => {
                    let hover = s.slot_at(x, y);
                    s.set_hover(hover);
                    None
                }
                xaml::POINTER_LEAVE => {
                    s.set_hover(None);
                    None
                }
                xaml::POINTER_LEFT => {
                    let Mode::Xaml { surface, .. } = &s.mode else { return None };
                    let origin = surface.element_origin();
                    // The clicked taskbar may have another DPI than the one we render for.
                    let k = surface.click_scale() / s.scale;
                    let slot = s.slots.get(s.slot_at(x, y)?)?;
                    let left = origin.0 + (slot.x as f32 * k) as i32;
                    let rect = RECT {
                        left,
                        top: origin.1,
                        right: left + (slot.w as f32 * k) as i32,
                        bottom: origin.1 + (s.height as f32 * k) as i32,
                    };
                    Some(Event::Click { module: slot.module, rect })
                }
                xaml::POINTER_RIGHT => {
                    let mut p = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut p);
                    }
                    Some(Event::Menu { x: p.x, y: p.y })
                }
                _ => None,
            })
            .flatten();
            if let Some(e) = event {
                State::emit(e);
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_DPICHANGED => {
            with_state(|s| {
                s.light = crate::win::taskbar_is_light();
                s.update(true);
            });
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        m if m == taskbar_created => {
            // Explorer restarted: our window (child of the old taskbar) or our code in the
            // old Explorer is gone. The XAML side is re-attached from the timer once the
            // new taskbar is ready; the window side is recreated right away.
            with_state(|s| {
                if let Mode::Window { bars, .. } = &mut s.mode {
                    bars.clear();
                }
                s.update(true);
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe extern "system" fn strip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE | WM_MOUSELEAVE => {
            let (x, y) = point(lp);
            with_state(|s| {
                let style = s.style();
                let Some(bar) = bar_of(&mut s.mode, hwnd) else { return };
                let hover = if msg == WM_MOUSELEAVE {
                    bar.tracking = false;
                    None
                } else {
                    if !bar.tracking {
                        let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                        bar.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
                    }
                    slot_at(&bar.slots, bar.height, x, y)
                };
                if hover != bar.hover {
                    bar.hover = hover;
                    bar.paint(&s.store, style);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let (x, y) = point(lp);
            let event = with_state(|s| {
                let origin = rect_of(hwnd);
                if msg == WM_RBUTTONUP {
                    return Some(Event::Menu { x: origin.left + x, y: origin.top + y });
                }
                let bar = bar_of(&mut s.mode, hwnd)?;
                let (module, rect) = bar.slot_rect(slot_at(&bar.slots, bar.height, x, y)?, (origin.left, origin.top))?;
                Some(Event::Click { module, rect })
            })
            .flatten();
            if let Some(e) = event {
                State::emit(e);
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// A native context menu at screen `(x, y)`. Must be called on the strip thread (i.e.
/// from the `on_event` callback). Items are `(id, label, checked)`; id 0 is a separator.
/// Returns the chosen id.
pub fn context_menu(shared: &Shared, x: i32, y: i32, items: &[(u32, &str, bool)]) -> Option<u32> {
    let owner = HWND(shared.host.load(Ordering::Acquire) as _);
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for &(id, label, checked) in items {
            if id == 0 {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            } else {
                let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
                let _ = AppendMenuW(menu, flags, id as usize, &windows::core::HSTRING::from(label));
            }
        }
        // Without this the menu does not close when clicking elsewhere.
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN).0, x, y, owner, None);
        let _ = DestroyMenu(menu);
        (cmd.0 != 0).then_some(cmd.0 as u32)
    }
}
