//! BRD2 — text variant with `BRDOUT:`/`NETS:`/`PARTS:`/`PINS:`/`NAILS:`
//! sections. Pins are stored in one flat list; each part records the index
//! where its pin run *starts*. Bottom-side Y coordinates are stored flipped
//! and must be unflipped against the board height.

use crate::cursor::Cursor;
use crate::text::{add_nails_as_pins, ltrim, split_lines, RawNail};
use crate::{find_in_buf, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};
use std::collections::HashMap;

pub fn verify(buf: &[u8]) -> bool {
    find_in_buf("BRDOUT:", buf) && find_in_buf("NETS:", buf)
}

struct RawPart {
    name: String,
    start_of_pins: i64,
    side: Side,
}

pub fn parse(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    let mut current_block = 0u8;
    let mut max = Point::new(0.0, 0.0);

    let mut nets: HashMap<i64, String> = HashMap::new();
    let mut outline_points: Vec<Point> = Vec::new();
    let mut raw_parts: Vec<RawPart> = Vec::new();
    // (pos, net_id, side)
    let mut raw_pins: Vec<(Point, i64, Side)> = Vec::new();
    let mut nails: Vec<RawNail> = Vec::new();

    for line in split_lines(bytes) {
        let line = ltrim(line);
        if line.is_empty() {
            continue;
        }
        let mut c = Cursor::new(line);

        if c.starts_with("BRDOUT:") {
            current_block = 1;
            c.advance(7);
            let _count = c.read_int();
            max.x = c.read_int() as f32;
            max.y = c.read_int() as f32;
            continue;
        }
        if c.starts_with("NETS:") {
            current_block = 2;
            continue;
        }
        if c.starts_with("PARTS:") {
            current_block = 3;
            continue;
        }
        if c.starts_with("PINS:") {
            current_block = 4;
            continue;
        }
        if c.starts_with("NAILS:") {
            current_block = 5;
            continue;
        }

        match current_block {
            1 => {
                let x = c.read_int() as f32;
                let y = c.read_int() as f32;
                if x > max.x || y > max.y {
                    return Err(ParseError::Malformed("outline point out of bounds".into()));
                }
                outline_points.push(Point::new(x, y));
            }
            2 => {
                let id = c.read_int();
                let name = c.read_token();
                nets.insert(id, name);
            }
            3 => {
                let name = c.read_token();
                let _p1x = c.read_int();
                let _p1y = c.read_int();
                let _p2x = c.read_int();
                let _p2y = c.read_int();
                let start_of_pins = c.read_int();
                let side = match c.read_int() {
                    1 => Side::Top,
                    2 => Side::Bottom,
                    _ => Side::Both,
                };
                raw_parts.push(RawPart {
                    name,
                    start_of_pins,
                    side,
                });
            }
            4 => {
                let x = c.read_int() as f32;
                let y = c.read_int() as f32;
                let net_id = c.read_int();
                let side = match c.read_int() {
                    1 => Side::Top,
                    2 => Side::Bottom,
                    _ => Side::Both,
                };
                raw_pins.push((Point::new(x, y), net_id, side));
            }
            5 => {
                let probe = c.read_int();
                let x = c.read_int() as f32;
                let mut y = c.read_int() as f32;
                let net_id = c.read_int();
                let net = nets.get(&net_id).cloned().unwrap_or_default();
                let is_top = c.read_int() == 1;
                if !is_top {
                    y = max.y - y;
                }
                nails.push(RawNail {
                    pos: Point::new(x, y),
                    side: if is_top { Side::Top } else { Side::Bottom },
                    net,
                    probe,
                });
            }
            _ => {}
        }
    }

    if current_block == 0 {
        return Err(ParseError::Unrecognized);
    }

    let mut b = BoardBuilder::new();
    b.add_outline_polyline(&outline_points);

    // Assign pins to parts via the per-part start indices, flipping
    // non-top pin Y coordinates, and detecting through-hole parts (all pins
    // on the opposite side of the part).
    let part_indices: Vec<u32> = raw_parts
        .iter()
        .map(|rp| {
            b.add_part(Part {
                refdes: rp.name.clone(),
                mfg_code: None,
                value: None,
                package: None,
                side: rp.side,
                pins: vec![],
                is_dummy: false,
            })
        })
        .collect();

    let mut cpi = 0usize;
    for (i, rp) in raw_parts.iter().enumerate() {
        let pin_end = if i + 1 < raw_parts.len() {
            raw_parts[i + 1].start_of_pins.max(0) as usize
        } else {
            raw_pins.len()
        };
        let mut is_dip = true;
        let mut part_pins: Vec<(Point, i64, Side)> = Vec::new();
        while cpi < pin_end.min(raw_pins.len()) {
            let (mut pos, net_id, side) = raw_pins[cpi];
            if side != Side::Top {
                pos.y = max.y - pos.y;
            }
            if (side == Side::Top && rp.side == Side::Top)
                || (side == Side::Bottom && rp.side == Side::Bottom)
            {
                is_dip = false;
            }
            part_pins.push((pos, net_id, side));
            cpi += 1;
        }
        for (pos, net_id, side) in part_pins {
            let net_name = nets.get(&net_id).cloned().unwrap_or_default();
            let net = b.net_id(&net_name);
            b.add_pin(Pin {
                part: part_indices[i],
                number: String::new(),
                name: String::new(),
                pos,
                radius: 0.5,
                side: if is_dip { Side::Both } else { side },
                net,
                is_test_pad: false,
            });
        }
        if is_dip {
            // Every pin sits opposite (or nowhere near) the part's side:
            // through-hole part, probe-able from both sides.
            if let Some(p) = b.part_mut(part_indices[i]) {
                p.side = Side::Both;
            }
        }
    }

    add_nails_as_pins(&mut b, &nails);
    Ok(b.finish(BoardFormat::Brd2))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
BRDOUT: 4 1000 800
0 0
1000 0
1000 800
0 800
NETS: 2
1 PPBUS_G3H
2 GND
PARTS: 2
U1 0 0 10 10 0 1
J5 0 0 10 10 2 2
PINS: 4
100 100 1 1
110 100 2 1
500 100 1 2
510 100 2 2
NAILS: 1
3 600 700 1 1
";

    #[test]
    fn verifies_and_parses() {
        assert!(verify(SAMPLE.as_bytes()));
        let m = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
        // Bottom-side pins get unflipped: stored y=100 => real y=700.
        let j5_pins: Vec<_> = m.parts[1].pins.iter().map(|&i| &m.pins[i as usize]).collect();
        assert_eq!(j5_pins[0].pos.y, 700.0);
        let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
        assert_eq!(ppbus.pins.len(), 3); // 2 pins + 1 nail
    }
}
