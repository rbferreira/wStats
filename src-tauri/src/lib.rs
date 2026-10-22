mod config;
mod monitors;
mod lang;
mod popup;
mod sensor_service;
mod shell;
mod stats;
mod strip;
mod update;
mod win;

use config::Settings;
use parking_lot::RwLock;
use popup::Popup;
use stats::{Control, Shared as Store};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::utils::config::WindowEffectsConfig;
use tauri::window::Effect;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_notification::NotificationExt;

pub struct App {
    store: Store,
    control: Arc<Control>,
    strip: Arc<strip::Shared>,
    popup: Arc<Popup>,
    settings: RwLock<Settings>,
    settings_path: PathBuf,
    updater: Arc<update::Updater>,
}

#[tauri::command]
fn get_state(app: tauri::State<'_, Arc<App>>) -> serde_json::Value {
    let s = app.store.read();
    serde_json::json!({ "snapshot": s.snapshot, "history": s.history })
}

#[tauri::command]
fn get_long_history(app: tauri::State<'_, Arc<App>>) -> Vec<stats::long::Minute> {
    app.store.read().long.minutes()
}

#[tauri::command]
fn popup_size(app: tauri::State<'_, Arc<App>>, height: f64) {
    app.popup.on_size(height);
}

#[tauri::command]
fn popup_pending(app: tauri::State<'_, Arc<App>>) -> Option<config::ModuleId> {
    app.popup.pending_module()
}

#[tauri::command]
async fn app_icon(exe: String) -> Option<String> {
    shell::icon_data_url(&exe)
}

#[tauri::command]
fn launch(app: tauri::State<'_, Arc<App>>, id: String) {
    app.popup.hide();
    shell::launch(&id);
}

#[tauri::command]
async fn sensor_status() -> sensor_service::Status {
    sensor_service::status()
}

/// Installs or removes the sensor service (one UAC prompt).
#[tauri::command]
async fn sensor_service(install: bool) -> Result<(), String> {
    sensor_service::run_elevated(if install { "install" } else { "uninstall" })
}

#[tauri::command]
fn list_monitors() -> Vec<monitors::Monitor> {
    monitors::list()
}

/// The strip as the taskbar would draw it, for the settings window's preview.
#[tauri::command]
async fn strip_preview(
    app: tauri::State<'_, Arc<App>>,
    modules: Vec<config::ModuleId>,
    light: Option<bool>,
    look: Option<config::Look>,
) -> Result<Option<String>, ()> {
    let light = light.unwrap_or_else(win::taskbar_is_light);
    // The page passes its own look: settings are saved a moment after each change.
    let look = look.unwrap_or_else(|| *app.strip.look.read());
    let store = app.store.read();
    Ok(strip::preview_png(&modules, look, &store, light, *app.strip.colors.read(), 2.0))
}

#[tauri::command]
fn get_autostart(handle: AppHandle) -> bool {
    handle.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(handle: AppHandle, enabled: bool) -> Result<(), String> {
    let al = handle.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

#[tauri::command]
fn update_status(app: tauri::State<'_, Arc<App>>) -> update::Status {
    app.updater.status()
}

#[tauri::command]
fn check_update(app: tauri::State<'_, Arc<App>>) {
    app.updater.check_now();
}

#[tauri::command]
fn open_release_notes(app: tauri::State<'_, Arc<App>>) {
    app.updater.open_notes();
}

#[tauri::command]
fn install_update(handle: AppHandle) {
    start_update(&handle);
}

/// Downloads and runs the new release's setup, then quits so it can replace wStats.
/// Errors (UAC refused, download failed) land in the update status Settings shows.
fn start_update(handle: &AppHandle) {
    let Some(app) = handle.try_state::<Arc<App>>().map(|a| a.inner().clone()) else { return };
    let handle = handle.clone();
    let _ = std::thread::Builder::new().name("update-install".into()).spawn(move || {
        if app.updater.install().is_ok() {
            handle.exit(0);
        }
    });
}

/// The update status changed: tell Settings, offer it in the menus, and announce a new
/// release once.
fn update_changed(handle: &AppHandle, st: &update::Status) {
    let _ = handle.emit("update", st);
    if let (Some(tray), Ok(menu)) = (handle.tray_by_id("main"), tray_menu(handle)) {
        let _ = tray.set_menu(Some(menu));
    }
    let (Some(version), Some(app)) = (&st.latest, handle.try_state::<Arc<App>>()) else { return };
    if app.settings.read().update_notified.as_ref() == Some(version) {
        return;
    }
    let body = format!("{} {version}. {}", lang::tr("Nova versão do wStats:", "New wStats version:"), lang::tr("Atualize pelo menu ou em Configurações.", "Update from the menu or in Settings."));
    if let Err(e) = handle.notification().builder().title("wStats").body(body).show() {
        log::warn!("update notification: {e}");
    }
    let mut s = app.settings.read().clone();
    s.update_notified = Some(version.clone());
    let _ = app.save(s, handle);
}

/// "Update to 0.4.0…", while a newer release is known.
fn update_label(app: &App) -> Option<String> {
    app.updater.status().latest.map(|v| format!("{} {v}…", lang::tr("Atualizar para a versão", "Update to version")))
}

#[tauri::command]
fn open_settings(handle: AppHandle) {
    show_settings(&handle);
}

fn show_settings(handle: &AppHandle) {
    if let Some(w) = handle.get_webview_window("settings") {
        if let Ok(hwnd) = w.hwnd() {
            win::force_foreground(hwnd);
        }
        return;
    }
    let built = WebviewWindowBuilder::new(handle, "settings", WebviewUrl::App("settings.html".into()))
        .title("wStats")
        .inner_size(1040.0, 700.0)
        .min_inner_size(900.0, 600.0)
        .center()
        // Tauri's file-drop handler takes over every drag in the window on Windows,
        // which kills the HTML drag and drop that reorders the items.
        .disable_drag_drop_handler()
        .additional_browser_args(popup::BROWSER_ARGS)
        .effects(WindowEffectsConfig { effects: vec![Effect::Mica], ..Default::default() })
        .build();
    match built {
        Ok(w) => {
            if let Ok(hwnd) = w.hwnd() {
                win::force_foreground(hwnd);
            }
            if let Some(app) = handle.try_state::<Arc<App>>() {
                app.control.settings_open.store(true, Ordering::Relaxed);
            }
        }
        Err(e) => log::warn!("settings window: {e}"),
    }
}

#[tauri::command]
fn get_settings(app: tauri::State<'_, Arc<App>>) -> Settings {
    app.settings.read().clone()
}

#[tauri::command]
fn set_settings(app: tauri::State<'_, Arc<App>>, handle: AppHandle, settings: Settings) -> Result<(), String> {
    app.save(settings, &handle)
}

impl App {
    /// Applies, persists and broadcasts new settings (every window re-reads them).
    fn save(&self, s: Settings, handle: &AppHandle) -> Result<(), String> {
        let relabel = s.language != self.settings.read().language;
        self.apply(&s);
        if relabel {
            // The tray menu is built once; give it the new language.
            if let (Some(tray), Ok(menu)) = (handle.tray_by_id("main"), tray_menu(handle)) {
                let _ = tray.set_menu(Some(menu));
            }
        }
        let result = s.save(&self.settings_path).map_err(|e| e.to_string());
        *self.settings.write() = s.clone();
        let _ = handle.emit("settings", &s);
        result
    }

    fn apply(&self, s: &Settings) {
        lang::set(s.language);
        // Turned on in Settings: check right away rather than tomorrow.
        if !self.updater.enabled.swap(s.check_updates, Ordering::Relaxed) && s.check_updates {
            self.updater.check_now();
        }
        self.control.interval_ms.store(s.interval_ms, Ordering::Relaxed);
        *self.control.fans.write() = stats::FanPrefs { hidden: s.hidden_fans.clone(), bar: s.bar_fan.clone() };
        if let Some(p) = &self.control.pinger {
            *p.host.write() = s.ping_host.clone();
        }
        *self.strip.modules.write() = s.modules.clone();
        *self.strip.position.write() = s.position;
        *self.strip.monitor.write() = s.monitor.clone();
        *self.strip.look.write() = s.look();
        let color = |hex: &str, fallback| strip::Rgba::from_hex(hex).unwrap_or(fallback);
        *self.strip.colors.write() = (color(&s.colors.primary, strip::Rgba::BLUE), color(&s.colors.secondary, strip::Rgba::GREEN));
        self.strip.refresh();
        self.popup.set_save_memory(s.save_memory);
    }
}

const MENU_AUTOSTART: u32 = 1;
const MENU_QUIT: u32 = 2;
const MENU_SETTINGS: u32 = 3;
const MENU_UPDATE: u32 = 4;

fn strip_event(handle: &AppHandle, app: &Arc<App>, event: strip::Event) {
    match event {
        strip::Event::Click { module, rect } => app.popup.toggle(module, rect),
        strip::Event::Menu { x, y } => {
            app.popup.hide();
            let autostart = handle.autolaunch().is_enabled().unwrap_or(false);
            let update = update_label(app);
            let mut items = vec![(MENU_SETTINGS, lang::tr("Configurações…", "Settings…"), false)];
            if let Some(label) = &update {
                items.push((MENU_UPDATE, label.as_str(), false));
            }
            items.extend([
                (0, "", false),
                (MENU_AUTOSTART, lang::tr("Iniciar com o Windows", "Start with Windows"), autostart),
                (0, "", false),
                (MENU_QUIT, lang::tr("Sair do wStats", "Quit wStats"), false),
            ]);
            match strip::context_menu(&app.strip, x, y, &items) {
                Some(MENU_SETTINGS) => {
                    let h = handle.clone();
                    let _ = handle.run_on_main_thread(move || show_settings(&h));
                }
                Some(MENU_UPDATE) => start_update(handle),
                Some(MENU_AUTOSTART) => toggle_autostart(handle),
                Some(MENU_QUIT) => handle.exit(0),
                _ => {}
            }
        }
    }
}

fn toggle_autostart(handle: &AppHandle) {
    let al = handle.autolaunch();
    let _ = if al.is_enabled().unwrap_or(false) { al.disable() } else { al.enable() };
}

fn tray_menu(handle: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let enabled = handle.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(handle, "autostart", lang::tr("Iniciar com o Windows", "Start with Windows"), true, enabled, None::<&str>)?;
    let quit = MenuItem::with_id(handle, "quit", lang::tr("Sair do wStats", "Quit wStats"), true, None::<&str>)?;
    let settings = MenuItem::with_id(handle, "settings", lang::tr("Configurações…", "Settings…"), true, None::<&str>)?;
    let menu = Menu::with_items(handle, &[&settings, &PredefinedMenuItem::separator(handle)?, &autostart, &PredefinedMenuItem::separator(handle)?, &quit])?;
    if let Some(label) = handle.try_state::<Arc<App>>().and_then(|app| update_label(&app)) {
        menu.insert(&MenuItem::with_id(handle, "update", label, true, None::<&str>)?, 1)?;
    }
    Ok(menu)
}

fn tray(handle: &AppHandle) -> tauri::Result<()> {
    let menu = tray_menu(handle)?;
    TrayIconBuilder::with_id("main")
        .icon(handle.default_window_icon().unwrap().clone())
        .tooltip("wStats")
        .menu(&menu)
        .on_menu_event(|h, e| match e.id().as_ref() {
            "settings" => show_settings(h),
            "update" => start_update(h),
            "autostart" => toggle_autostart(h),
            "quit" => h.exit(0),
            _ => {}
        })
        .build(handle)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Launching wStats again while it runs opens its settings.
        .plugin(tauri_plugin_single_instance::init(|handle, _, _| show_settings(handle)))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_long_history,
            popup_size,
            popup_pending,
            app_icon,
            launch,
            get_settings,
            set_settings,
            list_monitors,
            sensor_status,
            sensor_service,
            strip_preview,
            get_autostart,
            set_autostart,
            open_settings,
            update_status,
            check_update,
            install_update,
            open_release_notes
        ])
        .setup(|tauri_app| {
            // Here, not before the builder: a second instance exits in the single-instance
            // plugin, and would otherwise truncate the running instance's log on the way.
            win::init_log();
            let handle = tauri_app.handle().clone();
            let settings_path = handle.path().app_config_dir()?.join("settings.json");
            let mut settings = Settings::load(&settings_path);
            if settings.offer_npu(stats::npu_present()) {
                let _ = settings.save(&settings_path);
            }

            // With memory saving on, the popup is only built on first use.
            if !settings.save_memory {
                popup::create(&handle)?;
            }

            let data_dir = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("wStats"));
            let store = stats::new_store(data_dir.map(|d| d.join("history.json")));
            let control = Arc::new(Control {
                interval_ms: settings.interval_ms.into(),
                fans: stats::FanPrefs { hidden: settings.hidden_fans.clone(), bar: settings.bar_fan.clone() }.into(),
                pinger: Some(stats::ping::Pinger::spawn(settings.ping_host.clone())),
                ..Default::default()
            });
            let strip_shared = Arc::new(strip::Shared::default());
            *strip_shared.modules.write() = settings.modules.clone();
            *strip_shared.position.write() = settings.position;
            *strip_shared.monitor.write() = settings.monitor.clone();
            let updater = {
                let handle = handle.clone();
                update::Updater::spawn(settings.check_updates, move |st| update_changed(&handle, st))
            };
            let app = Arc::new(App {
                store: store.clone(),
                control: control.clone(),
                strip: strip_shared.clone(),
                popup: Popup::new(handle.clone(), control.clone(), strip_shared.clone()),
                settings: RwLock::new(settings),
                settings_path,
                updater,
            });
            handle.manage(app.clone());
            app.apply(&app.settings.read().clone());

            let tick = {
                let (handle, app) = (handle.clone(), app.clone());
                move |snap: &stats::model::Snapshot| {
                    app.strip.refresh();
                    if app.popup.is_open() {
                        let _ = handle.emit_to("popup", "stats", snap);
                    }
                    if app.control.settings_open.load(Ordering::Relaxed) {
                        let _ = handle.emit_to("settings", "stats", snap);
                    }
                }
            };
            stats::spawn(store.clone(), control, tick);

            let on_strip = {
                let (handle, app) = (handle.clone(), app.clone());
                move |e| strip_event(&handle, &app, e)
            };
            strip::spawn(store, strip_shared, on_strip);

            if let Err(e) = tray(&handle) {
                log::warn!("tray icon: {e}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "settings" {
                if let WindowEvent::Destroyed = event {
                    if let Some(app) = window.app_handle().try_state::<Arc<App>>() {
                        app.control.settings_open.store(false, Ordering::Relaxed);
                    }
                }
            }
            if window.label() == "popup" {
                if let WindowEvent::Focused(false) = event {
                    if let Some(app) = window.app_handle().try_state::<Arc<App>>() {
                        app.popup.hide();
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building wStats")
        .run(|handle, event| match event {
            // There is no main window: hiding the popup must not end the app.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            tauri::RunEvent::Exit => {
                if let Some(app) = handle.try_state::<Arc<App>>() {
                    app.store.read().long.save();
                }
            }
            _ => {}
        });
}
