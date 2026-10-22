//! Per-process CPU, memory and I/O, read for every process at once from
//! `NtQuerySystemInformation`, the way Task Manager does. Opening each process instead
//! (what `sysinfo` did) is denied to an unelevated app for system and protected
//! processes, which then read as idle: Defender at 60% CPU never made the list.

use super::model::{ProcRow, Procs};
use std::collections::HashMap;
use std::time::Instant;
use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS, SystemProcessInformation};
use windows::Win32::Foundation::STATUS_INFO_LENGTH_MISMATCH;
use windows::Win32::Storage::FileSystem::{GetLogicalDrives, QueryDosDeviceW};
use windows::core::HSTRING;

const TOP: usize = 6;
/// Image path of any process by id, no handle needed. Not in the SDK headers.
const SYSTEM_PROCESS_ID_INFORMATION: SYSTEM_INFORMATION_CLASS = SYSTEM_INFORMATION_CLASS(88);

#[repr(C)]
#[derive(Clone, Copy)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

/// `SYSTEM_PROCESS_INFORMATION` with the fields the SDK hides as `Reserved`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcInfo {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

#[repr(C)]
struct ProcIdInfo {
    process_id: usize,
    image_name: UnicodeString,
}

/// A process is its id plus its start time: ids are reused.
type Key = (u32, i64);

/// Counters from the previous sample, to turn totals into rates.
struct Seen {
    exe: String,
    cpu: u64,
    read: u64,
    write: u64,
}

/// Where a process's executable is and what to call it, without opening the process.
pub struct Exes {
    /// exe path -> display name, from the file's version info. Read once per executable.
    names: HashMap<String, String>,
    /// exe path -> what its row groups by: the package folder for a Store app (its helper
    /// executables join it, as in Task Manager), else the path itself.
    groups: HashMap<String, String>,
    /// `\Device\HarddiskVolume3` -> `C:`.
    drives: Vec<(String, String)>,
}

impl Exes {
    pub fn new() -> Self {
        Self { names: HashMap::new(), groups: HashMap::new(), drives: dos_drives() }
    }

    /// `C:\...` path of a process's executable, empty for System, Registry and the like.
    pub fn path(&mut self, pid: u32) -> String {
        let Some(nt) = image_path(pid) else { return String::new() };
        if let Some(p) = to_dos(&self.drives, &nt) {
            return p;
        }
        // A drive mounted after we started.
        self.drives = dos_drives();
        to_dos(&self.drives, &nt).unwrap_or_default()
    }

    /// The row a process with this executable belongs to (see `groups`).
    pub fn group(&mut self, exe: &str) -> String {
        self.groups
            .entry(exe.to_string())
            .or_insert_with(|| crate::shell::package_dir(exe).map_or_else(|| exe.to_string(), |d| d.to_string_lossy().into_owned()).to_lowercase())
            .clone()
    }

    /// A Store app's name ("WhatsApp"), else the executable's description ("Google
    /// Chrome"), else `name` without `.exe`.
    pub fn label(&mut self, exe: &str, name: &str) -> String {
        // No executable (System, Registry, Memory Compression): the name is all there is.
        if exe.is_empty() {
            return display_name(name);
        }
        self.names
            .entry(exe.to_string())
            .or_insert_with(|| crate::shell::package_name(exe).or_else(|| crate::shell::file_description(exe)).unwrap_or_else(|| display_name(name)))
            .clone()
    }
}

pub struct ProcCollector {
    /// Kept between samples; u64 so the entries it holds are aligned.
    buf: Vec<u64>,
    seen: HashMap<Key, Seen>,
    exes: Exes,
    logical: f64,
    last: Option<Instant>,
}

impl ProcCollector {
    pub fn new() -> Self {
        Self {
            buf: vec![0; 64 * 1024],
            seen: HashMap::new(),
            exes: Exes::new(),
            logical: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64,
            last: None,
        }
    }

    /// Every process in one call. `None` only if the call keeps failing.
    fn query(&mut self) -> Option<Vec<(ProcInfo, String)>> {
        for _ in 0..4 {
            let bytes = (self.buf.len() * 8) as u32;
            let mut needed = 0u32;
            let status = unsafe { NtQuerySystemInformation(SystemProcessInformation, self.buf.as_mut_ptr().cast(), bytes, &mut needed) };
            if status == STATUS_INFO_LENGTH_MISMATCH {
                // Processes start between the two calls: leave room for them.
                self.buf.resize((needed.max(bytes) as usize * 3 / 2).div_ceil(8), 0);
                continue;
            }
            if status.is_err() {
                return None;
            }
            let mut out = Vec::with_capacity(512);
            let base = self.buf.as_ptr().cast::<u8>();
            let mut offset = 0usize;
            loop {
                let info = unsafe { std::ptr::read_unaligned(base.add(offset).cast::<ProcInfo>()) };
                let name = unsafe { unicode(&info.image_name) };
                out.push((info, name));
                if info.next_entry_offset == 0 {
                    return Some(out);
                }
                offset += info.next_entry_offset as usize;
            }
        }
        None
    }

    pub fn sample(&mut self) -> Procs {
        let Some(list) = self.query() else { return Procs::default() };
        let now = Instant::now();
        let secs = self.last.map(|t| (now - t).as_secs_f64()).unwrap_or(0.0);
        self.last = Some(now);

        // Every process's executable first: a WebView2 process is counted under the app
        // that hosts it, which needs the host's.
        let mut procs = Vec::with_capacity(list.len());
        for (p, name) in list {
            let pid = p.unique_process_id as u32;
            if pid == 0 {
                continue; // System Idle Process
            }
            let name = if name.is_empty() { "System".to_string() } else { name };
            let prev = self.seen.remove(&(pid, p.create_time));
            let exe = match &prev {
                Some(s) => s.exe.clone(),
                None => self.exes.path(pid),
            };
            procs.push((p, name, exe, prev));
        }
        let nodes: Vec<Node> = procs
            .iter()
            .map(|(p, name, ..)| Node {
                pid: p.unique_process_id as u32,
                parent: p.inherited_from_unique_process_id as u32,
                created: p.create_time,
                webview: name.eq_ignore_ascii_case(WEBVIEW),
            })
            .collect();
        let by_pid: HashMap<u32, usize> = nodes.iter().enumerate().map(|(i, n)| (n.pid, i)).collect();

        // Group by executable so Chrome's 30 renderers read as one row.
        let mut groups: HashMap<String, ProcRow> = HashMap::new();
        let mut seen = HashMap::with_capacity(procs.len());
        for (i, (p, _, exe, prev)) in procs.iter().enumerate() {
            let key: Key = (p.unique_process_id as u32, p.create_time);
            let cpu = (p.user_time + p.kernel_time) as u64;
            let (read, write) = (p.read_transfer_count as u64, p.write_transfer_count as u64);

            let (_, owner_name, owner_exe, _) = &procs[webview_host(&nodes, &by_pid, i).unwrap_or(i)];
            let group = if owner_exe.is_empty() { owner_name.to_lowercase() } else { self.exes.group(owner_exe) };
            let row = groups.entry(group).or_insert_with(|| ProcRow {
                name: self.exes.label(owner_exe, owner_name),
                exe: owner_exe.clone(),
                ..Default::default()
            });
            row.count += 1;
            row.mem += p.working_set_size as u64;
            if let Some(s) = prev
                && secs > 0.0
            {
                // Process times are in 100 ns units.
                row.cpu += (cpu.saturating_sub(s.cpu) as f64 / (secs * 1e7 * self.logical) * 100.0) as f32;
                row.disk_read += read.saturating_sub(s.read) as f64 / secs;
                row.disk_write += write.saturating_sub(s.write) as f64 / secs;
            }
            seen.insert(key, Seen { exe: exe.clone(), cpu, read, write });
        }
        // Whatever was not seen this time has exited.
        self.seen = seen;

        let rows: Vec<ProcRow> = groups.into_values().collect();
        let top = |key: fn(&ProcRow) -> f64| {
            let mut v: Vec<ProcRow> = rows.iter().filter(|r| key(r) > 0.0).cloned().collect();
            v.sort_by(|a, b| key(b).total_cmp(&key(a)));
            v.truncate(TOP);
            v
        };
        Procs {
            by_cpu: top(|r| r.cpu as f64),
            by_mem: top(|r| r.mem as f64),
            by_disk: top(|r| r.disk_read + r.disk_write),
        }
    }
}

const WEBVIEW: &str = "msedgewebview2.exe";

/// What `webview_host` needs to know about a process.
struct Node {
    pid: u32,
    parent: u32,
    created: i64,
    webview: bool,
}

/// The app a WebView2 process works for: its first ancestor that is not WebView2, the
/// way Task Manager files it under WhatsApp instead of "Microsoft Edge WebView2". `None`
/// for any other process, and when the host has exited (the process then keeps a row of
/// its own). A parent started after its child is a reused id, not the parent.
fn webview_host(nodes: &[Node], by_pid: &HashMap<u32, usize>, i: usize) -> Option<usize> {
    if !nodes[i].webview {
        return None;
    }
    let mut cur = i;
    // The chain is short (renderer -> browser -> app); the bound guards against loops.
    for _ in 0..8 {
        let parent = *by_pid.get(&nodes[cur].parent)?;
        if parent == cur || nodes[parent].created > nodes[cur].created {
            return None;
        }
        if !nodes[parent].webview {
            return Some(parent);
        }
        cur = parent;
    }
    None
}

unsafe fn unicode(s: &UnicodeString) -> String {
    if s.buffer.is_null() || s.length == 0 {
        return String::new();
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s.buffer, s.length as usize / 2) })
}

/// NT path of a process's image (`\Device\HarddiskVolume3\...`).
fn image_path(pid: u32) -> Option<String> {
    let mut len = 512usize;
    for _ in 0..2 {
        let mut buf = vec![0u16; len];
        let mut info = ProcIdInfo {
            process_id: pid as usize,
            image_name: UnicodeString { length: 0, maximum_length: (len * 2).min(u16::MAX as usize) as u16, buffer: buf.as_mut_ptr() },
        };
        let status = unsafe {
            NtQuerySystemInformation(SYSTEM_PROCESS_ID_INFORMATION, (&raw mut info).cast(), size_of::<ProcIdInfo>() as u32, std::ptr::null_mut())
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // The call reports the size it needs in MaximumLength.
            len = (info.image_name.maximum_length as usize).div_ceil(2);
            continue;
        }
        if status.is_err() {
            return None;
        }
        let s = unsafe { unicode(&info.image_name) };
        return (!s.is_empty()).then_some(s);
    }
    None
}

/// Device name behind each drive letter, to turn NT paths into ones the shell knows.
fn dos_drives() -> Vec<(String, String)> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = vec![(r"\Device\Mup".to_string(), r"\".to_string())];
    for i in 0..26 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let drive = format!("{}:", (b'A' + i as u8) as char);
        let mut target = [0u16; 512];
        let n = unsafe { QueryDosDeviceW(&HSTRING::from(&drive), Some(&mut target)) } as usize;
        if n == 0 {
            continue;
        }
        // A list of NUL-terminated strings; the first is the current target.
        let end = target[..n].iter().position(|&c| c == 0).unwrap_or(n);
        out.push((String::from_utf16_lossy(&target[..end]), drive));
    }
    out
}

fn to_dos(drives: &[(String, String)], nt: &str) -> Option<String> {
    drives.iter().find_map(|(device, drive)| {
        let rest = nt.get(device.len()..)?;
        (nt[..device.len()].eq_ignore_ascii_case(device) && rest.starts_with('\\')).then(|| format!("{drive}{rest}"))
    })
}

/// `"msedgewebview2.exe"` -> `"msedgewebview2"`.
fn display_name(name: &str) -> String {
    name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    #[test]
    fn layout_matches_ntdll() {
        assert_eq!(offset_of!(ProcInfo, image_name), 56);
        assert_eq!(offset_of!(ProcInfo, unique_process_id), 80);
        assert_eq!(offset_of!(ProcInfo, working_set_size), 144);
        assert_eq!(offset_of!(ProcInfo, read_transfer_count), 232);
        assert_eq!(size_of::<ProcInfo>(), 256);
    }

    #[test]
    fn nt_paths_become_dos_paths() {
        let drives = vec![(r"\Device\Mup".to_string(), r"\".to_string()), (r"\Device\HarddiskVolume3".to_string(), "C:".to_string())];
        assert_eq!(to_dos(&drives, r"\Device\HarddiskVolume3\Windows\explorer.exe").as_deref(), Some(r"C:\Windows\explorer.exe"));
        assert_eq!(to_dos(&drives, r"\Device\HarddiskVolume31\x.exe"), None);
        assert_eq!(to_dos(&drives, r"\Device\Mup\server\share\x.exe").as_deref(), Some(r"\\server\share\x.exe"));
    }

    #[test]
    fn webview_counts_under_its_host() {
        let node = |pid, parent, created, webview| Node { pid, parent, created, webview };
        // app 10 -> browser 20 -> renderer 30; 40 is orphaned; 50's "parent" id was reused
        // by a process started after it.
        let nodes = vec![
            node(10, 1, 100, false),
            node(20, 10, 200, true),
            node(30, 20, 300, true),
            node(40, 99, 400, true),
            node(60, 1, 900, false),
            node(50, 60, 500, true),
        ];
        let by_pid: HashMap<u32, usize> = nodes.iter().enumerate().map(|(i, n)| (n.pid, i)).collect();
        let host = |i| webview_host(&nodes, &by_pid, i);
        assert_eq!(host(0), None, "the app itself");
        assert_eq!(host(1), Some(0), "browser process");
        assert_eq!(host(2), Some(0), "renderer, two levels down");
        assert_eq!(host(3), None, "host exited");
        assert_eq!(host(5), None, "parent id reused");
    }

    /// The point of reading through ntdll: system processes an unelevated app cannot
    /// open still get a path and CPU time.
    #[test]
    fn sees_system_processes() {
        let mut c = ProcCollector::new();
        let list = c.query().expect("process list");
        let lsass = list.iter().find(|(_, n)| n.eq_ignore_ascii_case("lsass.exe")).expect("lsass listed");
        assert!(lsass.0.user_time + lsass.0.kernel_time > 0, "lsass CPU time");
        let exe = c.exes.path(lsass.0.unique_process_id as u32);
        assert!(exe.to_lowercase().ends_with(r"\system32\lsass.exe"), "{exe}");
    }
}

