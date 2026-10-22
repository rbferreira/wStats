//! Draws the strip into a 32-bit DIB with Direct2D, for `UpdateLayeredWindow`.
//!
//! All geometry is in device pixels; sizes are written in DIPs and multiplied by `s`
//! (the taskbar's DPI scale) so the strip matches the taskbar at 100%..200%.

use super::icons::{self, Seg, Shape};
use crate::config::{Label, Look, ModuleId, Side, Tag};
use crate::stats::Store;
use crate::stats::model::Snapshot;
use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::PI;
use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::{PCWSTR, Result, w};
use windows_numerics::Vector2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(f32, f32, f32, f32);

impl Rgba {
    pub const BLUE: Rgba = Rgba(0x34 as f32 / 255.0, 0x78 as f32 / 255.0, 0xF6 as f32 / 255.0, 1.0);
    pub const GREEN: Rgba = Rgba(0x78 as f32 / 255.0, 0xC0 as f32 / 255.0, 0x4F as f32 / 255.0, 1.0);

    /// `#3478F6` -> colour; `None` for anything else.
    pub fn from_hex(hex: &str) -> Option<Rgba> {
        let h = hex.strip_prefix('#')?;
        if h.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        let c = |shift: u32| ((v >> shift) & 0xFF) as f32 / 255.0;
        Some(Rgba(c(16), c(8), c(0), 1.0))
    }
    const RED: Rgba = Rgba(0xEF as f32 / 255.0, 0x53 as f32 / 255.0, 0x50 as f32 / 255.0, 1.0);
    const ORANGE: Rgba = Rgba(0xEF as f32 / 255.0, 0x82 as f32 / 255.0, 0x40 as f32 / 255.0, 1.0);
    const YELLOW: Rgba = Rgba(0xF5 as f32 / 255.0, 0xC5 as f32 / 255.0, 0x42 as f32 / 255.0, 1.0);

    fn alpha(self, a: f32) -> Rgba {
        Rgba(self.0, self.1, self.2, self.3 * a)
    }

    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
    }
}

/// Which part of the bar's height a slot takes: all of it, or a compact-mode row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Full,
    Top,
    Bottom,
}

/// One laid-out item: where it sits in the strip, in device pixels.
#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub module: ModuleId,
    pub x: i32,
    pub w: i32,
    pub row: Row,
    /// Compact mode: widths (px) of the column's tag and number parts, so the column's
    /// two cells line up.
    pub tag_w: f32,
    pub num_w: f32,
}

struct Fonts {
    caption: IDWriteTextFormat,
    value: IDWriteTextFormat,
    unit: IDWriteTextFormat,
    net_unit: IDWriteTextFormat,
    arrow: IDWriteTextFormat,
    line: IDWriteTextFormat,
    letter: IDWriteTextFormat,
    letter_small: IDWriteTextFormat,
    big: IDWriteTextFormat,
    big_unit: IDWriteTextFormat,
    big_line: IDWriteTextFormat,
    compact_value: IDWriteTextFormat,
    compact_unit: IDWriteTextFormat,
    compact_tag: IDWriteTextFormat,
}

/// Which of the [`Fonts`] a piece of text uses.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Font {
    Caption,
    Value,
    Unit,
    NetUnit,
    Arrow,
    Line,
    Big,
    BigUnit,
    BigLine,
    CompactValue,
    CompactUnit,
    CompactTag,
}

impl Fonts {
    fn new(dwrite: &IDWriteFactory, s: f32) -> Result<Fonts> {
        let mk = |family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT, stretch: DWRITE_FONT_STRETCH| unsafe {
            dwrite.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, stretch, size * s, w!("pt-br"))
        };
        // One family in two widths, both shipped with Windows 10 and 11: Bahnschrift for
        // text that shares the bar with a name line (above, below, compact), and its
        // condensed cut where the number fills the bar alone (beside, none), being taller
        // than it is wide, it grows without pushing the item much wider.
        let normal = |size, weight| mk(w!("Bahnschrift"), size, weight, DWRITE_FONT_STRETCH_NORMAL);
        let tall = |size| mk(w!("Bahnschrift"), size, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STRETCH_CONDENSED);
        let fonts = Fonts {
            caption: normal(10.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            value: normal(14.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            unit: normal(10.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            net_unit: normal(7.5, DWRITE_FONT_WEIGHT_MEDIUM)?,
            arrow: normal(11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            line: normal(11.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            letter: normal(8.0, DWRITE_FONT_WEIGHT_BOLD)?,
            letter_small: normal(7.5, DWRITE_FONT_WEIGHT_BOLD)?,
            big: tall(22.0)?,
            big_unit: tall(13.0)?,
            big_line: tall(14.5)?,
            compact_value: normal(13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            compact_unit: normal(8.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            compact_tag: normal(8.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
        };
        unsafe {
            for f in [
                &fonts.caption,
                &fonts.value,
                &fonts.unit,
                &fonts.net_unit,
                &fonts.arrow,
                &fonts.line,
                &fonts.letter,
                &fonts.letter_small,
                &fonts.big,
                &fonts.big_unit,
                &fonts.big_line,
                &fonts.compact_value,
                &fonts.compact_unit,
                &fonts.compact_tag,
            ] {
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            }
        }
        Ok(fonts)
    }

    fn get(&self, f: Font) -> &IDWriteTextFormat {
        match f {
            Font::Caption => &self.caption,
            Font::Value => &self.value,
            Font::Unit => &self.unit,
            Font::NetUnit => &self.net_unit,
            Font::Arrow => &self.arrow,
            Font::Line => &self.line,
            Font::Big => &self.big,
            Font::BigUnit => &self.big_unit,
            Font::BigLine => &self.big_line,
            Font::CompactValue => &self.compact_value,
            Font::CompactUnit => &self.compact_unit,
            Font::CompactTag => &self.compact_tag,
        }
    }
}

// ---- what an item says ---------------------------------------------------------------
//
// Every item is a graphic followed by its text. The text is a few lines, each placed by
// its baseline in a 48-DIP bar (the designs were drawn that way); the label setting only
// changes which lines there are and where. Widths come from measuring the same lines
// filled with the widest values an item can show, so items never change width as their
// numbers change.

/// An item's text: one value with its unit, or the network's two rates in one unit.
enum Reading {
    Value { value: String, unit: &'static str },
    Rates { up: String, down: String, unit: &'static str },
}

/// A piece of a line: space before it (DIPs), text, font, opacity.
struct Part {
    gap: f32,
    text: String,
    font: Font,
    alpha: f32,
}

fn part(gap: f32, text: impl Into<String>, font: Font, alpha: f32) -> Part {
    Part { gap, text: text.into(), font, alpha }
}

/// A line of text and its baseline, in DIPs from the top of a 48-DIP bar.
struct Line {
    baseline: f32,
    parts: Vec<Part>,
}

fn name(m: ModuleId) -> &'static str {
    match m {
        ModuleId::Cpu => "CPU",
        ModuleId::Gpu => "GPU",
        ModuleId::Mem => "MEM",
        ModuleId::Net => "NET",
        ModuleId::Disk => "SSD",
        ModuleId::Battery => "BAT",
        ModuleId::Temp => "TEMP",
        ModuleId::Fans => "FAN",
        ModuleId::Power => "PWR",
        ModuleId::Npu => "NPU",
    }
}

/// Width of the graphic left of the text, in DIPs.
fn graphic_width(m: ModuleId) -> f32 {
    match m {
        ModuleId::Cpu | ModuleId::Gpu | ModuleId::Net => 28.0,
        ModuleId::Mem => 27.0,
        ModuleId::Disk | ModuleId::Fans => 24.0,
        ModuleId::Battery => 13.0,
        ModuleId::Temp => 14.0,
        ModuleId::Power => 16.0,
        ModuleId::Npu => 30.0,
    }
}

/// The widest text each item can show; sizes the item.
fn widest(m: ModuleId) -> Reading {
    let value = |v: &str, unit| Reading::Value { value: v.into(), unit };
    match m {
        ModuleId::Cpu | ModuleId::Gpu | ModuleId::Mem | ModuleId::Disk | ModuleId::Battery | ModuleId::Npu => value("100", "%"),
        ModuleId::Temp => value("100°", ""),
        ModuleId::Fans => value("9999", ""),
        ModuleId::Power => value("999", "W"),
        ModuleId::Net => Reading::Rates { up: "99".into(), down: "99".into(), unit: "MB/s" },
    }
}

/// The lines of an item's text for a label setting.
fn lines(m: ModuleId, label: Label, reading: &Reading) -> Vec<Line> {
    // Without a name line above or below, the number grows to fill the bar.
    let big = matches!(label, Label::Side | Label::None);
    let value = || match reading {
        Reading::Value { value, unit } => {
            let mut v = vec![part(0.0, value.as_str(), if big { Font::Big } else { Font::Value }, 1.0)];
            if !unit.is_empty() {
                let gap = if *unit == "%" { 1.5 } else { 3.0 };
                v.push(part(gap, *unit, if big { Font::BigUnit } else { Font::Unit }, 0.7));
            }
            v
        }
        Reading::Rates { up, down, .. } => vec![
            part(0.0, "↑", Font::Arrow, 0.75),
            part(1.0, up.as_str(), Font::Value, 1.0),
            part(6.0, "↓", Font::Arrow, 0.75),
            part(1.0, down.as_str(), Font::Value, 1.0),
        ],
    };
    // The network's unit rides on the name line, small: the rates below are bare numbers.
    let caption = || {
        let mut v = vec![part(0.0, name(m), Font::Caption, 0.62)];
        if let Reading::Rates { unit, .. } = reading {
            v.push(part(3.0, *unit, Font::NetUnit, 0.75));
        }
        v
    };
    let line = |baseline, parts| Line { baseline, parts };
    match (label, reading) {
        (Label::Top, _) => vec![line(20.0, caption()), line(37.0, value())],
        (Label::Bottom, _) => vec![line(25.0, value()), line(38.0, caption())],
        // No name line: each rate gets a line of its own, with its unit.
        (Label::Side | Label::None, Reading::Rates { up, down, unit }) => {
            let (font, first, second) = if big { (Font::BigLine, 21.5, 37.5) } else { (Font::Line, 20.0, 35.0) };
            vec![
                line(first, vec![part(0.0, format!("↑ {up} {unit}"), font, 1.0)]),
                line(second, vec![part(0.0, format!("↓ {down} {unit}"), font, 1.0)]),
            ]
        }
        (Label::Side | Label::None, _) => vec![line(32.0, value())],
    }
}

/// Lays out one piece of text (no wrapping): the layout, its width and its baseline (px).
fn text_layout(dwrite: &IDWriteFactory, fonts: &Fonts, p: &Part) -> Option<(IDWriteTextLayout, f32, f32)> {
    unsafe {
        let layout = dwrite.CreateTextLayout(&wide(&p.text), fonts.get(p.font), 4096.0, 4096.0).ok()?;
        let _ = layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m).ok()?;
        let mut lm = [DWRITE_LINE_METRICS::default()];
        let mut count = 0;
        layout.GetLineMetrics(Some(&mut lm), &mut count).ok()?;
        Some((layout, m.widthIncludingTrailingWhitespace, lm[0].baseline))
    }
}

fn line_width(dwrite: &IDWriteFactory, fonts: &Fonts, line: &Line, s: f32) -> f32 {
    line.parts.iter().map(|p| p.gap * s + text_layout(dwrite, fonts, p).map_or(0.0, |(_, w, _)| w)).sum()
}

thread_local! {
    /// Fonts for measuring (layout runs outside any renderer) and the widths found.
    static MEASURE: RefCell<Option<(f32, IDWriteFactory, Fonts)>> = const { RefCell::new(None) };
    static WIDTHS: RefCell<HashMap<(ModuleId, u8, u32), f32>> = RefCell::new(HashMap::new());
}

/// Runs `f` with the measuring fonts for scale `s`.
fn measure<T>(s: f32, f: impl FnOnce(&IDWriteFactory, &Fonts) -> T) -> Option<T> {
    MEASURE.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.as_ref().is_none_or(|(k, _, _)| *k != s) {
            let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.ok()?;
            let fonts = Fonts::new(&dwrite, s).ok()?;
            *cell = Some((s, dwrite, fonts));
        }
        let (_, dwrite, fonts) = cell.as_ref()?;
        Some(f(dwrite, fonts))
    })
}

/// Width of an item's content (stacked letters, graphic, text), in device pixels.
fn content_width(m: ModuleId, look: Look, s: f32) -> f32 {
    let key = (m, look.label as u8 | (look.graphics as u8) << 4, s.to_bits());
    if let Some(w) = WIDTHS.with(|c| c.borrow().get(&key).copied()) {
        return w;
    }
    let text = measure(s, |dwrite, fonts| lines(m, look.label, &widest(m)).iter().map(|l| line_width(dwrite, fonts, l, s)).fold(0.0, f32::max));
    let letters = if look.label == Label::Side { 12.0 } else { 0.0 };
    let graphic = if look.graphics { graphic_width(m) + 6.0 } else { 0.0 };
    let w = (letters + graphic + 1.0) * s + text.unwrap_or(40.0 * s);
    WIDTHS.with(|c| c.borrow_mut().insert(key, w));
    w
}

// ---- compact mode ----------------------------------------------------------------------
//
// Two items per column, numbers only, each tagged by its icon or its name on one side.
// The network keeps a column of its own: its two rates are two lines already.

/// DIPs between a cell's tag and its number; the icon's size; baselines (in the 48-DIP
/// bar) of the top row, the bottom row and a cell alone in its column.
const TAG_GAP: f32 = 4.0;
const ICON: f32 = 12.0;
const TOP_BASE: f32 = 20.5;
const BOTTOM_BASE: f32 = 37.5;
const FULL_BASE: f32 = 29.5;

fn columns(modules: &[ModuleId]) -> Vec<Vec<ModuleId>> {
    let mut cols: Vec<Vec<ModuleId>> = Vec::new();
    for &m in modules {
        match cols.last_mut() {
            Some(c) if c.len() == 1 && c[0] != ModuleId::Net && m != ModuleId::Net => c.push(m),
            _ => cols.push(vec![m]),
        }
    }
    cols
}

/// A compact number: the value and its unit, smaller.
fn compact_value(value: String, unit: &str) -> Vec<Part> {
    let mut v = vec![part(0.0, value, Font::CompactValue, 1.0)];
    if !unit.is_empty() {
        v.push(part(if unit == "%" { 1.5 } else { 2.5 }, unit.to_string(), Font::CompactUnit, 0.7));
    }
    v
}

/// Widths (px) of a compact cell's tag and of its widest number.
fn compact_widths(m: ModuleId, tag: Tag, s: f32) -> (f32, f32) {
    measure(s, |dwrite, fonts| {
        let w = |parts: Vec<Part>| line_width(dwrite, fonts, &Line { baseline: 0.0, parts }, s);
        let tag_w = match (m, tag) {
            (ModuleId::Net, _) => w(vec![part(0.0, "↑", Font::Arrow, 1.0)]),
            (_, Tag::Icon) => ICON * s,
            (_, Tag::Label) => w(vec![part(0.0, name(m), Font::CompactTag, 1.0)]),
        };
        let num_w = match widest(m) {
            Reading::Value { value, unit } => w(compact_value(value, unit)),
            Reading::Rates { up, unit, .. } => w(compact_value(up, unit)),
        };
        (tag_w, num_w)
    })
    .unwrap_or((ICON * s, 40.0 * s))
}

fn compact_layout(modules: &[ModuleId], tag: Tag, scale: f32) -> (Vec<Slot>, i32) {
    let mut x = 0.0;
    let mut slots = Vec::new();
    for (i, col) in columns(modules).iter().enumerate() {
        if i > 0 {
            x += ITEM_GAP * scale;
        }
        let (tag_w, num_w) = col.iter().map(|&m| compact_widths(m, tag, scale)).fold((0.0f32, 0.0f32), |(a, b), (t, n)| (a.max(t), b.max(n)));
        let w = (tag_w + TAG_GAP * scale + num_w + 2.0 * ITEM_PAD * scale).round();
        for (k, &m) in col.iter().enumerate() {
            let row = match (col.len(), k) {
                (1, _) => Row::Full,
                (_, 0) => Row::Top,
                _ => Row::Bottom,
            };
            slots.push(Slot { module: m, x: x.round() as i32, w: w as i32, row, tag_w, num_w });
        }
        x += w;
    }
    (slots, x.round() as i32)
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    target: ID2D1DCRenderTarget,
    round: ID2D1StrokeStyle,
    fonts: Option<(f32, Fonts)>,
    /// Memory DC, bitmap, previously selected object, width, height, pixel pointer.
    dib: Option<(HDC, HBITMAP, HGDIOBJ, i32, i32, *mut u8)>,
    decimal: char,
}

pub const ITEM_PAD: f32 = 5.0;
pub const ITEM_GAP: f32 = 2.0;

pub fn layout(modules: &[ModuleId], look: Look, scale: f32) -> (Vec<Slot>, i32) {
    if look.compact {
        return compact_layout(modules, look.tag, scale);
    }
    let mut x = 0.0;
    let mut slots = Vec::new();
    for (i, &m) in modules.iter().enumerate() {
        if i > 0 {
            x += ITEM_GAP * scale;
        }
        let w = (content_width(m, look, scale) + 2.0 * ITEM_PAD * scale).round();
        slots.push(Slot { module: m, x: x.round() as i32, w: w as i32, row: Row::Full, tag_w: 0.0, num_w: 0.0 });
        x += w;
    }
    (slots, x.round() as i32)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                // Software: the strip is a few hundred pixels redrawn once a second; a GPU
                // device (and the graphics driver it loads) would cost tens of MB for nothing.
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&props)?;
            let round = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_FLAT,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            Ok(Self { factory, dwrite, target, round, fonts: None, dib: None, decimal: decimal_separator() })
        }
    }

    fn fonts(&mut self, s: f32) -> Result<&Fonts> {
        if self.fonts.as_ref().is_none_or(|(k, _)| *k != s) {
            self.fonts = Some((s, Fonts::new(&self.dwrite, s)?));
        }
        Ok(&self.fonts.as_ref().unwrap().1)
    }

    /// (Re)creates the backing DIB when the strip size changes.
    fn surface(&mut self, w: i32, h: i32) -> Result<HDC> {
        if let Some((dc, _, _, cw, ch, _)) = self.dib {
            if cw == w && ch == h {
                return Ok(dc);
            }
        }
        self.free_surface();
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(dc, bmp.into());
            self.dib = Some((dc, bmp, old, w, h, bits.cast()));
            Ok(dc)
        }
    }

    fn free_surface(&mut self) {
        if let Some((dc, bmp, old, _, _, _)) = self.dib.take() {
            unsafe {
                SelectObject(dc, old);
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(dc);
            }
        }
    }

    /// Pixels of the last frame: BGRA, premultiplied, top-down.
    pub fn pixels(&self) -> Option<(&[u8], u32, u32)> {
        let (_, _, _, w, h, bits) = self.dib?;
        let _ = unsafe { GdiFlush() };
        Some((unsafe { std::slice::from_raw_parts(bits, (w * h * 4) as usize) }, w as u32, h as u32))
    }

    /// Pushes the last frame to a layered window.
    pub fn present_layered(&self, hwnd: HWND) -> Result<()> {
        let Some((dc, _, _, w, h, _)) = self.dib else { return Ok(()) };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        unsafe { UpdateLayeredWindow(hwnd, None, None, Some(&size), Some(dc), Some(&src), Default::default(), Some(&blend), ULW_ALPHA) }
    }

    /// Renders every slot into the backing bitmap.
    pub fn paint(&mut self, frame: &Frame, store: &Store) -> Result<()> {
        let (w, h) = (frame.width.max(1), frame.height.max(1));
        let dc = self.surface(w, h)?;
        let s = frame.scale;
        self.fonts(s)?;
        let rect = RECT { left: 0, top: 0, right: w, bottom: h };
        unsafe {
            self.target.BindDC(dc, &rect)?;
            self.target.BeginDraw();
            self.target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            // Fully transparent pixels are click-through on a layered window; alpha 1/255 is
            // invisible but keeps the whole strip clickable.
            self.target.Clear(Some(&Rgba(0.0, 0.0, 0.0, 1.0 / 255.0).d2d()));
            let fg = if frame.light { Rgba(0.0, 0.0, 0.0, 0.9) } else { Rgba(1.0, 1.0, 1.0, 1.0) };
            let brush = self.target.CreateSolidColorBrush(&fg.d2d(), None)?;
            // Designs are drawn for a 48-DIP bar; a taller or shorter one centres them.
            let oy = (h as f32 / s - 48.0) / 2.0;
            let ctx = Ctx { r: self, brush: &brush, fg, s, h: h as f32, oy, look: frame.look, primary: frame.primary, secondary: frame.secondary };
            for (i, slot) in frame.slots.iter().enumerate() {
                let x = slot.x as f32;
                let pill = if frame.active == Some(slot.module) {
                    Some(0.16)
                } else if frame.hover == Some(i) {
                    Some(0.08)
                } else {
                    None
                };
                if let Some(a) = pill {
                    // A compact row gets the half of the bar it is drawn in.
                    let (inset, mid) = (4.0 * s, h as f32 / 2.0);
                    let (top, bottom) = match slot.row {
                        Row::Full => (inset, h as f32 - inset),
                        Row::Top => (inset / 2.0, mid),
                        Row::Bottom => (mid, h as f32 - inset / 2.0),
                    };
                    ctx.round_rect(x, top, slot.w as f32, bottom - top, 5.0 * s, fg.alpha(a));
                }
                let left = x + ITEM_PAD * s;
                let width = slot.w as f32 - 2.0 * ITEM_PAD * s;
                if frame.look.compact {
                    ctx.compact(slot, left, &store.snapshot);
                } else {
                    ctx.item(slot.module, left, width, &store.snapshot, &store.history);
                }
            }
            self.target.EndDraw(None, None)?;
        }
        Ok(())
    }

    /// A number with the user's decimal separator.
    fn number(&self, v: f32, decimals: usize) -> String {
        let s = if v >= 10.0 { format!("{v:.0}") } else { format!("{v:.decimals$}") };
        s.replace('.', &self.decimal.to_string())
    }

    /// Both network rates in one unit, the larger one's, so they read as a pair. Each
    /// number keeps to two digits ("99", "9,9"): past 99 the unit steps up, which keeps
    /// the item narrow; the popup has the exact figures.
    fn rates(&self, up: f64, down: f64) -> (String, String, &'static str) {
        let kb = up.max(down) / 1024.0;
        let (div, unit) = if kb >= 99.5 * 1024.0 {
            (1024.0f64.powi(3), "GB/s")
        } else if kb >= 99.5 {
            (1024.0 * 1024.0, "MB/s")
        } else {
            (1024.0, "KB/s")
        };
        // Whole kilobytes; from MB/s up, one decimal below 10 ("0,4" next to "2,3").
        let f = |v: f64| {
            let v = (v / div) as f32;
            if unit == "KB/s" || v >= 9.95 {
                format!("{}", v.round() as i64)
            } else if v < 0.05 {
                "0".to_string()
            } else {
                self.number(v, 1)
            }
        };
        (f(up), f(down), unit)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.free_surface();
    }
}

pub struct Frame {
    pub slots: Vec<Slot>,
    pub width: i32,
    pub height: i32,
    pub scale: f32,
    pub light: bool,
    pub hover: Option<usize>,
    pub active: Option<ModuleId>,
    /// User, download, write, ... (the primary colour).
    pub primary: Rgba,
    /// System, upload, read, ... (the secondary colour).
    pub secondary: Rgba,
    pub look: Look,
}

struct Ctx<'a> {
    r: &'a Renderer,
    brush: &'a ID2D1SolidColorBrush,
    fg: Rgba,
    s: f32,
    h: f32,
    /// Vertical offset (DIPs) of the 48-DIP design inside the actual bar.
    oy: f32,
    look: Look,
    primary: Rgba,
    secondary: Rgba,
}

impl Ctx<'_> {
    fn t(&self) -> &ID2D1DCRenderTarget {
        &self.r.target
    }

    fn fonts(&self) -> &Fonts {
        &self.r.fonts.as_ref().unwrap().1
    }

    fn color(&self, c: Rgba) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&c.d2d()) };
        self.brush
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        unsafe {
            self.t().FillRectangle(&D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }, self.color(c));
        }
    }

    fn round_rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }, radiusX: r, radiusY: r };
        unsafe { self.t().FillRoundedRectangle(&rr, self.color(c)) };
    }

    fn text(&self, s: &str, font: &IDWriteTextFormat, x: f32, y: f32, w: f32, h: f32, align: DWRITE_TEXT_ALIGNMENT, c: Rgba) {
        unsafe {
            let _ = font.SetTextAlignment(align);
            self.t().DrawText(
                &wide(s),
                font,
                &D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h },
                self.color(c),
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// A y coordinate of the 48-DIP design, in pixels of the actual bar.
    fn y(&self, dip: f32) -> f32 {
        (self.oy + dip) * self.s
    }

    /// Draws an item's lines of text from `x` (px), each on its baseline.
    fn draw_lines(&self, x: f32, lines: &[Line]) {
        let dwrite = &self.r.dwrite;
        let fonts = self.fonts();
        for line in lines {
            let mut cx = x;
            for p in &line.parts {
                cx += p.gap * self.s;
                let Some((layout, width, baseline)) = text_layout(dwrite, fonts, p) else { continue };
                unsafe {
                    self.t().DrawTextLayout(
                        Vector2 { X: cx, Y: self.y(line.baseline) - baseline },
                        &layout,
                        self.color(self.fg.alpha(p.alpha)),
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
                cx += width;
            }
        }
    }

    /// `N/E/T` stacked letters: the name, standing beside the graphic.
    fn letters(&self, word: &str, x: f32) -> f32 {
        let s = self.s;
        let n = word.chars().count();
        // Four letters (TEMP) sit a little tighter to keep the column within the bar.
        let (line, font) = if n > 3 { (8.0 * s, &self.fonts().letter_small) } else { (9.0 * s, &self.fonts().letter) };
        let top = self.y(24.0) - line * n as f32 / 2.0 - 1.0 * s;
        for (i, ch) in word.chars().enumerate() {
            self.text(&ch.to_string(), font, x, top + i as f32 * line, 8.0 * s, line + 2.0 * s, DWRITE_TEXT_ALIGNMENT_CENTER, self.fg.alpha(0.8));
        }
        x + 8.0 * s
    }

    /// Mini bar graph in a rounded box; `layers` stack bottom-up, values 0..=max.
    fn graph(&self, x: f32, w: f32, layers: &[(Vec<f32>, Rgba)], max: f32) {
        let s = self.s;
        let h = 30.0 * s;
        let y = (self.h - h) / 2.0;
        self.round_rect(x, y, w, h, 3.0 * s, self.fg.alpha(0.1));
        let pad = (1.5 * s).round();
        let bar = (1.5 * s).round().max(1.0);
        let gap = (0.5 * s).round().max(1.0);
        let inner_h = h - 2.0 * pad;
        let n = ((w - 2.0 * pad + gap) / (bar + gap)).floor() as usize;
        let base = (y + h - pad).round();
        for i in 0..n {
            let bx = (x + w - pad - (n - i) as f32 * (bar + gap) + gap).round();
            let mut top = base;
            for (data, color) in layers {
                let k = data.len() as isize - (n - i) as isize;
                if k < 0 {
                    continue;
                }
                let v = data[k as usize].clamp(0.0, max);
                let bh = (v / max * inner_h).round();
                if bh > 0.0 {
                    self.rect(bx, top - bh, bar, bh, *color);
                    top -= bh;
                }
            }
        }
    }

    /// Mirrored mini graph: `up` grows from the middle upwards, `down` downwards.
    fn mirror(&self, x: f32, w: f32, up: &[f32], down: &[f32]) {
        let s = self.s;
        let h = 30.0 * s;
        let y = (self.h - h) / 2.0;
        self.round_rect(x, y, w, h, 3.0 * s, self.fg.alpha(0.1));
        let pad = (1.5 * s).round();
        let bar = (1.5 * s).round().max(1.0);
        let gap = (0.5 * s).round().max(1.0);
        let n = ((w - 2.0 * pad + gap) / (bar + gap)).floor() as usize;
        let tail = |a: &[f32]| a[a.len().saturating_sub(n)..].to_vec();
        let (u, d) = (tail(up), tail(down));
        let peak = u.iter().chain(&d).fold(64.0f32 * 1024.0, |m: f32, v| m.max(*v));
        let mid = (y + h / 2.0).round();
        let half = h / 2.0 - pad;
        for i in 0..n {
            let bx = (x + w - pad - (n - i) as f32 * (bar + gap) + gap).round();
            let get = |a: &Vec<f32>| a.len().checked_sub(n - i).map(|k| a[k]).unwrap_or(0.0);
            let hu = (get(&u) / peak * half).round().max(1.0);
            let hd = (get(&d) / peak * half).round().max(1.0);
            self.rect(bx, mid - hu, bar, hu, self.secondary);
            self.rect(bx, mid, bar, hd, self.primary);
        }
    }

    fn arc_path(&self, cx: f32, cy: f32, r: f32, frac: f32, pie: bool) -> Option<ID2D1PathGeometry> {
        let frac = frac.clamp(0.0, 1.0);
        if frac <= 0.001 {
            return None;
        }
        unsafe {
            let geo = self.r.factory.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            let start = Vector2 { X: cx, Y: cy - r };
            if pie {
                sink.BeginFigure(Vector2 { X: cx, Y: cy }, D2D1_FIGURE_BEGIN_FILLED);
                sink.AddLine(start);
            } else {
                sink.BeginFigure(start, D2D1_FIGURE_BEGIN_HOLLOW);
            }
            // A single arc cannot close a full circle; split at the half.
            let draw = |f: f32| {
                let a = f * 2.0 * PI;
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: Vector2 { X: cx + r * a.sin(), Y: cy - r * a.cos() },
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                });
            };
            if frac > 0.5 {
                draw(0.5);
            }
            draw(frac.min(0.9999));
            sink.EndFigure(if pie { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(geo)
        }
    }

    fn ring(&self, cx: f32, cy: f32, r: f32, stroke: f32, frac: f32, color: Rgba) {
        unsafe {
            let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
            self.t().DrawEllipse(&e, self.color(self.fg.alpha(0.2)), stroke, None);
            if let Some(g) = self.arc_path(cx, cy, r, frac, false) {
                self.t().DrawGeometry(&g, self.color(color), stroke, &self.r.round);
            }
        }
    }

    fn pie(&self, cx: f32, cy: f32, r: f32, frac: f32, color: Rgba) {
        unsafe {
            let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
            self.t().FillEllipse(&e, self.color(self.fg.alpha(0.2)));
            if let Some(g) = self.arc_path(cx, cy, r, frac, true) {
                self.t().FillGeometry(&g, self.color(color), None);
            }
            self.t().DrawEllipse(&e, self.color(self.fg.alpha(0.55)), 1.0 * self.s, None);
        }
    }

    /// A filled shape from points in design DIPs, relative to `x` (px) and the bar's top.
    fn polygon(&self, x: f32, points: &[(f32, f32)]) -> Option<ID2D1PathGeometry> {
        let at = |(px, py): (f32, f32)| Vector2 { X: x + px * self.s, Y: self.y(py) };
        unsafe {
            let geo = self.r.factory.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            sink.BeginFigure(at(points[0]), D2D1_FIGURE_BEGIN_FILLED);
            for &p in &points[1..] {
                sink.AddLine(at(p));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            Some(geo)
        }
    }

    /// Battery standing up, filling from the bottom: the secondary colour while
    /// charging (with a bolt), red at 20% or less.
    fn battery(&self, x: f32, frac: Option<f32>, charging: bool) {
        let s = self.s;
        let (w, h, y) = (13.0 * s, 24.0 * s, self.y(13.0));
        self.round_rect(x + 4.0 * s, y - 2.5 * s, 5.0 * s, 2.5 * s, 1.0 * s, self.fg.alpha(0.5));
        unsafe {
            let outline = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }, radiusX: 3.0 * s, radiusY: 3.0 * s };
            self.t().DrawRoundedRectangle(&outline, self.color(self.fg.alpha(0.5)), 1.0 * s, None);
        }
        let Some(frac) = frac else { return };
        let frac = frac.clamp(0.0, 1.0);
        let inner = h - 4.0 * s;
        let fill = inner * frac;
        let color = if charging {
            self.secondary
        } else if frac <= 0.2 {
            Rgba::RED
        } else {
            self.fg
        };
        self.round_rect(x + 2.0 * s, y + 2.0 * s + inner - fill, w - 4.0 * s, fill, 1.5 * s, color);
        if charging {
            let bolt = [(7.5, 18.0), (4.0, 25.0), (7.0, 25.0), (5.5, 31.0), (10.0, 23.0), (7.0, 23.0)];
            if let Some(g) = self.polygon(x, &bolt) {
                unsafe { self.t().FillGeometry(&g, self.color(Rgba(0.1, 0.1, 0.1, 1.0)), None) };
            }
        }
    }

    /// Four bands: the theme colour up to 70 °C, then warm, hot, and red from 90.
    fn temp_color(&self, celsius: f32) -> Rgba {
        match celsius.round() {
            c if c < 71.0 => self.primary,
            c if c < 80.0 => Rgba::YELLOW,
            c if c < 90.0 => Rgba::ORANGE,
            _ => Rgba::RED,
        }
    }

    /// Thermometer whose column rises with the temperature (30..100 °C).
    fn thermometer(&self, x: f32, celsius: Option<f32>) {
        let s = self.s;
        let cx = x + 7.0 * s;
        let (top, bulb_y) = (self.y(9.0), self.y(34.0));
        let (tube, bulb) = (3.2 * s, 5.6 * s);
        let outline = self.fg.alpha(0.5);
        // One outline: up the tube, round its top, down the other side and round the bulb
        // back to the start. The bulb is open where the tube enters it, with no seams.
        let neck = (bulb * bulb - tube * tube).sqrt(); // where the tube's sides meet the bulb
        unsafe {
            if let Ok(geo) = self.r.factory.CreatePathGeometry() {
                if let Ok(sink) = geo.Open() {
                    let arc = |to: Vector2, r: f32, size| D2D1_ARC_SEGMENT {
                        point: to,
                        size: D2D_SIZE_F { width: r, height: r },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: size,
                    };
                    sink.BeginFigure(Vector2 { X: cx - tube, Y: bulb_y - neck }, D2D1_FIGURE_BEGIN_HOLLOW);
                    sink.AddLine(Vector2 { X: cx - tube, Y: top + tube });
                    sink.AddArc(&arc(Vector2 { X: cx + tube, Y: top + tube }, tube, D2D1_ARC_SIZE_SMALL));
                    sink.AddLine(Vector2 { X: cx + tube, Y: bulb_y - neck });
                    sink.AddArc(&arc(Vector2 { X: cx - tube, Y: bulb_y - neck }, bulb, D2D1_ARC_SIZE_LARGE));
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    if sink.Close().is_ok() {
                        self.t().DrawGeometry(&geo, self.color(outline), 1.0 * s, &self.r.round);
                    }
                }
            }
        }
        let Some(c) = celsius else { return };
        let color = self.temp_color(c);
        let frac = ((c - 30.0) / 70.0).clamp(0.0, 1.0);
        let col_top = bulb_y - 2.0 * s - frac * (bulb_y - 2.0 * s - (top + 2.5 * s));
        self.round_rect(cx - 1.4 * s, col_top, 2.8 * s, bulb_y - col_top, 1.4 * s, color);
        let r = bulb - 1.8 * s;
        unsafe { self.t().FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: bulb_y }, radiusX: r, radiusY: r }, self.color(color)) };
    }

    /// A case fan: seven curved blades in a thin ring, hub in the theme colour.
    fn fan(&self, x: f32) {
        let s = self.s;
        let (cx, cy) = (x + 12.0 * s, self.y(24.0));
        unsafe {
            let ring = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 11.3 * s, radiusY: 11.3 * s };
            self.t().DrawEllipse(&ring, self.color(self.fg.alpha(0.45)), 1.2 * s, None);
            if let Ok(geo) = self.r.factory.CreatePathGeometry() {
                if let Ok(sink) = geo.Open() {
                    // Neighbouring blades overlap near the hub; the default (alternate)
                    // fill would cut those overlaps out and leave dark notches there.
                    sink.SetFillMode(D2D1_FILL_MODE_WINDING);
                    const BLADES: usize = 7;
                    for i in 0..BLADES {
                        let (sin, cos) = (i as f32 * 2.0 * PI / BLADES as f32).sin_cos();
                        // A blade pointing up, swept clockwise, turned into place.
                        let p = |bx: f32, by: f32| Vector2 { X: cx + (bx * cos - by * sin) * s, Y: cy + (bx * sin + by * cos) * s };
                        let curve = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| D2D1_BEZIER_SEGMENT { point1: p(a.0, a.1), point2: p(b.0, b.1), point3: p(c.0, c.1) };
                        sink.BeginFigure(p(-1.6, -2.6), D2D1_FIGURE_BEGIN_FILLED);
                        sink.AddBezier(&curve((-0.6, -6.2), (2.2, -9.4), (6.4, -9.6)));
                        sink.AddBezier(&curve((7.4, -9.6), (7.6, -8.9), (6.9, -8.4)));
                        sink.AddBezier(&curve((4.6, -6.9), (3.0, -4.8), (2.2, -2.2)));
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    }
                    if sink.Close().is_ok() {
                        self.t().FillGeometry(&geo, self.color(self.fg.alpha(0.92)), None);
                    }
                }
            }
            let hub = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 3.1 * s, radiusY: 3.1 * s };
            self.t().FillEllipse(&hub, self.color(self.primary));
        }
    }

    /// The NPU: a chip with three pins a side, whose core fills bottom-up with the load.
    /// Drawn in a 30-DIP square, the height of the bar graphs.
    fn chip(&self, x: f32, frac: Option<f32>) {
        let s = self.s;
        let top = self.y(9.0);
        let at = |v: f32| v * s;
        let pin = self.fg.alpha(0.5);
        for c in [10.0, 15.0, 20.0] {
            let (a, w, l) = (at(c - 0.4), at(0.8), at(2.1));
            self.round_rect(x + a, top + at(0.9), w, l, at(0.4), pin);
            self.round_rect(x + a, top + at(27.0), w, l, at(0.4), pin);
            self.round_rect(x + at(0.9), top + a, l, w, at(0.4), pin);
            self.round_rect(x + at(27.0), top + a, l, w, at(0.4), pin);
        }
        unsafe {
            let body = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: x + at(3.4), top: top + at(3.4), right: x + at(26.6), bottom: top + at(26.6) },
                radiusX: at(3.0),
                radiusY: at(3.0),
            };
            self.t().DrawRoundedRectangle(&body, self.color(self.fg.alpha(0.5)), at(0.8).max(1.0), None);
        }
        let (core, side) = (at(5.4), at(19.2));
        self.round_rect(x + core, top + core, side, side, at(1.6), self.fg.alpha(0.1));
        let fill = side * frac.unwrap_or(0.0).clamp(0.0, 1.0);
        if fill >= 0.5 {
            self.round_rect(x + core, top + core + side - fill, side, fill, at(1.6), self.primary);
        }
    }

    /// Power: an orange lightning bolt.
    fn bolt(&self, x: f32) {
        let points = [(10.0, 10.0), (2.0, 26.0), (8.0, 26.0), (6.0, 38.0), (15.0, 21.0), (9.0, 21.0), (12.0, 10.0)];
        if let Some(g) = self.polygon(x, &points) {
            unsafe {
                self.t().FillGeometry(&g, self.color(Rgba::ORANGE), None);
                self.t().DrawGeometry(&g, self.color(Rgba::ORANGE), 1.0 * self.s, &self.r.round);
            }
        }
    }

    /// What an item's text says right now.
    fn reading(&self, m: ModuleId, snap: &Snapshot) -> Reading {
        let pct = |v: f32| Reading::Value { value: (v.round() as i32).to_string(), unit: "%" };
        let none = || Reading::Value { value: "—".into(), unit: "" };
        match m {
            ModuleId::Cpu => pct(snap.cpu.total),
            ModuleId::Gpu => pct(snap.gpus.first().map(|g| g.util).unwrap_or(0.0)),
            ModuleId::Mem => pct(snap.mem.used_pct),
            ModuleId::Disk => pct(disk_used(snap) * 100.0),
            ModuleId::Battery => snap.battery.as_ref().map_or_else(none, |b| pct(b.percent)),
            ModuleId::Temp => snap.headline_temp().map_or_else(none, |t| Reading::Value { value: format!("{}°", t.round() as i32), unit: "" }),
            ModuleId::Fans => snap.sensors.bar_fan_rpm().map_or_else(none, |f| Reading::Value { value: (f.round() as i32).to_string(), unit: "" }),
            ModuleId::Power => snap.sensors.package_watts().map_or_else(none, |p| Reading::Value { value: self.r.number(p, 1), unit: "W" }),
            ModuleId::Net => {
                let (up, down, unit) = self.r.rates(snap.net.up, snap.net.down);
                Reading::Rates { up, down, unit }
            }
            ModuleId::Npu => snap.npu.as_ref().map_or_else(none, |n| pct(n.util)),
        }
    }

    fn item(&self, m: ModuleId, x: f32, w: f32, snap: &Snapshot, hist: &crate::stats::model::History) {
        let s = self.s;
        // Stacked letters go before the graphic, or after the number at the item's end;
        // either way they take the same room.
        let x = match (self.look.label, self.look.side) {
            (Label::Side, Side::Left) => self.letters(name(m), x) + 4.0 * s,
            (Label::Side, Side::Right) => {
                self.letters(name(m), x + w - 8.0 * s);
                x
            }
            _ => x,
        };
        if !self.look.graphics {
            self.draw_lines(x, &lines(m, self.look.label, &self.reading(m, snap)));
            return;
        }
        let gw = graphic_width(m) * s;
        self.graphic(m, x, gw, snap, hist);
        self.draw_lines(x + gw + 6.0 * s, &lines(m, self.look.label, &self.reading(m, snap)));
    }

    /// The graphic left of an item's text, `gw` px wide from `x`.
    fn graphic(&self, m: ModuleId, x: f32, gw: f32, snap: &Snapshot, hist: &crate::stats::model::History) {
        let s = self.s;
        let cy = self.y(24.0);
        match m {
            ModuleId::Cpu => self.graph(x, gw, &[(hist.cpu_user.last_n(64).collect(), self.primary), (hist.cpu_system.last_n(64).collect(), self.secondary)], 100.0),
            ModuleId::Gpu => {
                let data = hist.gpus.first().map(|r| r.last_n(64).collect()).unwrap_or_default();
                self.graph(x, gw, &[(data, self.primary)], 100.0);
            }
            ModuleId::Mem => {
                let r = 11.5 * s;
                self.ring(x + 2.0 * s + r, cy, r, 3.5 * s, snap.mem.used_pct / 100.0, self.primary);
            }
            ModuleId::Net => {
                let up: Vec<f32> = hist.net_up.last_n(64).collect();
                let down: Vec<f32> = hist.net_down.last_n(64).collect();
                self.mirror(x, gw, &up, &down);
            }
            ModuleId::Disk => {
                let r = 11.0 * s;
                self.pie(x + 1.0 * s + r, cy, r, disk_used(snap), self.primary);
            }
            ModuleId::Battery => self.battery(x, snap.battery.as_ref().map(|b| b.percent / 100.0), snap.battery.as_ref().is_some_and(|b| b.charging)),
            ModuleId::Temp => self.thermometer(x, snap.headline_temp()),
            ModuleId::Fans => self.fan(x),
            ModuleId::Power => self.bolt(x),
            ModuleId::Npu => self.chip(x, snap.npu.as_ref().map(|n| n.util / 100.0)),
        }
    }

    /// Compact mode: one cell, its tag on one side and its number on the other, lined
    /// up with the other cell of its column. Numbers are right-aligned.
    fn compact(&self, slot: &Slot, x: f32, snap: &Snapshot) {
        let s = self.s;
        let gap = TAG_GAP * s;
        let (tag_x, num_x) = match self.look.side {
            Side::Left => (x, x + slot.tag_w + gap),
            Side::Right => (x + slot.num_w + gap, x),
        };
        let (dwrite, fonts) = (&self.r.dwrite, self.fonts());
        let number = |parts: Vec<Part>, baseline: f32| {
            let line = Line { baseline, parts };
            let w = line_width(dwrite, fonts, &line, s);
            self.draw_lines(num_x + slot.num_w - w, &[line]);
        };
        if slot.module == ModuleId::Net {
            // The arrows are the network's tag.
            let (up, down, unit) = self.r.rates(snap.net.up, snap.net.down);
            for (baseline, arrow, v) in [(TOP_BASE, "↑", up), (BOTTOM_BASE, "↓", down)] {
                let a = Line { baseline, parts: vec![part(0.0, arrow, Font::Arrow, 0.75)] };
                let aw = line_width(dwrite, fonts, &a, s);
                self.draw_lines(tag_x + (slot.tag_w - aw) / 2.0, &[a]);
                number(compact_value(v, unit), baseline);
            }
            return;
        }
        let baseline = match slot.row {
            Row::Top => TOP_BASE,
            Row::Bottom => BOTTOM_BASE,
            Row::Full => FULL_BASE,
        };
        match self.look.tag {
            Tag::Label => self.draw_lines(tag_x, &[Line { baseline, parts: vec![part(0.0, name(slot.module), Font::CompactTag, 0.62)] }]),
            // Centred on the digits' middle, a little under half their height.
            Tag::Icon => self.icon(slot.module, tag_x, self.y(baseline - 4.8) - ICON * s / 2.0, ICON * s, self.fg.alpha(0.75)),
        }
        if let Reading::Value { value, unit } = self.reading(slot.module, snap) {
            number(compact_value(value, unit), baseline);
        }
    }

    /// One of the settings window's line icons, `size` px square with its corner at
    /// (x, y): stroked like the SVG (1.7 of 24 units, round ends and joins).
    fn icon(&self, m: ModuleId, x: f32, y: f32, size: f32, color: Rgba) {
        let k = size / 24.0;
        let at = |px: f32, py: f32| Vector2 { X: x + px * k, Y: y + py * k };
        let stroke = (1.7 * k).max(1.0);
        let rounded = |rx: f32, ry: f32, w: f32, h: f32, r: f32| D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: x + rx * k, top: y + ry * k, right: x + (rx + w) * k, bottom: y + (ry + h) * k },
            radiusX: r * k,
            radiusY: r * k,
        };
        let ellipse = |cx: f32, cy: f32, rx: f32, ry: f32| D2D1_ELLIPSE { point: at(cx, cy), radiusX: rx * k, radiusY: ry * k };
        let brush = self.color(color);
        unsafe {
            for shape in icons::shapes(m) {
                match *shape {
                    Shape::Path(d) => {
                        if let Some(g) = self.path(&icons::parse(d), &at, k, false) {
                            self.t().DrawGeometry(&g, brush, stroke, &self.r.round);
                        }
                    }
                    Shape::Fill(d) => {
                        if let Some(g) = self.path(&icons::parse(d), &at, k, true) {
                            self.t().FillGeometry(&g, brush, None);
                            self.t().DrawGeometry(&g, brush, k, &self.r.round);
                        }
                    }
                    Shape::Rect(rx, ry, w, h, r) => self.t().DrawRoundedRectangle(&rounded(rx, ry, w, h, r), brush, stroke, &self.r.round),
                    Shape::FillRect(rx, ry, w, h, r) => self.t().FillRoundedRectangle(&rounded(rx, ry, w, h, r), brush),
                    Shape::Ellipse(cx, cy, rx, ry) => self.t().DrawEllipse(&ellipse(cx, cy, rx, ry), brush, stroke, &self.r.round),
                    Shape::FillCircle(cx, cy, r) => self.t().FillEllipse(&ellipse(cx, cy, r, r), brush),
                }
            }
        }
    }

    /// Path segments as a geometry, through `at` (icon units to pixels).
    fn path(&self, segs: &[Seg], at: &dyn Fn(f32, f32) -> Vector2, k: f32, fill: bool) -> Option<ID2D1PathGeometry> {
        unsafe {
            let geo = self.r.factory.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            let begin = if fill { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW };
            let mut open = false;
            for seg in segs {
                match *seg {
                    Seg::Move(px, py) => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        sink.BeginFigure(at(px, py), begin);
                        open = true;
                    }
                    _ if !open => {}
                    Seg::Line(px, py) => sink.AddLine(at(px, py)),
                    Seg::Cubic(p) => sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: at(p[0], p[1]), point2: at(p[2], p[3]), point3: at(p[4], p[5]) }),
                    Seg::Arc { rx, ry, rotation, large, sweep, x, y } => sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: at(x, y),
                        size: D2D_SIZE_F { width: rx * k, height: ry * k },
                        rotationAngle: rotation,
                        sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    }),
                    Seg::Close => {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            sink.Close().ok()?;
            Some(geo)
        }
    }
}

/// Used fraction of C: (or the first volume).
fn disk_used(snap: &Snapshot) -> f32 {
    snap.disk
        .volumes
        .iter()
        .find(|v| v.mount.eq_ignore_ascii_case("C:"))
        .or(snap.disk.volumes.first())
        .map(|v| if v.total > 0 { (v.total - v.free) as f32 / v.total as f32 } else { 0.0 })
        .unwrap_or(0.0)
}

/// The user's decimal separator (`,` on pt-BR), so the strip matches the popup.
fn decimal_separator() -> char {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SDECIMAL};
    let mut buf = [0u16; 8];
    let n = unsafe { GetLocaleInfoEx(windows::core::PCWSTR::null(), LOCALE_SDECIMAL, Some(&mut buf)) };
    if n > 1 { char::from_u32(buf[0] as u32).unwrap_or('.') } else { '.' }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store with a minute of made-up but plausible readings for every item.
    fn sample_store(temp: f32) -> Store {
        use crate::stats::model::*;
        let mut store = Store { snapshot: Snapshot::default(), history: History::new(), long: crate::stats::long::LongHistory::load(None) };
        let reading = |v: f32| Reading { id: "x".into(), label: "x".into(), value: v };
        // A fixed pseudo-random walk, so the graphs look like a real minute, every run.
        let mut seed = 7u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        for i in 0..64 {
            let t = i as f32 / 4.0;
            let tf = t as f64;
            let n = noise();
            let snap = &mut store.snapshot;
            snap.cpu.user = 12.0 + 20.0 * t.sin().abs() * n + 10.0 * noise();
            snap.cpu.system = 3.0 + 7.0 * noise();
            snap.cpu.total = snap.cpu.user + snap.cpu.system;
            snap.cpu.temp_c = Some(temp);
            snap.mem.used_pct = 67.0;
            snap.net.up = 9.0 * 1024.0 * (0.3 + 1.4 * noise() as f64) * (1.0 + 0.6 * (tf * 1.3).sin());
            snap.net.down = 8.0 * 1024.0 * (0.2 + 1.8 * noise() as f64) * (1.0 + 0.8 * (tf * 0.9).cos());
            snap.disk.volumes = vec![Volume { mount: "C:".into(), total: 100, free: 38, ..Default::default() }];
            snap.gpus = vec![Gpu { util: 4.0 + 14.0 * noise() * noise(), ..Default::default() }];
            snap.battery = Some(Battery { percent: 84.0, charging: true, plugged: true, ..Default::default() });
            snap.sensors.fans = vec![reading(2140.0)];
            snap.sensors.power = vec![reading(18.0)];
            snap.npu = Some(Npu { name: "NPU".into(), util: 64.0, ..Default::default() });
            let snap = snap.clone();
            store.history.record(&snap);
        }
        store.snapshot.net.up = 9.0 * 1024.0;
        store.snapshot.net.down = 8.0 * 1024.0;
        store.snapshot.cpu.total = 36.0;
        store.snapshot.gpus[0].util = 10.0;
        store
    }

    /// Renders every item in every label mode, dark and light, to PNGs in the folder
    /// `WSTATS_SHOTS` names; for comparing against the designs by eye.
    #[test]
    #[ignore]
    fn render_every_label() {
        use base64::Engine;
        let Some(dir) = std::env::var_os("WSTATS_SHOTS") else { return };
        let all = [
            ModuleId::Cpu,
            ModuleId::Gpu,
            ModuleId::Npu,
            ModuleId::Mem,
            ModuleId::Net,
            ModuleId::Disk,
            ModuleId::Battery,
            ModuleId::Temp,
            ModuleId::Fans,
            ModuleId::Power,
        ];
        let colors = (Rgba::BLUE, Rgba::GREEN);
        let save = |name: &str, url: Option<String>| {
            let b64 = url.unwrap().split_once(',').unwrap().1.to_string();
            let png = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
            std::fs::write(std::path::Path::new(&dir).join(name), png).unwrap();
        };
        let store = sample_store(71.0);
        let shots = |name: &str, look: Look| {
            save(&format!("{name}-dark.png"), preview_png(&all, look, &store, false, colors, 2.0));
            save(&format!("{name}-light.png"), preview_png(&all, look, &store, true, colors, 2.0));
        };
        shots("side-right", Look { label: Label::Side, side: Side::Right, ..Look::default() });
        shots("side-right-plain", Look { label: Label::Side, side: Side::Right, graphics: false, ..Look::default() });
        for (label, tag) in [(Label::Top, "top"), (Label::Bottom, "bottom"), (Label::Side, "side"), (Label::None, "none")] {
            shots(tag, Look { label, ..Look::default() });
            shots(&format!("{tag}-plain"), Look { label, graphics: false, ..Look::default() });
        }
        for (tag, t) in [(Tag::Icon, "icon"), (Tag::Label, "label")] {
            for (side, sd) in [(Side::Left, "left"), (Side::Right, "right")] {
                shots(&format!("compact-{t}-{sd}"), Look { compact: true, tag, side, ..Look::default() });
            }
        }
        for t in [55.0, 70.0, 75.0, 85.0, 94.0] {
            save(&format!("temp-{t}.png"), preview_png(&[ModuleId::Temp], Look::default(), &sample_store(t), false, colors, 2.0));
        }
    }

    #[test]
    fn network_rates_share_a_unit_and_keep_to_two_digits() {
        let r = Renderer::new().unwrap();
        let kb = 1024.0;
        let mb = kb * 1024.0;
        let rates = |up, down| {
            let (u, d, unit) = r.rates(up, down);
            (u.replace(',', "."), d.replace(',', "."), unit)
        };
        assert_eq!(rates(9.0 * kb, 8.0 * kb), ("9".into(), "8".into(), "KB/s"));
        assert_eq!(rates(99.0 * kb, 1.0 * kb), ("99".into(), "1".into(), "KB/s"));
        assert_eq!(rates(100.0 * kb, 0.0), ("0.1".into(), "0".into(), "MB/s"));
        assert_eq!(rates(450.0 * kb, 2.3 * mb), ("0.4".into(), "2.3".into(), "MB/s"));
        assert_eq!(rates(3.0 * kb, 50.0 * mb), ("0".into(), "50".into(), "MB/s"));
        assert_eq!(rates(0.0, 150.0 * mb), ("0".into(), "0.1".into(), "GB/s"));
    }

    #[test]
    fn compact_pairs_items_and_leaves_the_network_alone() {
        use ModuleId::*;
        let cols = columns(&[Cpu, Gpu, Npu, Net, Mem, Disk, Power]);
        assert_eq!(cols, vec![vec![Cpu, Gpu], vec![Npu], vec![Net], vec![Mem, Disk], vec![Power]]);
        let (slots, _) = layout(&[Cpu, Gpu, Net, Power], Look { compact: true, ..Look::default() }, 1.0);
        let rows: Vec<Row> = slots.iter().map(|s| s.row).collect();
        assert_eq!(rows, vec![Row::Top, Row::Bottom, Row::Full, Row::Full]);
        // A column's two cells share it.
        assert_eq!((slots[0].x, slots[0].w), (slots[1].x, slots[1].w));
        assert!(slots[2].x >= slots[0].x + slots[0].w);
    }

    #[test]
    fn items_without_graphics_are_narrower() {
        let full = content_width(ModuleId::Cpu, Look::default(), 1.0);
        let plain = content_width(ModuleId::Cpu, Look { graphics: false, ..Look::default() }, 1.0);
        assert!((full - plain - (graphic_width(ModuleId::Cpu) + 6.0)).abs() < 0.01);
    }

    #[test]
    fn layout_is_contiguous() {
        let (slots, width) = layout(&[ModuleId::Net, ModuleId::Cpu], Look::default(), 1.75);
        assert_eq!(slots[0].x, 0);
        assert_eq!(slots[1].x, slots[0].w + (ITEM_GAP * 1.75f32).round() as i32);
        assert_eq!(width, slots[1].x + slots[1].w);
    }
}

/// Renders the strip off-screen as a PNG data URL, for the settings window's preview.
/// `scale` 2 keeps it crisp on high-DPI screens; the page draws it at half size.
pub fn preview_png(modules: &[ModuleId], look: Look, store: &Store, light: bool, colors: (Rgba, Rgba), scale: f32) -> Option<String> {
    use base64::Engine;
    let (slots, width) = layout(modules, look, scale);
    let frame = Frame {
        slots,
        width,
        height: (48.0 * scale).round() as i32,
        scale,
        light,
        hover: None,
        active: None,
        primary: colors.0,
        secondary: colors.1,
        look,
    };
    let mut r = Renderer::new().ok()?;
    r.paint(&frame, store).ok()?;
    let (px, w, h) = r.pixels()?;
    // Premultiplied BGRA -> straight RGBA for PNG.
    let mut rgba = Vec::with_capacity(px.len());
    for p in px.chunks(4) {
        let a = p[3] as f32 / 255.0;
        let un = |c: u8| if a > 0.0 { ((c as f32 / a).round()).min(255.0) as u8 } else { 0 };
        rgba.extend_from_slice(&[un(p[2]), un(p[1]), un(p[0]), if p[3] <= 1 { 0 } else { p[3] }]);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().ok()?.write_image_data(&rgba).ok()?;
    }
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(out)))
}
