//! GenCAD 1.4 (`.cad` / `.gcd`) — the documented industry interchange format
//! and the most common thing actually shipped as ".cad" boardviews.
//! Sections: `$HEADER` (units), `$BOARD` (outline), `$PADS`/`$PADSTACKS`
//! (pad geometry, drill), `$SHAPES` (footprints with PIN offsets),
//! `$COMPONENTS` (placements), `$SIGNALS` (net ↔ component/pin),
//! `$DEVICES` (part number / value / package), `$ROUTES` (vias).
//!
//! Placement math (rotation + MIRRORX/MIRRORY + FLIP semantics) is ported
//! 1:1 from OpenBoardView's GenCADFile. Unlike OBV we also read `$DEVICES`
//! so components carry real part numbers/values into the library index.

use crate::text::{add_nails_as_pins, RawNail};
use crate::{find_in_buf, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};
use std::collections::{HashMap, HashSet};

pub fn verify(buf: &[u8]) -> bool {
    find_in_buf("GENCAD", buf) && find_in_buf("$HEADER", buf)
}

#[derive(Clone, Copy, PartialEq)]
enum Units {
    Thou,
    Inch,
    Mm,
    Mm100,
    User(f64),
    UserM(f64),
    UserMm(f64),
}

impl Units {
    /// Board units -> mils, matching OBV's conversion table.
    fn to_mils(self, v: f64) -> f64 {
        match self {
            Units::Thou => v,
            Units::Inch => v * 1000.0,
            Units::Mm => v * (100.0 / 2.54),
            Units::Mm100 => v * (1.0 / 2.54),
            Units::User(n) => (n * v) / 1000.0,
            Units::UserM(n) => n * v * (10.0 / 2.54),
            Units::UserMm(n) => n * v * (1.0 / 2.54),
        }
    }
}

/// Splits a GenCAD statement into tokens; double-quoted strings (which may
/// contain spaces) form one token.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Rest of the line after the leading keyword, unquoted and trimmed —
/// signal/device names run to end-of-line in real files.
fn rest_after_keyword(line: &str) -> String {
    let trimmed = line.trim_start();
    let after = trimmed
        .split_once(|c: char| c.is_whitespace())
        .map(|(_, r)| r)
        .unwrap_or("");
    after.trim().trim_matches('"').to_string()
}

fn num(tok: Option<&String>) -> f64 {
    tok.and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0)
}

struct ShapePin {
    name: String,
    pad: String,
    x: f64,
    y: f64,
}

#[derive(Default)]
struct Component {
    refdes: String,
    device: Option<String>,
    place: (f64, f64),
    bottom: bool,
    rotation: f64,
    shape: Option<String>,
    mirror_x: bool,
    mirror_y: bool,
    flip: bool,
}

#[derive(Default, Clone)]
struct DeviceInfo {
    part: Option<String>,
    value: Option<String>,
    package: Option<String>,
}

#[derive(Default, Clone, Copy)]
struct PadstackInfo {
    drilled: bool,
    top: bool,
    bottom: bool,
}

impl PadstackInfo {
    fn side(&self) -> Side {
        match (self.top, self.bottom) {
            (true, true) => Side::Both,
            (true, false) => Side::Top,
            (false, true) => Side::Bottom,
            // Inner layers only: still probe-able knowledge-wise, show both.
            (false, false) => Side::Both,
        }
    }
}

/// 0.1-rad arc tessellation, as in OBV's BRDFileBase::arc_to_segments.
fn arc_segments(p1: Point, p2: Point, pc: Point) -> Vec<(Point, Point)> {
    let r = ((p1.x - pc.x).powi(2) + (p1.y - pc.y).powi(2)).sqrt() as f64;
    let start = (p1.y as f64 - pc.y as f64).atan2(p1.x as f64 - pc.x as f64);
    let mut end = (p2.y as f64 - pc.y as f64).atan2(p2.x as f64 - pc.x as f64);
    if end < start {
        end += 2.0 * std::f64::consts::PI;
    }
    let mut out = Vec::new();
    let mut prev = p1;
    let mut a = start + 0.1;
    while a < end {
        let p = Point::new(
            (pc.x as f64 + r * a.cos()) as f32,
            (pc.y as f64 + r * a.sin()) as f32,
        );
        out.push((prev, p));
        prev = p;
        a += 0.1;
    }
    out.push((prev, p2));
    out
}

pub fn parse(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    let text = String::from_utf8_lossy(bytes);
    let mut units = Units::Thou; // OBV default when UNITS is absent/unknown

    #[derive(PartialEq, Clone, Copy)]
    enum Section {
        None,
        Header,
        Board,
        Pads,
        Padstacks,
        Shapes,
        Components,
        Signals,
        Devices,
        Routes,
        Other,
    }
    let mut section = Section::None;

    let mut outline: Vec<(Point, Point)> = Vec::new();
    let mut pad_radius: HashMap<String, f64> = HashMap::new(); // $PADS name -> mils
    let mut padstacks: HashMap<String, PadstackInfo> = HashMap::new();
    let mut pads_in_stack: HashMap<String, Vec<String>> = HashMap::new();
    let mut shapes: HashMap<String, Vec<ShapePin>> = HashMap::new();
    let mut components: Vec<Component> = Vec::new();
    let mut signals: HashMap<(String, String), String> = HashMap::new(); // (refdes, pin) -> net
    let mut devices: HashMap<String, DeviceInfo> = HashMap::new();
    let mut nails: Vec<RawNail> = Vec::new();

    let mut cur_pad: Option<String> = None;
    let mut cur_padstack: Option<String> = None;
    let mut cur_shape: Option<String> = None;
    let mut cur_signal: Option<String> = None;
    let mut cur_device: Option<String> = None;
    let mut cur_route: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('$') {
            section = match rest.trim() {
                "HEADER" => Section::Header,
                "BOARD" => Section::Board,
                "PADS" => Section::Pads,
                "PADSTACKS" => Section::Padstacks,
                "SHAPES" => Section::Shapes,
                "COMPONENTS" => Section::Components,
                "SIGNALS" => Section::Signals,
                "DEVICES" => Section::Devices,
                "ROUTES" => Section::Routes,
                s if s.starts_with("END") => Section::None,
                _ => Section::Other,
            };
            continue;
        }
        let toks = tokens(line);
        let Some(keyword) = toks.first().map(|s| s.as_str()) else {
            continue;
        };

        match section {
            Section::Header => {
                if keyword == "UNITS" {
                    units = match toks.get(1).map(|s| s.as_str()) {
                        Some("INCH") => Units::Inch,
                        Some("THOU") => Units::Thou,
                        Some("MM") => Units::Mm,
                        Some("MM100") => Units::Mm100,
                        Some("USER") => Units::User(num(toks.get(2))),
                        Some("USERM") => Units::UserM(num(toks.get(2))),
                        Some("USERMM") => Units::UserMm(num(toks.get(2))),
                        _ => Units::Thou,
                    };
                }
            }
            Section::Board => match keyword {
                "LINE" => {
                    let p1 = Point::new(
                        units.to_mils(num(toks.get(1))) as f32,
                        units.to_mils(num(toks.get(2))) as f32,
                    );
                    let p2 = Point::new(
                        units.to_mils(num(toks.get(3))) as f32,
                        units.to_mils(num(toks.get(4))) as f32,
                    );
                    outline.push((p1, p2));
                }
                "RECTANGLE" => {
                    let x = units.to_mils(num(toks.get(1))) as f32;
                    let y = units.to_mils(num(toks.get(2))) as f32;
                    let w = units.to_mils(num(toks.get(3))) as f32;
                    let h = units.to_mils(num(toks.get(4))) as f32;
                    let p1 = Point::new(x, y);
                    let p2 = Point::new(x, y + h);
                    let p3 = Point::new(x + w, y + h);
                    let p4 = Point::new(x + w, y);
                    outline.extend([(p1, p2), (p2, p3), (p3, p4), (p4, p1)]);
                }
                "ARC" => {
                    let p1 = Point::new(
                        units.to_mils(num(toks.get(1))) as f32,
                        units.to_mils(num(toks.get(2))) as f32,
                    );
                    let p2 = Point::new(
                        units.to_mils(num(toks.get(3))) as f32,
                        units.to_mils(num(toks.get(4))) as f32,
                    );
                    let pc = Point::new(
                        units.to_mils(num(toks.get(5))) as f32,
                        units.to_mils(num(toks.get(6))) as f32,
                    );
                    outline.extend(arc_segments(p1, p2, pc));
                }
                _ => {}
            },
            Section::Pads => match keyword {
                // PAD <name> <type> <drill>, then geometry lines
                "PAD" => {
                    cur_pad = toks.get(1).cloned();
                }
                // CIRCLE <x> <y> <radius> inside a PAD gives its radius
                "CIRCLE" => {
                    if let Some(pad) = &cur_pad {
                        let r = units.to_mils(num(toks.get(3)));
                        let entry = pad_radius.entry(pad.clone()).or_insert(0.0);
                        if r > *entry {
                            *entry = r;
                        }
                    }
                }
                _ => {}
            },
            Section::Padstacks => match keyword {
                // PADSTACK <name> <drill>
                "PADSTACK" => {
                    let name = toks.get(1).cloned().unwrap_or_default();
                    let drilled = num(toks.get(2)) != 0.0;
                    padstacks.insert(
                        name.clone(),
                        PadstackInfo {
                            drilled,
                            top: false,
                            bottom: false,
                        },
                    );
                    cur_padstack = Some(name);
                }
                // PAD <pad_name> <layer> <rot> <mirror>
                "PAD" => {
                    if let Some(stack) = &cur_padstack {
                        if let Some(pad) = toks.get(1) {
                            pads_in_stack
                                .entry(stack.clone())
                                .or_default()
                                .push(pad.clone());
                        }
                        if let Some(ps) = padstacks.get_mut(stack) {
                            match toks.get(2).map(|s| s.as_str()) {
                                Some("TOP") => ps.top = true,
                                Some("BOTTOM") => ps.bottom = true,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            },
            Section::Shapes => match keyword {
                "SHAPE" => {
                    let name = toks.get(1).cloned().unwrap_or_default();
                    shapes.entry(name.clone()).or_default();
                    cur_shape = Some(name);
                }
                // PIN <pin_name> <pad_name> <x> <y> <layer> <rot> <mirror>
                "PIN" => {
                    if let Some(shape) = cur_shape.as_ref().and_then(|n| shapes.get_mut(n)) {
                        shape.push(ShapePin {
                            name: toks.get(1).cloned().unwrap_or_default(),
                            pad: toks.get(2).cloned().unwrap_or_default(),
                            x: units.to_mils(num(toks.get(3))),
                            y: units.to_mils(num(toks.get(4))),
                        });
                    }
                }
                _ => {}
            },
            Section::Components => match keyword {
                "COMPONENT" => {
                    components.push(Component {
                        refdes: toks.get(1).cloned().unwrap_or_default(),
                        ..Default::default()
                    });
                }
                "DEVICE" => {
                    if let Some(c) = components.last_mut() {
                        c.device = Some(rest_after_keyword(line));
                    }
                }
                "PLACE" => {
                    if let Some(c) = components.last_mut() {
                        c.place = (
                            units.to_mils(num(toks.get(1))),
                            units.to_mils(num(toks.get(2))),
                        );
                    }
                }
                "LAYER" => {
                    if let Some(c) = components.last_mut() {
                        c.bottom = toks.get(1).map(|s| s == "BOTTOM").unwrap_or(false);
                    }
                }
                "ROTATION" => {
                    if let Some(c) = components.last_mut() {
                        c.rotation = num(toks.get(1));
                    }
                }
                "SHAPE" => {
                    if let Some(c) = components.last_mut() {
                        c.shape = toks.get(1).cloned();
                        for t in toks.iter().skip(2) {
                            match t.as_str() {
                                "MIRRORX" => c.mirror_x = true,
                                "MIRRORY" => c.mirror_y = true,
                                "FLIP" => c.flip = true,
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            },
            Section::Signals => match keyword {
                "SIGNAL" => {
                    cur_signal = Some(rest_after_keyword(line));
                }
                // NODE <component> <pin>
                "NODE" => {
                    if let (Some(sig), Some(comp), Some(pin)) =
                        (&cur_signal, toks.get(1), toks.get(2))
                    {
                        signals.insert((comp.clone(), pin.clone()), sig.clone());
                    }
                }
                _ => {}
            },
            Section::Devices => match keyword {
                "DEVICE" => {
                    let name = rest_after_keyword(line);
                    devices.entry(name.clone()).or_default();
                    cur_device = Some(name);
                }
                "PART" => {
                    if let Some(d) = cur_device.as_ref().and_then(|n| devices.get_mut(n)) {
                        let p = rest_after_keyword(line);
                        if !p.is_empty() {
                            d.part = Some(p);
                        }
                    }
                }
                "VALUE" | "Value" => {
                    if let Some(d) = cur_device.as_ref().and_then(|n| devices.get_mut(n)) {
                        let v = rest_after_keyword(line);
                        if !v.is_empty() {
                            d.value = Some(v);
                        }
                    }
                }
                "PACKAGE" => {
                    if let Some(d) = cur_device.as_ref().and_then(|n| devices.get_mut(n)) {
                        let v = rest_after_keyword(line);
                        if !v.is_empty() {
                            d.package = Some(v);
                        }
                    }
                }
                _ => {}
            },
            Section::Routes => match keyword {
                "ROUTE" => {
                    cur_route = Some(rest_after_keyword(line));
                }
                // VIA <pad_name> <x> <y> <layer> <drill> <via_name>
                "VIA" => {
                    if let Some(net) = &cur_route {
                        nails.push(RawNail {
                            pos: Point::new(
                                units.to_mils(num(toks.get(2))) as f32,
                                units.to_mils(num(toks.get(3))) as f32,
                            ),
                            side: Side::Both,
                            net: net.clone(),
                            probe: 0,
                        });
                    }
                }
                _ => {}
            },
            Section::None | Section::Other => {}
        }
    }

    if components.is_empty() && shapes.is_empty() {
        return Err(ParseError::Malformed(
            "GenCAD file contains no $COMPONENTS/$SHAPES data".into(),
        ));
    }

    let mut b = BoardBuilder::new();
    for seg in &outline {
        b.add_outline_segment(seg.0, seg.1);
    }

    // Duplicate guard, as in OBV: same side+position+shape appears twice in
    // some exports.
    let mut seen: HashSet<(bool, i64, i64, String)> = HashSet::new();

    let lookup_device = |name: &Option<String>| -> DeviceInfo {
        let Some(name) = name else {
            return DeviceInfo::default();
        };
        if let Some(d) = devices.get(name) {
            return d.clone();
        }
        // OBV works around CAMCAD exports whose COMPONENT->DEVICE refs have
        // spaces mangled to underscores (or vice versa).
        let normalized = name.replace(' ', "_");
        devices
            .iter()
            .find(|(k, _)| k.replace(' ', "_") == normalized)
            .map(|(_, d)| d.clone())
            .unwrap_or_default()
    };

    for comp in &components {
        let shape_pins = comp.shape.as_ref().and_then(|s| shapes.get(s));
        if let Some(shape_name) = &comp.shape {
            let key = (
                comp.bottom,
                comp.place.0 as i64,
                comp.place.1 as i64,
                shape_name.clone(),
            );
            if !seen.insert(key) {
                continue; // duplicate component
            }
        }

        // Through-hole when any pin's padstack is drilled (OBV: SMD iff none).
        let is_th = shape_pins
            .map(|pins| {
                pins.iter().any(|p| {
                    padstacks
                        .get(&p.pad)
                        .map(|ps| ps.drilled)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        let part_side = if is_th {
            Side::Both
        } else if comp.bottom {
            Side::Bottom
        } else {
            Side::Top
        };

        let dev = lookup_device(&comp.device);
        let part_idx = b.add_part(Part {
            refdes: comp.refdes.clone(),
            mfg_code: dev.part.clone().or_else(|| comp.device.clone()),
            value: dev.value.clone(),
            package: dev
                .package
                .clone()
                .or_else(|| comp.shape.clone()),
            side: part_side,
            pins: vec![],
            is_dummy: false,
        });

        let Some(shape_pins) = shape_pins else {
            continue;
        };

        // OBV's placement math, verbatim: exactly one mirror mirrors the
        // rotation angle across pi.
        let mut rot_rads = comp.rotation.to_radians();
        let mx: f64 = if comp.mirror_x { -1.0 } else { 1.0 };
        let my: f64 = if comp.mirror_y { -1.0 } else { 1.0 };
        if mx * my < 0.0 {
            rot_rads = std::f64::consts::PI - rot_rads;
        }
        let (sin, cos) = rot_rads.sin_cos();

        for sp in shape_pins {
            let x = comp.place.0 + mx * (sp.x * cos - sp.y * sin);
            let y = comp.place.1 + my * (sp.x * sin + sp.y * cos);

            let net_name = signals
                .get(&(comp.refdes.clone(), sp.name.clone()))
                .cloned()
                .unwrap_or_default();
            let net = b.net_id(&net_name);

            let mut side = padstacks
                .get(&sp.pad)
                .map(|ps| ps.side())
                .unwrap_or(part_side);
            if comp.flip {
                side = match side {
                    Side::Top => Side::Bottom,
                    Side::Bottom => Side::Top,
                    Side::Both => Side::Both,
                };
            }

            // Pin pad ref may name a $PADS entry directly, or a $PADSTACKS
            // stack whose member pads carry the geometry.
            let radius = pad_radius.get(&sp.pad).copied().unwrap_or_else(|| {
                pads_in_stack
                    .get(&sp.pad)
                    .map(|pads| {
                        pads.iter()
                            .filter_map(|p| pad_radius.get(p))
                            .fold(0.0f64, |a, &b| a.max(b))
                    })
                    .unwrap_or(0.0)
            });
            b.add_pin(Pin {
                part: part_idx,
                number: sp.name.clone(),
                name: String::new(),
                pos: Point::new(x as f32, y as f32),
                radius: if radius > 0.5 { (radius / 2.0) as f32 } else { 0.5 },
                side,
                net,
                is_test_pad: false,
            });
        }
    }

    add_nails_as_pins(&mut b, &nails);
    b.generate_outline_from_pins();
    Ok(b.finish(BoardFormat::GenCad))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"$HEADER
GENCAD 1.4
USER "FlexibleBoardViewer test"
UNITS THOU
$ENDHEADER
$BOARD
LINE 0 0 2000 0
LINE 2000 0 2000 1000
LINE 2000 1000 0 1000
LINE 0 1000 0 0
$ENDBOARD
$PADS
PAD P50 ROUND 0
CIRCLE 0 0 25
PAD PTH60 ROUND 30
CIRCLE 0 0 30
$ENDPADS
$PADSTACKS
PADSTACK PS_SMT 0
PAD P50 TOP 0 0
PADSTACK PS_TH 30
PAD PTH60 TOP 0 0
PAD PTH60 BOTTOM 0 0
$ENDPADSTACKS
$SHAPES
SHAPE SOT23
PIN 1 PS_SMT -30 0
PIN 2 PS_SMT 30 0
SHAPE CONN2
PIN 1 PS_TH 0 0
PIN 2 PS_TH 0 100
$ENDSHAPES
$COMPONENTS
COMPONENT Q6001
DEVICE DEV_MOSFET
PLACE 500 300
LAYER TOP
ROTATION 90
SHAPE SOT23 0 0
COMPONENT Q6002
DEVICE DEV_MOSFET
PLACE 900 300
LAYER BOTTOM
ROTATION 0
SHAPE SOT23 MIRRORX 0
COMPONENT J1
DEVICE DEV_CONN
PLACE 1500 500
LAYER TOP
ROTATION 0
SHAPE CONN2 0 0
$ENDCOMPONENTS
$SIGNALS
SIGNAL PPBUS_G3H
NODE Q6001 1
NODE Q6002 1
NODE J1 1
SIGNAL GND
NODE Q6001 2
$ENDSIGNALS
$DEVICES
DEVICE DEV_MOSFET
PART FDMC510P
VALUE P-CH 30V
PACKAGE SOT23
DEVICE DEV_CONN
PART CONN-2P
$ENDDEVICES
$ROUTES
ROUTE PPBUS_G3H
VIA PS_SMT 1200 800 TOP 0 v1
$ENDROUTES
"#;

    #[test]
    fn verifies() {
        assert!(verify(SAMPLE.as_bytes()));
        assert!(!verify(b"COMP U1\nC_PIN U1-1 1 2"));
    }

    #[test]
    fn parses_components_pins_nets_devices() {
        let m = parse(SAMPLE.as_bytes()).unwrap();
        let real: Vec<_> = m.parts.iter().filter(|p| !p.is_dummy).collect();
        assert_eq!(real.len(), 3);

        // Device enrichment from $DEVICES — the donor-search payoff.
        let q1 = real.iter().find(|p| p.refdes == "Q6001").unwrap();
        assert_eq!(q1.mfg_code.as_deref(), Some("FDMC510P"));
        assert_eq!(q1.value.as_deref(), Some("P-CH 30V"));
        assert_eq!(q1.package.as_deref(), Some("SOT23"));
        assert_eq!(q1.side, Side::Top);

        // Rotation 90°: pin offset (-30, 0) rotates to (0, -30).
        let q1_pin1 = m
            .pins
            .iter()
            .find(|p| p.part == 0 && p.number == "1")
            .unwrap();
        assert!((q1_pin1.pos.x - 500.0).abs() < 0.01, "{}", q1_pin1.pos.x);
        assert!((q1_pin1.pos.y - 270.0).abs() < 0.01, "{}", q1_pin1.pos.y);

        // MIRRORX (mirror about X axis): rot' = pi, x-sign -1 => offsets
        // (-30,0) -> (+(-30*cos pi)) ... net effect: x offset preserved.
        let q2 = real.iter().find(|p| p.refdes == "Q6002").unwrap();
        assert_eq!(q2.side, Side::Bottom);

        // Through-hole connector: drilled padstack => part on Both sides.
        let j1 = real.iter().find(|p| p.refdes == "J1").unwrap();
        assert_eq!(j1.side, Side::Both);
        let j1_pin = m
            .pins
            .iter()
            .find(|p| m.parts[p.part as usize].refdes == "J1")
            .unwrap();
        assert_eq!(j1_pin.side, Side::Both);

        // Nets: PPBUS_G3H = Q6001.1 + Q6002.1 + J1.1 + via test pad.
        let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
        assert_eq!(ppbus.pins.len(), 4);
        assert!(m.pins.iter().any(|p| p.is_test_pad));

        // Pad radius flowed from $PADS CIRCLE (25 thou circle -> r 12.5).
        assert!((q1_pin1.radius - 12.5).abs() < 0.01);

        // Outline present from $BOARD.
        assert_eq!(m.outline.len(), 4);
    }

    #[test]
    fn units_mm() {
        let sample = SAMPLE.replace("UNITS THOU", "UNITS MM");
        let m = parse(sample.as_bytes()).unwrap();
        // PLACE 500 300 in mm -> 500 * 39.37 mils.
        let q1_pin2 = m
            .pins
            .iter()
            .find(|p| p.part == 0 && p.number == "2")
            .unwrap();
        // place.x = 500mm -> 19685.04 mils; pin 2 offset (30,0) rotated 90° -> (0, 30mm)
        assert!((q1_pin2.pos.x - 500.0 * 100.0 / 2.54).abs() < 1.0);
    }

    #[test]
    fn quoted_and_spaced_names() {
        let s = r#"$HEADER
GENCAD 1.4
UNITS THOU
$ENDHEADER
$SHAPES
SHAPE "small cap"
PIN 1 P1 0 0
$ENDSHAPES
$COMPONENTS
COMPONENT C1
DEVICE CAP 100NF 16V
PLACE 10 10
LAYER TOP
ROTATION 0
SHAPE "small cap" 0 0
$ENDCOMPONENTS
$SIGNALS
SIGNAL SOME NET WITH SPACES
NODE C1 1
$ENDSIGNALS
$DEVICES
DEVICE CAP 100NF 16V
PART GRM155R61C104KA88
$ENDDEVICES
"#;
        let m = parse(s.as_bytes()).unwrap();
        let c1 = m.parts.iter().find(|p| p.refdes == "C1").unwrap();
        assert_eq!(c1.mfg_code.as_deref(), Some("GRM155R61C104KA88"));
        assert_eq!(m.nets[0].name, "SOME NET WITH SPACES");
        assert_eq!(m.pins.len(), 1);
    }
}
