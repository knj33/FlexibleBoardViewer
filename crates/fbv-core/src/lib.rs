//! Core domain model: the normalized board representation ("IR") that every
//! parser produces and that the renderer, index and search layers consume.
//!
//! Coordinates are in mils (thousandths of an inch), Y-up, viewed from the
//! top side. The renderer applies flip/mirror/rotation; parsers must never
//! bake a view transform into the data beyond this normalization.

pub mod identity;
pub mod netclass;

use serde::{Deserialize, Serialize};

/// Bump when `BoardModel` changes incompatibly; stale caches re-parse lazily.
pub const MODEL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Side {
    #[default]
    Top,
    Bottom,
    /// Through-hole: visible/probe-able from both sides.
    Both,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Top => "top",
            Side::Bottom => "bottom",
            Side::Both => "both",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BoardFormat {
    Brd,
    Brd2,
    Bdv,
    Bvr,
    Bvr3,
    Asc,
    Cad,
    Cst,
    Fz,
    XzzPcb,
}

impl BoardFormat {
    pub fn label(self) -> &'static str {
        match self {
            BoardFormat::Brd => "BRD (Test_Link)",
            BoardFormat::Brd2 => "BRD2",
            BoardFormat::Bdv => "BDV (Toptest)",
            BoardFormat::Bvr => "BVR",
            BoardFormat::Bvr3 => "BVR3",
            BoardFormat::Asc => "ASC",
            BoardFormat::Cad => "CAD (Samsung)",
            BoardFormat::Cst => "CST",
            BoardFormat::Fz => "FZ (ASUS)",
            BoardFormat::XzzPcb => "XZZ PCB",
        }
    }
}

/// Reference into `BoardModel::nets`. `NO_NET` marks unconnected pins.
pub type NetId = u32;
pub const NO_NET: NetId = u32::MAX;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub refdes: String,
    /// Manufacturer code / part description when the format carries it (FZ descr block).
    pub mfg_code: Option<String>,
    pub value: Option<String>,
    pub package: Option<String>,
    pub side: Side,
    /// Indices into `BoardModel::pins`, always contiguous ranges in practice.
    pub pins: Vec<u32>,
    /// True for the synthetic "..." parts that group loose test pads/nails.
    pub is_dummy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pin {
    /// Index into `BoardModel::parts`.
    pub part: u32,
    /// Pin number as printed ("1", "A3", ...). Empty when unknown.
    pub number: String,
    /// Functional name when distinct from the number (BGA signal names).
    pub name: String,
    pub pos: Point,
    pub radius: f32,
    pub side: Side,
    pub net: NetId,
    pub is_test_pad: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Net {
    pub name: String,
    /// Indices into `BoardModel::pins`.
    pub pins: Vec<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardModel {
    pub format_label: String,
    pub outline: Vec<(Point, Point)>,
    pub parts: Vec<Part>,
    pub pins: Vec<Pin>,
    pub nets: Vec<Net>,
    pub warnings: Vec<String>,
}

impl BoardModel {
    /// Axis-aligned bounds of everything drawable. None for an empty board.
    pub fn bounds(&self) -> Option<(Point, Point)> {
        let mut min = Point::new(f32::MAX, f32::MAX);
        let mut max = Point::new(f32::MIN, f32::MIN);
        let mut any = false;
        let mut extend = |p: Point| {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        };
        for (a, b) in &self.outline {
            extend(*a);
            extend(*b);
            any = true;
        }
        for pin in &self.pins {
            extend(pin.pos);
            any = true;
        }
        any.then_some((min, max))
    }

    /// Bounds of one part, derived from its pins (boardview formats rarely
    /// carry part outlines).
    pub fn part_bounds(&self, part_idx: usize) -> Option<(Point, Point)> {
        let part = self.parts.get(part_idx)?;
        let mut min = Point::new(f32::MAX, f32::MAX);
        let mut max = Point::new(f32::MIN, f32::MIN);
        if part.pins.is_empty() {
            return None;
        }
        for &pi in &part.pins {
            let p = self.pins[pi as usize].pos;
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        Some((min, max))
    }

    pub fn net_bounds(&self, net: NetId) -> Option<(Point, Point)> {
        let n = self.nets.get(net as usize)?;
        let mut min = Point::new(f32::MAX, f32::MAX);
        let mut max = Point::new(f32::MIN, f32::MIN);
        if n.pins.is_empty() {
            return None;
        }
        for &pi in &n.pins {
            let p = self.pins[pi as usize].pos;
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        Some((min, max))
    }
}

/// Incrementally builds a `BoardModel`, interning net names and wiring the
/// part<->pin<->net cross references. All parsers go through this; it is the
/// single place where net-name canonicalization happens.
#[derive(Default)]
pub struct BoardBuilder {
    model: BoardModel,
    net_ids: std::collections::HashMap<String, NetId>,
}

impl BoardBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        self.model.warnings.push(msg.into());
    }

    pub fn add_outline_segment(&mut self, a: Point, b: Point) {
        self.model.outline.push((a, b));
    }

    /// Adds a closed/open polyline as segments.
    pub fn add_outline_polyline(&mut self, points: &[Point]) {
        for w in points.windows(2) {
            self.model.outline.push((w[0], w[1]));
        }
    }

    pub fn add_part(&mut self, part: Part) -> u32 {
        self.model.parts.push(part);
        (self.model.parts.len() - 1) as u32
    }

    pub fn part_count(&self) -> usize {
        self.model.parts.len()
    }

    pub fn last_part_mut(&mut self) -> Option<&mut Part> {
        self.model.parts.last_mut()
    }

    pub fn part_mut(&mut self, idx: u32) -> Option<&mut Part> {
        self.model.parts.get_mut(idx as usize)
    }

    /// Interns a net name. Unconnected markers collapse to `NO_NET`.
    pub fn net_id(&mut self, raw_name: &str) -> NetId {
        let name = raw_name.trim();
        if name.is_empty() || netclass::is_no_connect(name) {
            return NO_NET;
        }
        if let Some(&id) = self.net_ids.get(name) {
            return id;
        }
        let id = self.model.nets.len() as NetId;
        self.model.nets.push(Net {
            name: name.to_string(),
            pins: Vec::new(),
        });
        self.net_ids.insert(name.to_string(), id);
        id
    }

    pub fn add_pin(&mut self, mut pin: Pin) -> u32 {
        let idx = self.model.pins.len() as u32;
        if pin.radius <= 0.0 {
            pin.radius = 0.5;
        }
        if pin.net != NO_NET {
            self.model.nets[pin.net as usize].pins.push(idx);
        }
        let part = pin.part as usize;
        if part < self.model.parts.len() {
            self.model.parts[part].pins.push(idx);
        }
        self.model.pins.push(pin);
        idx
    }

    /// Fabricates a rectangular outline around the pins for formats that
    /// carry none (FZ, CAD, CST). Margin matches OpenBoardView's 20 mil.
    pub fn generate_outline_from_pins(&mut self) {
        const MARGIN: f32 = 20.0;
        if !self.model.outline.is_empty() || self.model.pins.is_empty() {
            return;
        }
        let mut min = Point::new(f32::MAX, f32::MAX);
        let mut max = Point::new(f32::MIN, f32::MIN);
        for pin in &self.model.pins {
            min.x = min.x.min(pin.pos.x);
            min.y = min.y.min(pin.pos.y);
            max.x = max.x.max(pin.pos.x);
            max.y = max.y.max(pin.pos.y);
        }
        min.x -= MARGIN;
        min.y -= MARGIN;
        max.x += MARGIN;
        max.y += MARGIN;
        let corners = [
            Point::new(min.x, min.y),
            Point::new(max.x, min.y),
            Point::new(max.x, max.y),
            Point::new(min.x, max.y),
            Point::new(min.x, min.y),
        ];
        self.add_outline_polyline(&corners);
    }

    pub fn finish(mut self, format: BoardFormat) -> BoardModel {
        self.model.format_label = format.label().to_string();
        // Parts with no explicit side inherit the majority side of their pins.
        for part in &mut self.model.parts {
            if part.pins.is_empty() {
                continue;
            }
            let (mut top, mut bottom) = (0usize, 0usize);
            for &pi in &part.pins {
                match self.model.pins[pi as usize].side {
                    Side::Top => top += 1,
                    Side::Bottom => bottom += 1,
                    Side::Both => {}
                }
            }
            if part.side == Side::Both && top != bottom {
                // Keep Both for genuine through-hole parts (mixed pin sides
                // don't occur there; each pin reports Both as well).
                let all_both = part
                    .pins
                    .iter()
                    .all(|&pi| self.model.pins[pi as usize].side == Side::Both);
                if !all_both {
                    part.side = if top >= bottom { Side::Top } else { Side::Bottom };
                }
            }
        }
        self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_wires_cross_references() {
        let mut b = BoardBuilder::new();
        let part = b.add_part(Part {
            refdes: "U1".into(),
            mfg_code: None,
            value: None,
            package: None,
            side: Side::Top,
            pins: vec![],
            is_dummy: false,
        });
        let net = b.net_id("PPBUS_G3H");
        let same_net = b.net_id("  PPBUS_G3H ");
        assert_eq!(net, same_net);
        assert_eq!(b.net_id("NC"), NO_NET);
        assert_eq!(b.net_id(""), NO_NET);
        b.add_pin(Pin {
            part,
            number: "1".into(),
            name: String::new(),
            pos: Point::new(10.0, 20.0),
            radius: 5.0,
            side: Side::Top,
            net,
            is_test_pad: false,
        });
        let m = b.finish(BoardFormat::Brd);
        assert_eq!(m.parts[0].pins, vec![0]);
        assert_eq!(m.nets[0].pins, vec![0]);
        assert_eq!(m.nets[0].name, "PPBUS_G3H");
    }

    #[test]
    fn generated_outline_wraps_pins() {
        let mut b = BoardBuilder::new();
        let part = b.add_part(Part {
            refdes: "R1".into(),
            mfg_code: None,
            value: None,
            package: None,
            side: Side::Top,
            pins: vec![],
            is_dummy: false,
        });
        for (x, y) in [(0.0, 0.0), (100.0, 50.0)] {
            b.add_pin(Pin {
                part,
                number: String::new(),
                name: String::new(),
                pos: Point::new(x, y),
                radius: 1.0,
                side: Side::Top,
                net: NO_NET,
                is_test_pad: false,
            });
        }
        b.generate_outline_from_pins();
        let m = b.finish(BoardFormat::Fz);
        let (min, max) = m.bounds().unwrap();
        assert!(min.x <= -20.0 && max.x >= 120.0);
    }
}
