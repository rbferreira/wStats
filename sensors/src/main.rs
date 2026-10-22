//! wstats-sensors: the part of wStats that needs admin rights. Runs as a Windows service
//! (LocalSystem), reads CPU temperatures and power (through PawnIO), desktop board fans
//! and temperatures (the Super I/O chip, through PawnIO) and Dell fans and board sensors
//! (through WMI), and publishes them for the wStats app every second.
//!
//!   wstats-sensors install     install PawnIO if shipped alongside and missing, copy to
//!                              Program Files, register and start the service
//!   wstats-sensors uninstall   stop and remove it
//!   wstats-sensors once        print one sample as JSON (run elevated)
//!
//! Started by the service manager without arguments.

#![windows_subsystem = "windows"]

mod cpu;
mod dell;
mod pawnio;
mod publish;
mod rapl;
mod superio;

use serde::Serialize;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoInitializeSecurity, EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE};
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus,
    ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

const SERVICE_NAME: &str = "wStatsSensors";
const DISPLAY_NAME: &str = "wStats Sensors";
/// Shown in services.msc, in the language Windows uses when the service is installed.
fn description() -> &'static str {
    const LANG_PORTUGUESE: u16 = 0x16;
    if unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3ff == LANG_PORTUGUESE {
        "Lê temperaturas, consumo da CPU e ventoinhas para o wStats."
    } else {
        "Reads temperatures, CPU power and fans for wStats."
    }
}
const INTERVAL: Duration = Duration::from_secs(1);
/// WMI method calls cost more than MSR reads; fans and board sensors change slowly.
const DDV_INTERVAL: Duration = Duration::from_secs(2);
const MODE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Serialize)]
struct Payload<'a> {
    v: u32,
    /// Unix time in milliseconds, so readers can tell a stopped service.
    t: u64,
    cpu: Option<cpu::CpuTemps>,
    /// CPU power by RAPL domain, when Windows does not publish it itself.
    power: &'a [rapl::Power],
    /// The motherboard's Super I/O chip (desktops).
    board: Option<&'a superio::Board>,
    fans: &'a [dell::Fan],
    thermals: &'a [dell::Thermal],
    /// Dell's BIOS thermal mode, when the BIOS offers one.
    thermal_mode: Option<&'a dell::ThermalMode>,
    /// Why a source is missing (no PawnIO, not a Dell, ...).
    notes: &'a [String],
}

struct Sampler {
    cpu: Option<cpu::Reader>,
    rapl: Option<rapl::Rapl>,
    power: Vec<rapl::Power>,
    sio: Option<superio::SuperIo>,
    board: Option<superio::Board>,
    ddv: Option<dell::Ddv>,
    ddv_at: Option<Instant>,
    fans: Vec<dell::Fan>,
    thermals: Vec<dell::Thermal>,
    mode: Option<dell::ThermalMode>,
    mode_at: Option<Instant>,
    notes: Vec<String>,
}

impl Sampler {
    fn new() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = CoInitializeSecurity(None, -1, None, None, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE, None);
        }
        let mut notes = Vec::new();
        let lib = pawnio::Lib::load();
        let cpu = match lib.clone().and_then(|lib| cpu::Reader::open(&lib)) {
            Ok(r) => Some(r),
            Err(e) => {
                notes.push(format!("cpu: {e}"));
                None
            }
        };
        let rapl = cpu.as_ref().and_then(rapl::Rapl::open);
        if cpu.is_some() && rapl.is_none() {
            notes.push("power: no RAPL counters".into());
        }
        let sio = match lib.and_then(|lib| superio::SuperIo::open(&lib)) {
            Ok(s) => Some(s),
            Err(e) => {
                notes.push(format!("superio: {e}"));
                None
            }
        };
        let ddv = match dell::Ddv::open() {
            Ok(d) => Some(d),
            Err(e) => {
                notes.push(e);
                None
            }
        };
        Self { cpu, rapl, power: Vec::new(), sio, board: None, ddv, ddv_at: None, fans: Vec::new(), thermals: Vec::new(), mode: None, mode_at: None, notes }
    }

    fn sample(&mut self) -> Vec<u8> {
        if let (Some(rapl), Some(cpu)) = (&mut self.rapl, &self.cpu) {
            self.power = rapl.read(cpu);
        }
        if let Some(sio) = &mut self.sio {
            // Busy bus (another monitor reading it): keep the last reading.
            if let Some(b) = sio.read() {
                self.board = Some(b);
            }
        }
        if let Some(ddv) = &self.ddv {
            if self.ddv_at.is_none_or(|t| t.elapsed() >= DDV_INTERVAL) {
                self.ddv_at = Some(Instant::now());
                self.fans = ddv.fans().unwrap_or_default();
                self.thermals = ddv.thermals().unwrap_or_default();
            }
        }
        // A BIOS setting: it changes only when someone changes it.
        if self.ddv.is_some() && self.mode_at.is_none_or(|t| t.elapsed() >= MODE_INTERVAL) {
            self.mode_at = Some(Instant::now());
            self.mode = dell::thermal_mode().ok();
        }
        let payload = Payload {
            v: 2,
            t: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
            cpu: self.cpu.as_ref().map(cpu::Reader::read),
            power: &self.power,
            board: self.board.as_ref(),
            fans: &self.fans,
            thermals: &self.thermals,
            thermal_mode: self.mode.as_ref(),
            notes: &self.notes,
        };
        serde_json::to_vec(&payload).unwrap_or_default()
    }
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_args: Vec<OsString>) {
    let _ = run_service();
}

fn run_service() -> windows_service::Result<()> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state, accept| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
    };
    report(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN)?;

    if let Ok(publisher) = publish::Publisher::create() {
        let mut sampler = Sampler::new();
        loop {
            publisher.publish(&sampler.sample());
            if !matches!(stop_rx.recv_timeout(INTERVAL), Err(mpsc::RecvTimeoutError::Timeout)) {
                break;
            }
        }
    }
    report(ServiceState::Stopped, ServiceControlAccept::empty())
}

/// Where the service runs from: a directory only administrators can write to, since
/// the binary runs as SYSTEM.
fn install_path() -> Result<PathBuf, String> {
    let dir = std::env::var_os("ProgramFiles").map(PathBuf::from).ok_or("no ProgramFiles")?;
    Ok(dir.join("wStats").join("Sensors").join("wstats-sensors.exe"))
}

fn stop_and_wait(service: &windows_service::service::Service) {
    if service.query_status().is_ok_and(|s| s.current_state != ServiceState::Stopped) {
        let _ = service.stop();
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until && service.query_status().is_ok_and(|s| s.current_state != ServiceState::Stopped) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Installs the PawnIO driver from the setup shipped next to this binary (the wStats
/// installer puts it in `pawnio\`), unless PawnIO is already there. Without it the
/// service still runs, minus CPU temperatures, so a failure here is not fatal.
fn ensure_pawnio() {
    let installed = std::env::var_os("ProgramFiles").is_some_and(|d| PathBuf::from(d).join(r"PawnIO\PawnIOLib.dll").exists());
    let setup = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join(r"pawnio\PawnIO_setup.exe")));
    if let Some(setup) = setup.filter(|s| !installed && s.exists()) {
        let _ = std::process::Command::new(setup).args(["-install", "-silent"]).status();
    }
}

fn install() -> Result<(), String> {
    ensure_pawnio();
    let target = install_path()?;
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .map_err(|e| format!("service manager: {e}"))?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG;
    let existing = manager.open_service(SERVICE_NAME, access).ok();
    if let Some(s) = &existing {
        stop_and_wait(s);
    }

    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if me != target {
        std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
        // The old process may take a moment to exit after the service reports stopped.
        let mut copied = std::fs::copy(&me, &target);
        for _ in 0..20 {
            if copied.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
            copied = std::fs::copy(&me, &target);
        }
        copied.map_err(|e| format!("copy to {}: {e}", target.display()))?;
    }

    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target,
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = match existing {
        Some(s) => {
            s.change_config(&info).map_err(|e| format!("update service: {e}"))?;
            s
        }
        None => manager.create_service(&info, access).map_err(|e| format!("create service: {e}"))?,
    };
    let _ = service.set_description(description());
    service.start::<&str>(&[]).map_err(|e| format!("start service: {e}"))
}

fn uninstall() -> Result<(), String> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(|e| format!("service manager: {e}"))?;
    if let Ok(service) = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE) {
        stop_and_wait(&service);
        service.delete().map_err(|e| format!("delete service: {e}"))?;
    }
    let target = install_path()?;
    // The process may take a moment to exit after the service reports stopped.
    for _ in 0..20 {
        if !target.exists() || std::fs::remove_file(&target).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if let Some(dir) = target.parent() {
        let _ = std::fs::remove_dir(dir);
        let _ = dir.parent().map(std::fs::remove_dir);
    }
    Ok(())
}

fn main() {
    let arg = std::env::args().nth(1);
    let result = match arg.as_deref() {
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("once") => {
            let mut sampler = Sampler::new();
            // Power is a rate: it needs two readings.
            sampler.sample();
            std::thread::sleep(INTERVAL);
            let json = sampler.sample();
            std::fs::write(std::env::args().nth(2).unwrap_or_else(|| "sensors.json".into()), json).map_err(|e| e.to_string())
        }
        _ => service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|e| e.to_string()),
    };
    // A GUI-subsystem binary has no console: the exit code is the result.
    std::process::exit(if result.is_ok() { 0 } else { 1 });
}
