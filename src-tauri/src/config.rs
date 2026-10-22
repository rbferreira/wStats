//! User settings, persisted as JSON in the app config directory.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ModuleId {
    Cpu,
    Gpu,
    Mem,
    Net,
    Disk,
    Battery,
    /// Was "sensors" before fans and power became items of their own.
    #[serde(alias = "sensors")]
    Temp,
    Fans,
    Power,
    Npu,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Auto,
    Dark,
    Light,
}

/// Interface language; `Auto` follows Windows' display language.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Auto,
    Pt,
    En,
}

/// Where each strip item's name (CPU, MEM, ...) goes relative to its value.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Label {
    /// Name over the value.
    #[default]
    Top,
    /// Value over the name.
    Bottom,
    /// Name in stacked letters left of the graphic.
    Side,
    /// No name; the value is drawn larger.
    None,
}

/// What identifies each number in the compact mode.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Tag {
    /// The settings window's line icon.
    #[default]
    Icon,
    /// The item's name (CPU, MEM, ...).
    Label,
}

/// Which side of the number an item's name goes: the compact mode's tag, and the full
/// mode's stacked letters (label "beside").
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Left,
    Right,
}

/// How the strip draws its items: what the label, graphics and compact settings decide.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Look {
    pub label: Label,
    /// The graphic left of each item's text (bar graph, ring, thermometer, ...).
    pub graphics: bool,
    /// Two items per column, numbers only, each with a small tag.
    pub compact: bool,
    pub tag: Tag,
    pub side: Side,
}

impl Default for Look {
    fn default() -> Self {
        Self { label: Label::Top, graphics: true, compact: false, tag: Tag::Icon, side: Side::Left }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    /// Between the Widgets button and Start: part of the bar's left group.
    Left,
    /// Next to the notification area, like TrafficMonitor.
    Right,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub position: Position,
    /// GDI device name of the monitor whose taskbar hosts the strip; `None` = primary.
    pub monitor: Option<String>,
    /// Strip items, left to right. A module missing here is hidden.
    pub modules: Vec<ModuleId>,
    pub interval_ms: u32,
    pub theme: Theme,
    pub ping_host: String,
    pub colors: Colors,
    /// Close the popup's webview after a minute unused, at the cost of a slower next open.
    pub save_memory: bool,
    pub language: Language,
    pub label: Label,
    /// Draw each item's graphic; off leaves the text alone.
    pub graphics: bool,
    /// Compact mode: two items per column, numbers only, tagged by an icon or the name.
    pub compact: bool,
    pub compact_tag: Tag,
    pub side: Side,
    /// The NPU item was added for this machine once; turning it off afterwards sticks.
    pub npu_offered: bool,
    /// Fans (reading ids) left out of the panel and the taskbar.
    pub hidden_fans: Vec<String>,
    /// The fan the taskbar shows; `None` = the fastest.
    pub bar_fan: Option<String>,
}

/// The two colours every chart is built from, as `#RRGGBB`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Colors {
    pub primary: String,
    pub secondary: String,
}

impl Default for Colors {
    fn default() -> Self {
        Self { primary: "#3478F6".into(), secondary: "#78C04F".into() }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            position: Position::Left,
            monitor: None,
            modules: vec![ModuleId::Cpu, ModuleId::Mem, ModuleId::Disk, ModuleId::Net, ModuleId::Battery],
            interval_ms: 1000,
            theme: Theme::Auto,
            ping_host: "1.1.1.1".into(),
            colors: Colors::default(),
            save_memory: false,
            language: Language::Auto,
            label: Label::Top,
            graphics: true,
            compact: false,
            compact_tag: Tag::Icon,
            side: Side::Left,
            npu_offered: false,
            hidden_fans: Vec::new(),
            bar_fan: None,
        }
    }
}

impl Settings {
    pub fn look(&self) -> Look {
        Look { label: self.label, graphics: self.graphics, compact: self.compact, tag: self.compact_tag, side: self.side }
    }

    /// Shows the NPU on machines that have one, the first time wStats sees it. Returns
    /// whether the settings changed.
    pub fn offer_npu(&mut self, present: bool) -> bool {
        if self.npu_offered || !present {
            return false;
        }
        if !self.modules.contains(&ModuleId::Npu) {
            self.modules.push(ModuleId::Npu);
        }
        self.npu_offered = true;
        true
    }

    pub fn load(path: &PathBuf) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &PathBuf) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write-then-rename so a crash never leaves a half-written file behind.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_settings_keep_graphics_and_stay_full() {
        let s: Settings = serde_json::from_str(r#"{"label":"bottom"}"#).unwrap();
        assert_eq!(s.look(), Look { label: Label::Bottom, ..Look::default() });
        let s: Settings = serde_json::from_str(r#"{"compact":true,"compactTag":"label","side":"right"}"#).unwrap();
        assert!(s.look().compact && s.look().tag == Tag::Label && s.look().side == Side::Right);
    }

    #[test]
    fn npu_is_offered_once() {
        let mut s = Settings::default();
        assert!(!s.offer_npu(false));
        assert!(!s.modules.contains(&ModuleId::Npu));
        assert!(s.offer_npu(true));
        assert_eq!(s.modules.last(), Some(&ModuleId::Npu));
        // Turned off by the user: stays off.
        s.modules.retain(|m| *m != ModuleId::Npu);
        assert!(!s.offer_npu(true));
        assert!(!s.modules.contains(&ModuleId::Npu));
    }

    #[test]
    fn old_sensors_item_becomes_temperature() {
        let s: Settings = serde_json::from_str(r#"{"modules":["cpu","sensors"]}"#).unwrap();
        assert_eq!(s.modules, vec![ModuleId::Cpu, ModuleId::Temp]);
    }

    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        let s: Settings = serde_json::from_str(r#"{"modules":["cpu","mem"],"future":1}"#).unwrap();
        assert_eq!(s.modules, vec![ModuleId::Cpu, ModuleId::Mem]);
        assert_eq!(s.interval_ms, 1000);
        assert_eq!(s.label, Label::Top);
    }

    #[test]
    fn label_round_trips() {
        let s: Settings = serde_json::from_str(r#"{"label":"none"}"#).unwrap();
        assert_eq!(s.label, Label::None);
        assert!(serde_json::to_string(&s).unwrap().contains(r#""label":"none""#));
    }
}
