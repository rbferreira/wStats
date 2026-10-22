//! New releases. A minute after starting, and then once a day, wStats asks GitHub for the
//! latest release (Settings › About turns this off). A newer one shows in Settings and in
//! both menus. Installing it downloads the setup attached to the release, checks it
//! against the SHA-256 GitHub publishes for it, and runs it: the setup asks for UAC once
//! and updates the sensor service as well.

use crate::lang::tr;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

const LATEST: &str = "https://api.github.com/repos/rbferreira/wStats/releases/latest";
/// Where a release's setup may come from (GitHub redirects the download to its storage).
const DOWNLOADS: &str = "https://github.com/rbferreira/wStats/releases/download/";
const FIRST_CHECK: Duration = Duration::from_secs(60);
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The setup is about 5 MB; the API answer, a few KB.
const MAX_SETUP: usize = 64 << 20;
const MAX_JSON: usize = 1 << 20;

#[derive(Serialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub current: String,
    /// The latest release, when it is newer than this build.
    pub latest: Option<String>,
    /// Its page on GitHub, with the release notes.
    pub url: Option<String>,
    /// Unix seconds of the last check that got an answer.
    pub checked_at: Option<u64>,
    /// A check or a download is running.
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct Setup {
    url: String,
    name: String,
    size: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `sha256:<hex>`, published by GitHub for every asset.
    #[serde(default)]
    digest: Option<String>,
}

pub struct Updater {
    status: Mutex<Status>,
    setup: Mutex<Option<Setup>>,
    /// Settings › About: look for new releases.
    pub enabled: AtomicBool,
    on_change: Box<dyn Fn(&Status) + Send + Sync>,
}

impl Updater {
    /// Starts the daily check. `on_change` runs (on a background thread) whenever the
    /// status changes.
    pub fn spawn(enabled: bool, on_change: impl Fn(&Status) + Send + Sync + 'static) -> Arc<Self> {
        let me = Arc::new(Self {
            status: Mutex::new(Status { current: env!("CARGO_PKG_VERSION").into(), ..Default::default() }),
            setup: Mutex::new(None),
            enabled: AtomicBool::new(enabled),
            on_change: Box::new(on_change),
        });
        let bg = me.clone();
        let _ = std::thread::Builder::new().name("update".into()).spawn(move || {
            std::thread::sleep(FIRST_CHECK);
            loop {
                if bg.enabled.load(Ordering::Relaxed) {
                    bg.check();
                }
                std::thread::sleep(EVERY);
            }
        });
        me
    }

    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    fn update(&self, change: impl FnOnce(&mut Status)) {
        let st = {
            let mut s = self.status.lock();
            change(&mut s);
            s.clone()
        };
        (self.on_change)(&st);
    }

    /// Checks now, on its own thread (the button in Settings, or the check turned on).
    pub fn check_now(self: &Arc<Self>) {
        if self.status.lock().busy {
            return;
        }
        let me = self.clone();
        let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || me.check());
    }

    fn check(&self) {
        self.update(|s| {
            s.busy = true;
            s.error = None;
        });
        let result = latest().map(|(release, setup)| {
            let version = release.tag_name.trim_start_matches('v').to_string();
            let newer = newer_than(&version, env!("CARGO_PKG_VERSION"));
            (newer.then_some((version, release.html_url)), setup.filter(|_| newer))
        });
        match &result {
            Ok((_, setup)) => *self.setup.lock() = setup.clone(),
            Err(e) => log::warn!("update check: {e}"),
        }
        self.update(|s| {
            s.busy = false;
            match result {
                Ok((found, _)) => {
                    s.checked_at = SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs());
                    (s.latest, s.url) = found.unzip();
                }
                Err(e) => s.error = Some(e),
            }
        });
    }

    /// Opens the new release's page (its notes) in the browser.
    pub fn open_notes(&self) {
        let Some(url) = self.status.lock().url.clone() else { return };
        unsafe {
            windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }

    /// Downloads and checks the new release's setup, then runs it. Once this returns Ok
    /// the setup is running (UAC accepted) and wStats should quit so it can be replaced.
    pub fn install(&self) -> Result<(), String> {
        let setup = self.setup.lock().clone().ok_or(tr("a versão nova não traz o instalador", "the new version has no setup attached"))?;
        self.update(|s| {
            s.busy = true;
            s.error = None;
        });
        let result = download(&setup).and_then(|path| run(&path));
        self.update(|s| {
            s.busy = false;
            s.error = result.as_ref().err().cloned();
        });
        result
    }
}

/// The latest release, and its setup when it ships one with a digest.
fn latest() -> Result<(Release, Option<Setup>), String> {
    let body = get(LATEST, "application/vnd.github+json", MAX_JSON)?;
    let release: Release = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let setup = release.assets.iter().find_map(|a| {
        let sha256 = a.digest.as_deref()?.strip_prefix("sha256:")?.to_ascii_lowercase();
        let ours = a.name.starts_with("wStats_") && a.name.ends_with("_x64-setup.exe") && a.browser_download_url.starts_with(DOWNLOADS);
        ours.then(|| Setup { url: a.browser_download_url.clone(), name: a.name.clone(), size: a.size, sha256 })
    });
    Ok((release, setup))
}

/// Downloads the setup to %TEMP% and checks its size and SHA-256.
fn download(setup: &Setup) -> Result<std::path::PathBuf, String> {
    let bytes = get(&setup.url, "application/octet-stream", MAX_SETUP)?;
    if bytes.len() as u64 != setup.size || sha256_hex(&bytes)? != setup.sha256 {
        return Err(tr("o arquivo baixado não confere com o publicado", "the download does not match the published file").into());
    }
    let dir = std::env::temp_dir().join("wStats-update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // A plain file name from the API, and checked above, but keep it to a file name.
    let name = std::path::Path::new(&setup.name).file_name().ok_or("bad file name")?;
    let path = dir.join(name);
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Starts the setup. It is a per-machine installer, so Windows asks for UAC here; a
/// refusal comes back as an error and wStats keeps running.
fn run(path: &std::path::Path) -> Result<(), String> {
    let file = HSTRING::from(path.as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| {
        if e.code() == ERROR_CANCELLED.to_hresult() { tr("cancelado no UAC", "cancelled at the UAC prompt").into() } else { e.message() }
    })
}

fn sha256_hex(bytes: &[u8]) -> Result<String, String> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, bytes, &mut out) };
    status.ok().map_err(|e| e.message())?;
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// `a` newer than `b`, both `major.minor.patch` (a `-suffix` is ignored).
fn newer_than(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Option<(u32, u32, u32)> {
        let mut it = v.split('-').next()?.split('.').map(|p| p.parse::<u32>().ok());
        Some((it.next()??, it.next()??, it.next()??))
    };
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

/// WinHTTP handle, closed on drop.
struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void) -> Result<Self, String> {
        if h.is_null() { Err(windows::core::Error::from_thread().message()) } else { Ok(Self(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// One HTTPS GET through WinHTTP (the system's proxy settings, no extra TLS stack).
/// Redirects are followed: release downloads live on another host.
fn get(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>, String> {
    let rest = url.strip_prefix("https://").ok_or("https only")?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let agent = HSTRING::from(format!("wStats/{}", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = Handle::new(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
        let _ = WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 30_000);
        let connect = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT as u16, 0))?;
        let request = Handle::new(WinHttpOpenRequest(connect.0, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE))?;
        let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0).map_err(|e| e.message())?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| e.message())?;
        let mut code = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(request.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some((&raw mut code).cast()), &mut len, std::ptr::null_mut())
            .map_err(|e| e.message())?;
        if code != 200 {
            return Err(format!("HTTP {code}"));
        }
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut n = 0u32;
            WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n).map_err(|e| e.message())?;
            if n == 0 {
                return Ok(body);
            }
            body.extend_from_slice(&buf[..n as usize]);
            if body.len() > limit {
                return Err(tr("resposta grande demais", "response too large").into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer_than("0.3.1", "0.3.0"));
        assert!(newer_than("0.10.0", "0.9.9"));
        assert!(newer_than("1.0.0", "0.99.0"));
        assert!(!newer_than("0.3.0", "0.3.0"));
        assert!(!newer_than("0.2.9", "0.3.0"));
        assert!(newer_than("0.4.0-beta", "0.3.9"));
        assert!(!newer_than("garbage", "0.3.0"));
    }

    #[test]
    fn hashes() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// Talks to GitHub: run by hand (`cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn reads_the_latest_release() {
        let (release, setup) = latest().expect("latest release");
        assert!(release.tag_name.starts_with('v'), "{}", release.tag_name);
        let setup = setup.expect("setup asset with a digest");
        assert_eq!(setup.sha256.len(), 64);
        let path = download(&setup).expect("download matches its digest");
        println!("{} {} {} bytes -> {}", release.tag_name, setup.name, setup.size, path.display());
        let _ = std::fs::remove_file(path);
        // A digest that does not match is refused.
        let wrong = Setup { sha256: "0".repeat(64), ..setup };
        assert!(download(&wrong).is_err());
    }
}
