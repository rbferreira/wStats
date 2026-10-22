//! Desktop motherboards: fans and board temperatures from the Super I/O chip's hardware
//! monitor, through PawnIO's LpcIO module (which only lets us touch the chip's own
//! ports). Reads only; fan control is left to the BIOS.
//!
//! Supported:
//! - Nuvoton NCT6779D and newer (NCT6791D..NCT6799D): ASUS, ASRock, many MSI boards.
//! - Nuvoton NCT6683D/NCT6686D/NCT6687D (embedded-controller style): recent MSI boards.
//! - ITE IT86xx/IT87xx: Gigabyte and others.
//!
//! Register maps as in LibreHardwareMonitor and Linux's nct6775/it87 drivers.
//! Every access holds the ISA bus mutex shared by the monitoring tools that use it
//! (LibreHardwareMonitor, HWiNFO, FanControl...), so we never interleave with them.

use crate::pawnio::{LPC_IO, Lib, Module};
use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

/// Below this, a fan reading is noise; above it, a disconnected header.
const MIN_RPM: u32 = 100;
const MAX_RPM: u32 = 20_000;
/// Plausible board temperatures. Unconnected inputs read garbage outside this range.
const MIN_C: f32 = 1.0;
const MAX_C: f32 = 110.0;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BoardTemp {
    /// What the input measures: "board", "cpu-socket", "cpu", "vrm", "pch", "pcie", "m2", "aux".
    pub key: &'static str,
    pub celsius: f32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BoardFan {
    /// The header: "cpu", "pump", "sys", "aux".
    pub key: &'static str,
    /// Stable number of the header (chip * 8 + input), so settings can name a fan.
    pub header: u8,
    pub rpm: u32,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Board {
    /// Chip name, e.g. "NCT6779D".
    pub chip: String,
    pub temps: Vec<BoardTemp>,
    pub fans: Vec<BoardFan>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// NCT6779D and later: banked registers, fan speeds already in RPM.
    Nuvoton { fans: usize },
    /// NCT6683D/6686D/6687D: paged EC space.
    NuvotonEc,
    Ite,
}

struct Chip {
    module: Module,
    kind: Kind,
    name: String,
    base: u16,
    /// Headers seen spinning: a fan that stops (zero-RPM mode) stays listed at 0.
    seen: [bool; 8],
    /// Nuvoton: temperature sources (bit n = source n) the firmware watches.
    sources: u32,
}

pub struct SuperIo {
    chips: Vec<Chip>,
    mutex: HANDLE,
}

// Used from the sampling thread only.
unsafe impl Send for SuperIo {}

/// Holds the ISA bus mutex while alive.
struct BusLock(HANDLE);

impl BusLock {
    fn acquire(mutex: HANDLE) -> Option<Self> {
        let r = unsafe { WaitForSingleObject(mutex, 100) };
        (r == WAIT_OBJECT_0 || r == WAIT_ABANDONED).then_some(Self(mutex))
    }
}

impl Drop for BusLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
        }
    }
}

/// Port and Super I/O access through LpcIO.
trait Lpc {
    fn outb(&self, port: u16, value: u8);
    fn inb(&self, port: u16) -> u8;
    fn sio_in(&self, reg: u8) -> u8;
    fn sio_out(&self, reg: u8, value: u8);
}

impl Lpc for Module {
    fn outb(&self, port: u16, value: u8) {
        let _ = self.call::<0>(b"ioctl_pio_outb\0", &[port as u64, value as u64]);
    }
    fn inb(&self, port: u16) -> u8 {
        self.call::<1>(b"ioctl_pio_inb\0", &[port as u64]).map_or(0xFF, |v| v[0] as u8)
    }
    fn sio_in(&self, reg: u8) -> u8 {
        self.call::<1>(b"ioctl_superio_inb\0", &[reg as u64]).map_or(0xFF, |v| v[0] as u8)
    }
    fn sio_out(&self, reg: u8, value: u8) {
        let _ = self.call::<0>(b"ioctl_superio_outb\0", &[reg as u64, value as u64]);
    }
}

const NUVOTON_HWM_LDN: u8 = 0x0B;
const ITE_EC_LDN: u8 = 0x04;
const NUVOTON_IO_SPACE_LOCK: u8 = 0x28;

/// Nuvoton chip by ID and revision (registers 0x20, 0x21).
fn nuvoton(id: u8, rev: u8) -> Option<(Kind, &'static str)> {
    Some(match (id, rev) {
        (0xC5, _) => (Kind::Nuvoton { fans: 5 }, "NCT6779D"),
        (0xC8, _) => (Kind::Nuvoton { fans: 6 }, "NCT6791D"),
        (0xC9, _) => (Kind::Nuvoton { fans: 7 }, "NCT6792D"),
        (0xD1, _) => (Kind::Nuvoton { fans: 7 }, "NCT6793D"),
        (0xD3, _) => (Kind::Nuvoton { fans: 7 }, "NCT6795D"),
        (0xD4, 0x40 | 0x41) => (Kind::NuvotonEc, "NCT6686D"),
        (0xD4, 0x51) => (Kind::Nuvoton { fans: 7 }, "NCT6797D"),
        (0xD4, 0x2B) => (Kind::Nuvoton { fans: 7 }, "NCT6798D"),
        (0xD4, _) => (Kind::Nuvoton { fans: 7 }, "NCT6796D"),
        (0xD8, _) => (Kind::Nuvoton { fans: 7 }, "NCT6799D"),
        (0xC7, _) => (Kind::NuvotonEc, "NCT6683D"),
        (0xD5, _) => (Kind::NuvotonEc, "NCT6687D"),
        _ => return None,
    })
}

/// A hardware monitor base address, if it looks like one.
fn valid_base(base: u16) -> bool {
    base >= 0x100 && base & 0xF007 == 0
}

/// Looks for a supported chip on `slot` (0: ports 0x2E/0x2F, 1: 0x4E/0x4F) and, when
/// found, unlocks LpcIO's access to its hardware monitor ports.
fn probe(module: Module, slot: u64) -> Result<Chip, String> {
    let port: u16 = if slot == 0 { 0x2E } else { 0x4E };
    module.call::<0>(b"ioctl_select_slot\0", &[slot])?;
    let read_base = |m: &Module, ldn: u8| {
        m.sio_out(0x07, ldn);
        ((m.sio_in(0x60) as u16) << 8) | m.sio_in(0x61) as u16
    };

    // Nuvoton (and Winbond, Fintek) configuration mode.
    module.outb(port, 0x87);
    module.outb(port, 0x87);
    let (id, rev) = (module.sio_in(0x20), module.sio_in(0x21));
    if let Some((kind, name)) = nuvoton(id, rev) {
        let base = read_base(&module, NUVOTON_HWM_LDN);
        if matches!(kind, Kind::Nuvoton { .. }) && id >= 0xC8 {
            // NCT6791D and later can lock the monitor's I/O space; reading needs it open.
            let lock = module.sio_in(NUVOTON_IO_SPACE_LOCK);
            if lock & 0x10 != 0 {
                module.sio_out(NUVOTON_IO_SPACE_LOCK, lock & !0x10);
            }
        }
        let bars = module.call::<0>(b"ioctl_find_bars\0", &[]);
        module.outb(port, 0xAA);
        bars?;
        if !valid_base(base) {
            return Err(format!("{name}: no monitor address ({base:#x})"));
        }
        return Ok(Chip { module, kind, name: name.into(), base, seen: [false; 8], sources: 0 });
    }
    let other = (id != 0xFF && id != 0x00).then(|| format!("{id:02X}{rev:02X}"));
    module.outb(port, 0xAA);

    // ITE configuration mode.
    for b in [0x87, 0x01, 0x55, if slot == 0 { 0x55 } else { 0xAA }] {
        module.outb(port, b);
    }
    let chip_id = ((module.sio_in(0x20) as u16) << 8) | module.sio_in(0x21) as u16;
    let result = if matches!(chip_id >> 8, 0x86 | 0x87) {
        let base = read_base(&module, ITE_EC_LDN);
        let bars = module.call::<0>(b"ioctl_find_bars\0", &[]);
        Some((base, bars))
    } else {
        None
    };
    module.sio_out(0x02, 0x02);
    match result {
        Some((base, bars)) => {
            bars?;
            let name = format!("IT{chip_id:04X}");
            if !valid_base(base) {
                return Err(format!("{name}: no monitor address ({base:#x})"));
            }
            Ok(Chip { module, kind: Kind::Ite, name, base, seen: [false; 8], sources: 0 })
        }
        None => Err(match other {
            Some(id) => format!("unsupported chip {id} at {port:#x}"),
            None => format!("no chip at {port:#x}"),
        }),
    }
}

impl Chip {
    /// Nuvoton NCT6779D+: register `bank:reg` through the address/data ports.
    fn nct_read(&self, addr: u16) -> u8 {
        let m = &self.module;
        m.outb(self.base + 5, 0x4E);
        m.outb(self.base + 6, (addr >> 8) as u8);
        m.outb(self.base + 5, addr as u8);
        m.inb(self.base + 6)
    }

    /// Nuvoton NCT668x: paged EC space.
    fn ec_read(&self, addr: u16) -> u8 {
        let m = &self.module;
        m.outb(self.base + 4, 0xFF);
        m.outb(self.base + 4, (addr >> 8) as u8);
        m.outb(self.base + 5, addr as u8);
        m.inb(self.base + 6)
    }

    fn ite_read(&self, reg: u8) -> u8 {
        self.module.outb(self.base + 5, reg);
        self.module.inb(self.base + 6)
    }

    /// Nuvoton: which temperature sources the firmware reads, from the fan-control and
    /// monitoring channels' source selects. Auxiliary inputs nobody watches are usually
    /// unconnected pins that read garbage.
    fn watched_sources(&self) -> u32 {
        const SELECTS: [u16; 13] = [0x100, 0x200, 0x300, 0x800, 0x900, 0xA00, 0x621, 0x622, 0x623, 0x624, 0x625, 0x626, 0x627];
        SELECTS.iter().fold(0, |mask, reg| mask | 1 << (self.nct_read(*reg) & 0x1F))
    }

    fn read(&mut self) -> (Vec<BoardTemp>, Vec<(usize, &'static str, u32)>) {
        let mut temps = Vec::new();
        let mut fans = Vec::new();
        match self.kind {
            Kind::Nuvoton { fans: count } => {
                // Direct readings of each input (sources 1-6: SYSTIN, CPUTIN, AUXTIN0-3).
                const TEMPS: [(u16, &str); 6] = [(0x490, "board"), (0x491, "cpu-socket"), (0x492, "aux"), (0x493, "aux"), (0x494, "aux"), (0x495, "aux")];
                for (i, (reg, key)) in TEMPS.into_iter().enumerate() {
                    if key == "aux" && self.sources & 1 << (i + 1) == 0 {
                        continue;
                    }
                    temps.push(BoardTemp { key, celsius: self.nct_read(reg) as i8 as f32 });
                }
                const FANS: [u16; 7] = [0x4C0, 0x4C2, 0x4C4, 0x4C6, 0x4C8, 0x4CA, 0x4CE];
                const KEYS: [&str; 7] = ["sys", "cpu", "aux", "aux", "aux", "aux", "aux"];
                for i in 0..count {
                    let rpm = ((self.nct_read(FANS[i]) as u32) << 8) | self.nct_read(FANS[i] + 1) as u32;
                    fans.push((i, KEYS[i], rpm));
                }
            }
            Kind::NuvotonEc => {
                const TEMPS: [(u16, &str); 7] =
                    [(0x100, "cpu"), (0x102, "board"), (0x104, "vrm"), (0x106, "pch"), (0x108, "cpu-socket"), (0x10A, "pcie"), (0x10C, "m2")];
                for (reg, key) in TEMPS {
                    let whole = self.ec_read(reg) as i8 as f32;
                    let half = if self.ec_read(reg + 1) & 0x80 != 0 { 0.5 } else { 0.0 };
                    temps.push(BoardTemp { key, celsius: whole + half });
                }
                const KEYS: [&str; 8] = ["cpu", "pump", "sys", "sys", "sys", "sys", "sys", "sys"];
                for (i, key) in KEYS.iter().enumerate() {
                    let reg = 0x140 + 2 * i as u16;
                    let rpm = ((self.ec_read(reg) as u32) << 8) | self.ec_read(reg + 1) as u32;
                    fans.push((i, *key, rpm));
                }
            }
            Kind::Ite => {
                for reg in 0x29..=0x2B {
                    temps.push(BoardTemp { key: "board", celsius: self.ite_read(reg) as i8 as f32 });
                }
                // 16-bit tachometer counts: low byte, then the extension byte.
                const FANS: [(u8, u8); 5] = [(0x0D, 0x18), (0x0E, 0x19), (0x0F, 0x1A), (0x80, 0x81), (0x82, 0x83)];
                const KEYS: [&str; 5] = ["cpu", "sys", "sys", "sys", "sys"];
                for (i, (lo, hi)) in FANS.iter().enumerate() {
                    let count = self.ite_read(*lo) as u32 | (self.ite_read(*hi) as u32) << 8;
                    let rpm = if count > 0x3F && count < 0xFFFF { 1_350_000 / (2 * count) } else { 0 };
                    fans.push((i, KEYS[i], rpm));
                }
            }
        }
        temps.retain(|t| (MIN_C..=MAX_C).contains(&t.celsius));
        (temps, fans)
    }
}

impl SuperIo {
    pub fn open(lib: &Lib) -> Result<Self, String> {
        let mutex = unsafe { CreateMutexW(None, false, w!(r"Global\Access_ISABUS.HTP.Method")) }.map_err(|e| format!("ISA bus mutex: {e}"))?;
        let mut chips = Vec::new();
        let mut errors = Vec::new();
        {
            let Some(_lock) = BusLock::acquire(mutex) else {
                unsafe {
                    let _ = CloseHandle(mutex);
                }
                return Err("ISA bus busy".into());
            };
            for slot in 0..2 {
                // Each chip keeps its own module instance: LpcIO remembers one chip.
                match lib.module(LPC_IO).and_then(|m| probe(m, slot)) {
                    Ok(mut chip) => {
                        if matches!(chip.kind, Kind::Nuvoton { .. }) {
                            chip.sources = chip.watched_sources();
                        }
                        chips.push(chip)
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
        if chips.is_empty() {
            unsafe {
                let _ = CloseHandle(mutex);
            }
            return Err(errors.join("; "));
        }
        Ok(Self { chips, mutex })
    }

    /// One reading of every chip; `None` if another tool holds the bus right now.
    pub fn read(&mut self) -> Option<Board> {
        let _lock = BusLock::acquire(self.mutex)?;
        let mut board = Board { chip: self.chips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" + "), ..Default::default() };
        for (c, chip) in self.chips.iter_mut().enumerate() {
            let (temps, fans) = chip.read();
            board.temps.extend(temps);
            for (i, key, rpm) in fans {
                let rpm = if (MIN_RPM..MAX_RPM).contains(&rpm) { rpm } else { 0 };
                chip.seen[i] |= rpm > 0;
                if chip.seen[i] {
                    board.fans.push(BoardFan { key, header: (c * 8 + i) as u8, rpm });
                }
            }
        }
        Some(board)
    }
}

impl Drop for SuperIo {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.mutex);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_nuvoton_chips() {
        assert_eq!(nuvoton(0xC5, 0x62), Some((Kind::Nuvoton { fans: 5 }, "NCT6779D")));
        assert_eq!(nuvoton(0xD5, 0x92), Some((Kind::NuvotonEc, "NCT6687D")));
        assert_eq!(nuvoton(0xD4, 0x2B).map(|k| k.1), Some("NCT6798D"));
        assert_eq!(nuvoton(0xD4, 0x41).map(|k| k.0), Some(Kind::NuvotonEc));
        assert_eq!(nuvoton(0xB4, 0x70), None);
    }

    #[test]
    fn base_addresses() {
        assert!(valid_base(0x290));
        assert!(valid_base(0xA20));
        assert!(!valid_base(0x0000));
        assert!(!valid_base(0xFFFF));
        assert!(!valid_base(0x295));
    }
}
