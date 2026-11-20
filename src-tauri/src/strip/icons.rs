//! The settings window's line icons, for the compact strip. They are 24-unit SVG drawn
//! with a 1.7-unit round stroke; copied from `src/settings.ts`, keep the two in step.
//! The NPU differs on purpose: the chip with a star inside reads as the CPU at 12 DIPs,
//! so the strip uses the star alone.

use crate::config::ModuleId;
use crate::stats::peripherals::Kind;

pub enum Shape {
    /// Stroked path data.
    Path(&'static str),
    /// Filled (and thinly stroked) path data.
    Fill(&'static str),
    /// x, y, width, height, corner radius; stroked.
    Rect(f32, f32, f32, f32, f32),
    FillRect(f32, f32, f32, f32, f32),
    /// cx, cy, rx, ry; stroked.
    Ellipse(f32, f32, f32, f32),
    FillCircle(f32, f32, f32),
}

use Shape::*;

pub fn shapes(m: ModuleId) -> &'static [Shape] {
    match m {
        ModuleId::Cpu => &[
            Rect(6.0, 6.0, 12.0, 12.0, 2.0),
            Rect(9.5, 9.5, 5.0, 5.0, 0.8),
            Path("M9 3v3M12 3v3M15 3v3M9 18v3M12 18v3M15 18v3M3 9h3M3 12h3M3 15h3M18 9h3M18 12h3M18 15h3"),
        ],
        ModuleId::Gpu => &[Rect(2.5, 6.5, 19.0, 11.0, 2.0), Ellipse(9.0, 12.0, 2.8, 2.8), Path("M15 10h3.5M15 13.5h3.5M5 17.5v2M8 17.5v2")],
        ModuleId::Mem => &[Rect(3.0, 7.0, 18.0, 9.0, 1.5), Path("M7 10v3M10.5 10v3M14 10v3M17.5 10v3M5 16v2.5M9 16v2.5M13 16v2.5M17 16v2.5")],
        ModuleId::Disk => &[Ellipse(12.0, 6.5, 8.0, 3.0), Path("M4 6.5v11c0 1.7 3.6 3 8 3s8-1.3 8-3v-11"), Path("M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3")],
        ModuleId::Net => &[Path("M7 20V5M3.5 8.5 7 5l3.5 3.5M17 4v15M13.5 15.5 17 19l3.5-3.5")],
        ModuleId::Battery => &[Rect(2.5, 7.0, 17.0, 10.0, 2.2), Path("M21.5 10.5v3"), FillRect(5.0, 9.5, 9.0, 5.0, 1.0)],
        ModuleId::Temp => &[Path("M10 14.5V5a2 2 0 1 1 4 0v9.5a4 4 0 1 1-4 0z"), Path("M12 9v7"), FillCircle(12.0, 17.5, 1.4)],
        ModuleId::Fans => &[
            Ellipse(12.0, 12.0, 9.2, 9.2),
            Ellipse(12.0, 12.0, 2.2, 2.2),
            Path("M11.5 9.7C11.8 7.0 14.0 4.8 16.9 4.6M13.7 10.4C16.2 9.3 19.2 10.1 20.9 12.5M14.2 12.7C16.4 14.3 17.2 17.3 16.0 19.9M12.5 14.3C12.2 17.0 10.0 19.2 7.1 19.4M10.3 13.6C7.8 14.7 4.8 13.9 3.1 11.5M9.8 11.3C7.6 9.7 6.8 6.7 8.0 4.1"),
        ],
        ModuleId::Power => &[Path("M13 2.8 5.5 13.2h5.6L10 21.2l8.5-11h-5.8z")],
        ModuleId::Npu => &[
            Fill("M12 3.2l1.9 5.6 5.6 1.9-5.6 1.9-1.9 5.6-1.9-5.6-5.6-1.9 5.6-1.9z"),
            Fill("M18.6 15.4l.6 1.7 1.7.6-1.7.6-.6 1.7-.6-1.7-1.7-.6 1.7-.6z"),
        ],
        ModuleId::Devices => kind_shapes(Kind::Headset),
    }
}

/// A peripheral's kind; the same drawings as `src/modules/devices.ts`.
pub fn kind_shapes(k: Kind) -> &'static [Shape] {
    match k {
        Kind::Headset => &[Path("M4 14v-2a8 8 0 0 1 16 0v2"), Rect(3.0, 14.0, 4.0, 6.0, 1.5), Rect(17.0, 14.0, 4.0, 6.0, 1.5)],
        Kind::Mouse => &[Rect(6.0, 3.0, 12.0, 18.0, 6.0), Path("M12 7v4")],
        Kind::Keyboard => &[Rect(2.5, 6.0, 19.0, 12.0, 2.0), Path("M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10")],
        Kind::Gamepad => &[
            Path("M6 8h12a4 4 0 0 1 4 4l-.5 4a2.5 2.5 0 0 1-4.3 1.4L15.5 16h-7l-1.7 1.4A2.5 2.5 0 0 1 2.5 16L2 12a4 4 0 0 1 4-4z"),
            Path("M7 11v3M5.5 12.5h3"),
        ],
        Kind::Phone => &[Rect(7.0, 2.5, 10.0, 19.0, 2.0), Path("M11 18.5h2")],
        Kind::Other => &[Path("M7 7l10 10-5 4V3l5 4L7 17")],
    }
}

/// A path segment in absolute coordinates.
#[derive(Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    /// Two control points and the end point.
    Cubic([f32; 6]),
    Arc { rx: f32, ry: f32, rotation: f32, large: bool, sweep: bool, x: f32, y: f32 },
    Close,
}

#[derive(Debug, PartialEq)]
enum Tok {
    Cmd(char),
    Num(f32),
}

/// Commands and numbers. Numbers may run together: "1.7.6" is 1.7 then .6, "8-1.3" is
/// 8 then -1.3, as SVG allows.
fn tokens(d: &str) -> Vec<Tok> {
    let b = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphabetic() {
            out.push(Tok::Cmd(c as char));
            i += 1;
        } else if c.is_ascii_digit() || c == b'-' || c == b'+' || c == b'.' {
            let start = i;
            let mut dot = c == b'.';
            i += 1;
            while i < b.len() && (b[i].is_ascii_digit() || (b[i] == b'.' && !dot)) {
                dot |= b[i] == b'.';
                i += 1;
            }
            if let Ok(v) = d[start..i].parse() {
                out.push(Tok::Num(v));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Path data to absolute segments: M L H V C S A Z, upper and lower case, with
/// implicit repeats. Stops at anything malformed.
pub fn parse(d: &str) -> Vec<Seg> {
    let t = tokens(d);
    let mut out = Vec::new();
    let (mut i, mut cmd) = (0, ' ');
    let (mut cx, mut cy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    // Second control point of the last cubic, for S.
    let mut ctrl: Option<(f32, f32)> = None;
    while i < t.len() {
        if let Tok::Cmd(c) = t[i] {
            cmd = c;
            i += 1;
            if c == 'z' || c == 'Z' {
                out.push(Seg::Close);
                (cx, cy, ctrl) = (sx, sy, None);
                continue;
            }
        }
        let mut n = |count: usize| -> Option<Vec<f32>> {
            let v: Vec<f32> = t.get(i..i + count)?.iter().map_while(|x| if let Tok::Num(v) = x { Some(*v) } else { None }).collect();
            (v.len() == count).then(|| {
                i += count;
                v
            })
        };
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let seg = match cmd.to_ascii_uppercase() {
            'M' => {
                let Some(v) = n(2) else { break };
                (cx, cy) = (ox + v[0], oy + v[1]);
                (sx, sy) = (cx, cy);
                // Further pairs after a move are lines.
                cmd = if rel { 'l' } else { 'L' };
                Seg::Move(cx, cy)
            }
            'L' => {
                let Some(v) = n(2) else { break };
                (cx, cy) = (ox + v[0], oy + v[1]);
                Seg::Line(cx, cy)
            }
            'H' => {
                let Some(v) = n(1) else { break };
                cx = ox + v[0];
                Seg::Line(cx, cy)
            }
            'V' => {
                let Some(v) = n(1) else { break };
                cy = oy + v[0];
                Seg::Line(cx, cy)
            }
            'C' => {
                let Some(v) = n(6) else { break };
                let p = [ox + v[0], oy + v[1], ox + v[2], oy + v[3], ox + v[4], oy + v[5]];
                (cx, cy) = (p[4], p[5]);
                ctrl = Some((p[2], p[3]));
                out.push(Seg::Cubic(p));
                continue;
            }
            'S' => {
                let Some(v) = n(4) else { break };
                // The first control point mirrors the last one about the current point.
                let (c1x, c1y) = ctrl.map_or((cx, cy), |(x, y)| (2.0 * cx - x, 2.0 * cy - y));
                let p = [c1x, c1y, ox + v[0], oy + v[1], ox + v[2], oy + v[3]];
                (cx, cy) = (p[4], p[5]);
                ctrl = Some((p[2], p[3]));
                out.push(Seg::Cubic(p));
                continue;
            }
            'A' => {
                let Some(v) = n(7) else { break };
                (cx, cy) = (ox + v[5], oy + v[6]);
                Seg::Arc { rx: v[0], ry: v[1], rotation: v[2], large: v[3] != 0.0, sweep: v[4] != 0.0, x: cx, y: cy }
            }
            _ => break,
        };
        ctrl = None;
        out.push(seg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_run_together() {
        assert_eq!(tokens("l.6 1.7.6-1.7"), vec![Tok::Cmd('l'), Tok::Num(0.6), Tok::Num(1.7), Tok::Num(0.6), Tok::Num(-1.7)]);
    }

    #[test]
    fn implicit_lines_after_a_move() {
        assert_eq!(parse("M3.5 8.5 7 5l3.5 3.5"), vec![Seg::Move(3.5, 8.5), Seg::Line(7.0, 5.0), Seg::Line(10.5, 8.5)]);
    }

    #[test]
    fn relative_moves_lines_and_close() {
        assert_eq!(parse("M9 3v3M12 3h3z"), vec![Seg::Move(9.0, 3.0), Seg::Line(9.0, 6.0), Seg::Move(12.0, 3.0), Seg::Line(15.0, 3.0), Seg::Close]);
    }

    #[test]
    fn smooth_cubic_mirrors_the_last_control_point() {
        let s = parse("M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3");
        assert_eq!(s[1], Seg::Cubic([4.0, 13.7, 7.6, 15.0, 12.0, 15.0]));
        assert_eq!(s[2], Seg::Cubic([16.4, 15.0, 20.0, 13.7, 20.0, 12.0]));
    }

    #[test]
    fn arcs_end_where_svg_says() {
        let s = parse("M10 14.5V5a2 2 0 1 1 4 0v9.5a4 4 0 1 1-4 0z");
        assert_eq!(s[2], Seg::Arc { rx: 2.0, ry: 2.0, rotation: 0.0, large: true, sweep: true, x: 14.0, y: 5.0 });
        assert_eq!(s[4], Seg::Arc { rx: 4.0, ry: 4.0, rotation: 0.0, large: true, sweep: true, x: 10.0, y: 14.5 });
        assert_eq!(s[5], Seg::Close);
    }

    /// Every icon parses all the way through.
    #[test]
    fn every_icon_parses() {
        for m in [ModuleId::Cpu, ModuleId::Gpu, ModuleId::Npu, ModuleId::Mem, ModuleId::Net, ModuleId::Disk, ModuleId::Battery, ModuleId::Temp, ModuleId::Fans, ModuleId::Power] {
            for shape in shapes(m) {
                if let Path(d) | Fill(d) = shape {
                    let segs = parse(d);
                    let commands = d.chars().filter(|c| c.is_ascii_alphabetic()).count();
                    assert!(segs.len() >= commands, "{m:?}: {d}");
                }
            }
        }
        use crate::stats::peripherals::Kind;
        for k in [Kind::Headset, Kind::Mouse, Kind::Keyboard, Kind::Gamepad, Kind::Phone, Kind::Other] {
            for shape in kind_shapes(k) {
                if let Path(d) | Fill(d) = shape {
                    let commands = d.chars().filter(|c| c.is_ascii_alphabetic()).count();
                    assert!(parse(d).len() >= commands, "{k:?}: {d}");
                }
            }
        }
    }
}
