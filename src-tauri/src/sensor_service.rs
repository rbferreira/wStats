//! Installing and inspecting the sensor service (sensors/ in this repo) from the
//! settings window. Installing needs admin rights, so it runs the service binary
//! elevated (one UAC prompt); everything else here works as a normal user.

use crate::lang::tr;
use crate::stats::service;
use serde::Serialize;
use std::path::PathBuf;
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, w};

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The service binary ships with this build (it can be installed).
    pub available: bool,
    pub installed: bool,
    pub running: bool,
    /// The installed service differs from the one this build ships.
    pub outdated: bool,
    /// The PawnIO driver is installed (CPU temperatures).
    pub pawnio: bool,
    /// This build ships the PawnIO setup, so installing the service installs PawnIO too.
    pub pawnio_bundled: bool,
    /// What the service publishes right now.
    pub cpu: bool,
    /// CPU power (RAPL) read by the service.
    pub power: bool,
    pub fans: usize,
    pub board: usize,
    /// Why a source is missing, as reported by the service.
    pub notes: Vec<String>,
}

fn binary() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [exe_dir.join("wstats-sensors.exe"), PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sensors/target/release/wstats-sensors.exe")]
        .into_iter()
        .find(|p| p.exists())
}

fn service_state() -> Option<u32> {
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT).ok()?;
        let state = OpenServiceW(scm, w!("wStatsSensors"), SERVICE_QUERY_STATUS).ok().and_then(|svc| {
            let mut st = SERVICE_STATUS::default();
            let ok = QueryServiceStatus(svc, &mut st).is_ok();
            let _ = CloseServiceHandle(svc);
            ok.then_some(st.dwCurrentState.0)
        });
        let _ = CloseServiceHandle(scm);
        state
    }
}

/// Where `wstats-sensors install` puts the binary.
fn installed_binary() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("ProgramFiles")?).join(r"wStats\Sensors\wstats-sensors.exe"))
}

pub fn status() -> Status {
    let state = service_state();
    let outdated = state.is_some()
        && match (binary().and_then(|p| std::fs::read(p).ok()), installed_binary().and_then(|p| std::fs::read(p).ok())) {
            (Some(ours), Some(theirs)) => ours != theirs,
            _ => false,
        };
    let pawnio = std::env::var_os("ProgramFiles").is_some_and(|d| PathBuf::from(d).join(r"PawnIO\PawnIOLib.dll").exists());
    let pawnio_bundled = binary().and_then(|b| Some(b.parent()?.join("pawnio").join("PawnIO_setup.exe"))).is_some_and(|p| p.exists());
    let mut st = Status { available: binary().is_some(), pawnio_bundled, installed: state.is_some(), running: state == Some(SERVICE_RUNNING.0), outdated, pawnio, ..Default::default() };
    if let Some(p) = service::Reader::open().and_then(|r| r.read()) {
        st.cpu = p.cpu.as_ref().is_some_and(|c| c.package.is_some());
        st.power = !p.power.is_empty();
        st.fans = p.fans.len() + p.board.as_ref().map_or(0, |b| b.fans.len());
        st.board = p.thermals.len() + p.board.as_ref().map_or(0, |b| b.temps.len());
        st.notes = p.notes;
    }
    st
}

/// Runs the service binary elevated with `verb` ("install" / "uninstall") and waits.
pub fn run_elevated(verb: &str) -> Result<(), String> {
    let exe = binary().ok_or(tr("o serviço de sensores não acompanha esta versão", "this version does not ship the sensor service"))?;
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(verb);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            return Err(if e.code() == ERROR_CANCELLED.to_hresult() { tr("cancelado no UAC", "cancelled at the UAC prompt").into() } else { e.message() });
        }
        if info.hProcess.is_invalid() {
            return Err(tr("o processo não iniciou", "the process did not start").into());
        }
        let waited = WaitForSingleObject(info.hProcess, 60_000);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if waited != WAIT_OBJECT_0 {
            return Err(tr("tempo esgotado", "timed out").into());
        }
        if code != 0 {
            return Err(format!("{} {code}", tr("falhou com o código", "failed with code")));
        }
    }
    Ok(())
}
