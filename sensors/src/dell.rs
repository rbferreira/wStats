//! Dell's "Data Vault" WMI interface (DDVWmiMethodFunction in root\WMI), the one Dell's
//! own tools use: fan speeds and the board's thermal sensors, no driver needed. Calling
//! its methods takes admin rights. Layouts as documented by Linux's dell-wmi-ddv.

use serde::Serialize;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoSetProxyBlanket, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayAccessData, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VT_ARRAY, VT_BSTR, VT_I4, VT_UI1, VariantClear};
use windows::Win32::System::Wmi::IEnumWbemClassObject;
use windows::Win32::System::Wmi::{IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WbemLocator};
use windows::core::{BSTR, HSTRING, PCWSTR};

const CLASS: &str = "DDVWmiMethodFunction";
/// How long a query may take. Never forever: these run on the thread that publishes
/// every reading and answers the service manager's stop.
const WMI_WAIT_MS: i32 = 5000;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Fan {
    /// Dell's fan type: 0 CPU, 1 chassis, 2 video, 3 power supply, 4 chipset, ...
    pub kind: u8,
    pub rpm: u16,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Thermal {
    /// Dell's sensor type: 0x00 CPU, 0x11 video, 0x22 memory, 0x44 ambient, ...
    pub kind: u8,
    pub celsius: i8,
}

/// Dell's BIOS thermal mode ("ThermalManagement"): Optimized, Cool, Quiet,
/// UltraPerformance, ... as offered by this machine.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ThermalMode {
    pub current: String,
    pub options: Vec<String>,
}

/// Reads the BIOS thermal mode through Dell's BIOS attribute WMI (admin only).
pub fn thermal_mode() -> Result<ThermalMode, String> {
    let e = |err: windows::core::Error| format!("BIOS: {err}");
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(e)?;
        let svc = locator
            .ConnectServer(&BSTR::from(r"ROOT\dcim\sysman\biosattributes"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
            .map_err(e)?;
        CoSetProxyBlanket(&svc, RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, None, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE).map_err(e)?;
        let query = BSTR::from("SELECT CurrentValue, PossibleValue FROM EnumerationAttribute WHERE AttributeName = 'ThermalManagement'");
        let rows: IEnumWbemClassObject =
            svc.ExecQuery(&BSTR::from("WQL"), &query, WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None).map_err(e)?;
        let mut found = [None];
        let mut n = 0;
        let _ = rows.Next(WMI_WAIT_MS, &mut found, &mut n);
        let row = found[0].take().ok_or("BIOS: no ThermalManagement attribute")?;
        let current = strings_prop(&row, "CurrentValue").into_iter().next().ok_or("BIOS: no current value")?;
        Ok(ThermalMode { current, options: strings_prop(&row, "PossibleValue") })
    }
}

pub struct Ddv {
    svc: IWbemServices,
    class: IWbemClassObject,
    path: BSTR,
}

impl Ddv {
    /// Needs COM initialised (and its security set up) on the calling thread.
    pub fn open() -> Result<Self, String> {
        let e = |what: &'static str| move |err: windows::core::Error| format!("DDV {what}: {err}");
        unsafe {
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(e("locator"))?;
            let svc = locator
                .ConnectServer(&BSTR::from(r"ROOT\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)
                .map_err(e("connect"))?;
            CoSetProxyBlanket(&svc, RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, None, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE)
                .map_err(e("proxy"))?;

            let query = BSTR::from(format!("SELECT __PATH FROM {CLASS}"));
            let rows = svc
                .ExecQuery(&BSTR::from("WQL"), &query, WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY, None)
                .map_err(e("query"))?;
            let mut found = [None];
            let mut n = 0;
            let _ = rows.Next(WMI_WAIT_MS, &mut found, &mut n);
            let instance = found[0].take().ok_or("DDV: not present on this machine")?;
            let path = string_prop(&instance, "__PATH").ok_or("DDV: no instance path")?;

            let mut class = None;
            svc.GetObject(&BSTR::from(CLASS), Default::default(), None, Some(&mut class), None).map_err(e("class"))?;
            Ok(Self { svc, class: class.ok_or("DDV: no class")?, path })
        }
    }

    /// Calls a DDV method that returns a byte buffer.
    fn buffer(&self, method: &str) -> Result<Vec<u8>, String> {
        let e = |err: windows::core::Error| format!("DDV {method}: {err}");
        let name = HSTRING::from(method);
        unsafe {
            let mut in_def = None;
            self.class.GetMethod(PCWSTR(name.as_ptr()), 0, &mut in_def, std::ptr::null_mut()).map_err(e)?;
            let input = in_def.ok_or("DDV: no input definition")?.SpawnInstance(0).map_err(e)?;
            let mut arg_inner = VARIANT_0_0 { vt: VT_I4, ..Default::default() };
            arg_inner.Anonymous.lVal = 0;
            let arg = VARIANT { Anonymous: VARIANT_0 { Anonymous: std::mem::ManuallyDrop::new(arg_inner) } };
            input.Put(windows::core::w!("arg2"), 0, &arg, 0).map_err(e)?;
            let mut output = None;
            self.svc.ExecMethod(&self.path, &BSTR::from(method), Default::default(), None, &input, Some(&mut output), None).map_err(e)?;
            let output = output.ok_or("DDV: no output")?;
            let mut v = VARIANT::default();
            output.Get(windows::core::w!("RawData"), 0, &mut v, None, None).map_err(e)?;
            let bytes = bytes_of(&v);
            let _ = VariantClear(&mut v);
            bytes.ok_or_else(|| format!("DDV {method}: no data"))
        }
    }

    pub fn fans(&self) -> Result<Vec<Fan>, String> {
        Ok(parse_fans(&self.buffer("FanSensorInformation")?))
    }

    pub fn thermals(&self) -> Result<Vec<Thermal>, String> {
        Ok(parse_thermals(&self.buffer("ThermalSensorInformation")?))
    }
}

/// Fan entries are 3 bytes (type, RPM as u16 LE), the list ends with 0xFF.
pub fn parse_fans(raw: &[u8]) -> Vec<Fan> {
    raw.chunks_exact(3)
        .take_while(|c| c[0] != 0xFF)
        .map(|c| Fan { kind: c[0], rpm: u16::from_le_bytes([c[1], c[2]]) })
        .collect()
}

/// Thermal entries are 5 bytes (type, current, min, max, unknown), then a 0xFF byte.
pub fn parse_thermals(raw: &[u8]) -> Vec<Thermal> {
    let entries = raw.len().saturating_sub(1) / 5;
    raw.chunks_exact(5).take(entries).map(|c| Thermal { kind: c[0], celsius: c[1] as i8 }).collect()
}

unsafe fn string_prop(obj: &IWbemClassObject, name: &str) -> Option<BSTR> {
    let name = HSTRING::from(name);
    let mut v = VARIANT::default();
    unsafe {
        obj.Get(PCWSTR(name.as_ptr()), 0, &mut v, None, None).ok()?;
    }
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        let s = (inner.vt == VT_BSTR).then(|| (*inner.Anonymous.bstrVal).clone());
        let _ = VariantClear(&mut v);
        s
    }
}

/// A string or string-array property, as a list.
unsafe fn strings_prop(obj: &IWbemClassObject, name: &str) -> Vec<String> {
    let name = HSTRING::from(name);
    let mut v = VARIANT::default();
    unsafe {
        if obj.Get(PCWSTR(name.as_ptr()), 0, &mut v, None, None).is_err() {
            return Vec::new();
        }
        let inner = &v.Anonymous.Anonymous;
        let mut out = Vec::new();
        if inner.vt == VT_BSTR {
            out.push((*inner.Anonymous.bstrVal).to_string());
        } else if inner.vt.0 == (VT_ARRAY.0 | VT_BSTR.0) {
            let array = inner.Anonymous.parray as *const SAFEARRAY;
            if let (Ok(lo), Ok(hi)) = (SafeArrayGetLBound(array, 1), SafeArrayGetUBound(array, 1)) {
                let mut data = std::ptr::null_mut();
                if SafeArrayAccessData(array, &mut data).is_ok() {
                    let items = std::slice::from_raw_parts(data as *const BSTR, (hi - lo + 1).max(0) as usize);
                    out.extend(items.iter().map(|b| b.to_string()));
                    let _ = SafeArrayUnaccessData(array);
                }
            }
        }
        let _ = VariantClear(&mut v);
        out
    }
}

/// The bytes of a `VT_ARRAY | VT_UI1` variant.
unsafe fn bytes_of(v: &VARIANT) -> Option<Vec<u8>> {
    unsafe {
        let raw = &v.Anonymous.Anonymous;
        if raw.vt.0 != (VT_ARRAY.0 | VT_UI1.0) {
            return None;
        }
        let array = raw.Anonymous.parray as *const SAFEARRAY;
        let lo = SafeArrayGetLBound(array, 1).ok()?;
        let hi = SafeArrayGetUBound(array, 1).ok()?;
        let len = (hi - lo + 1).max(0) as usize;
        let mut data = std::ptr::null_mut();
        SafeArrayAccessData(array, &mut data).ok()?;
        let bytes = std::slice::from_raw_parts(data as *const u8, len).to_vec();
        let _ = SafeArrayUnaccessData(array);
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_this_machines_buffers() {
        // Dell Pro 14 Premium PA14250, captured with the probe.
        let fans = parse_fans(&[0x00, 0xbf, 0x0e, 0x02, 0xb1, 0x0e, 0xff]);
        assert_eq!(fans, vec![Fan { kind: 0, rpm: 3775 }, Fan { kind: 2, rpm: 3761 }]);
        let raw = [
            0x00, 0x39, 0, 0, 0, 0x22, 0x30, 0, 0, 0, 0x33, 0x2d, 0, 0, 0, 0x22, 0x23, 0, 0, 0, 0x33, 0x25, 0, 0, 0, 0x44, 0x25, 0, 0, 0, 0x44, 0x20, 0, 0, 0,
            0xff, 0x21, 0, 0, 0, 0xff,
        ];
        let t = parse_thermals(&raw);
        assert_eq!(t.len(), 8);
        assert_eq!(t[0], Thermal { kind: 0, celsius: 57 });
        assert_eq!(t[7], Thermal { kind: 0xff, celsius: 33 });
    }
}
