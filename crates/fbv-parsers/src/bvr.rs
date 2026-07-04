//! Boardview Raw (`.bvr`) — inflex's open interchange format.
//! v1: `<<Layout>>` / `<<Pin>>` / `<<Nail>>` sections, coordinates in inches
//! (scaled x1000 to mils). v3 (`BVRAW_FORMAT_3`): key/value records per part
//! and pin, plus outline point/segment lists, already in mils.

use crate::cursor::Cursor;
use crate::text::{add_nails_as_pins, ltrim, split_lines, RawNail};
use crate::{find_in_buf, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};

pub fn verify_v1(buf: &[u8]) -> bool {
    find_in_buf("BVRAW_FORMAT_1", buf)
}

pub fn verify_v3(buf: &[u8]) -> bool {
    find_in_buf("BVRAW_FORMAT_3", buf)
}

pub fn parse_v1(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    let lines = split_lines(bytes);
    let mut b = BoardBuilder::new();
    let mut outline_points: Vec<Point> = Vec::new();
    let mut nails: Vec<RawNail> = Vec::new();
    let mut current_block = 0u8;
    let mut prev_part_name = String::new();
    let mut last_part: Option<u32> = None;
    let mut last_part_side = Side::Top;

    let mut i = 0usize;
    while i < lines.len() {
        let line = ltrim(lines[i]);
        i += 1;
        if line.is_empty() {
            continue;
        }
        match line {
            b"<<Layout>>" => {
                current_block = 1;
                i += 1;
                continue;
            }
            b"<<Pin>>" => {
                current_block = 2;
                i += 1;
                continue;
            }
            b"<<Nail>>" => {
                current_block = 3;
                i += 1;
                continue;
            }
            _ => {}
        }

        let mut c = Cursor::new(line);
        match current_block {
            1 => {
                let x = c.read_double() * 1000.0;
                if c.peek() == Some(b',') {
                    c.advance(1);
                }
                let y = c.read_double() * 1000.0;
                outline_points.push(Point::new(x.trunc() as f32, y.trunc() as f32));
            }
            2 => {
                let name = c.read_token();
                let loc = c.read_token();
                let side = if loc == "(T)" { Side::Top } else { Side::Bottom };
                if name != prev_part_name {
                    last_part = Some(b.add_part(Part {
                        refdes: name.clone(),
                        mfg_code: None,
                        value: None,
                        package: None,
                        side,
                        pins: vec![],
                        is_dummy: false,
                    }));
                    last_part_side = side;
                    prev_part_name = name;
                }
                if let Some(part) = last_part {
                    let _id = c.read_int();
                    let pin_name = c.read_token();
                    let x = c.read_double() * 1000.0;
                    let y = c.read_double() * 1000.0;
                    let _layer = c.read_int();
                    let net_name = c.read_token();
                    let net = b.net_id(&net_name);
                    b.add_pin(Pin {
                        part,
                        number: pin_name,
                        name: String::new(),
                        pos: Point::new(x.trunc() as f32, y.trunc() as f32),
                        radius: 0.5,
                        side: last_part_side,
                        net,
                        is_test_pad: false,
                    });
                }
            }
            3 => {
                c.next_tab_field();
                let x = c.read_double() * 1000.0;
                let y = c.read_double() * 1000.0;
                let _type = c.read_int();
                let _grid = c.read_token();
                let loc = c.read_token();
                let side = if loc == "(T)" { Side::Top } else { Side::Bottom };
                let _net_id = c.read_token();
                let net = c.read_token();
                nails.push(RawNail {
                    pos: Point::new(x as f32, y as f32),
                    side,
                    net,
                    probe: 0,
                });
            }
            _ => {}
        }
    }

    if current_block == 0 {
        return Err(ParseError::Unrecognized);
    }
    b.add_outline_polyline(&outline_points);
    add_nails_as_pins(&mut b, &nails);
    Ok(b.finish(BoardFormat::Bvr))
}

#[derive(Default, Clone)]
struct V3Part {
    name: String,
    side: Side,
    origin: Point,
    is_th: bool,
}

pub fn parse_v3(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    let mut b = BoardBuilder::new();
    let mut part = V3Part::default();
    let mut part_idx: Option<u32> = None;

    #[derive(Default)]
    struct V3Pin {
        number: String,
        name: String,
        side: Option<Side>,
        pos: Point,
        radius: f32,
        net: String,
    }
    let mut pin = V3Pin::default();
    let mut pending_pins: Vec<V3Pin> = Vec::new();
    let mut any = false;

    for line in split_lines(bytes) {
        let line = ltrim(line);
        if line.is_empty() {
            continue;
        }
        let mut c = Cursor::new(line);
        let take_prefix = |c: &mut Cursor, p: &str| -> bool {
            if c.starts_with(p) {
                c.advance(p.len());
                true
            } else {
                false
            }
        };

        if take_prefix(&mut c, "PART_NAME ") {
            part.name = c.read_token();
            any = true;
        } else if take_prefix(&mut c, "PART_SIDE ") {
            part.side = match c.read_token().as_str() {
                "T" => Side::Top,
                "B" => Side::Bottom,
                _ => Side::Both,
            };
        } else if take_prefix(&mut c, "PART_ORIGIN ") {
            part.origin = Point::new(c.read_double().trunc() as f32, c.read_double().trunc() as f32);
        } else if take_prefix(&mut c, "PART_MOUNT ") {
            part.is_th = c.read_token() != "SMD";
        } else if take_prefix(&mut c, "PIN_NUMBER ") {
            pin.number = c.read_token();
        } else if take_prefix(&mut c, "PIN_NAME ") {
            pin.name = c.read_token();
        } else if take_prefix(&mut c, "PIN_SIDE ") {
            pin.side = Some(match c.read_token().as_str() {
                "T" => Side::Top,
                "B" => Side::Bottom,
                _ => Side::Both,
            });
        } else if take_prefix(&mut c, "PIN_ORIGIN ") {
            pin.pos = Point::new(
                c.read_double().trunc() as f32 + part.origin.x,
                c.read_double().trunc() as f32 + part.origin.y,
            );
        } else if take_prefix(&mut c, "PIN_RADIUS ") {
            pin.radius = c.read_double() as f32;
        } else if take_prefix(&mut c, "PIN_NET ") {
            pin.net = c.read_token();
        } else if line == b"PIN_END" {
            pending_pins.push(std::mem::take(&mut pin));
        } else if line == b"PART_END" {
            let side = if part.is_th { Side::Both } else { part.side };
            let idx = b.add_part(Part {
                refdes: std::mem::take(&mut part.name),
                mfg_code: None,
                value: None,
                package: None,
                side,
                pins: vec![],
                is_dummy: false,
            });
            for p in pending_pins.drain(..) {
                let net = b.net_id(&p.net);
                b.add_pin(Pin {
                    part: idx,
                    number: p.number,
                    name: p.name,
                    pos: p.pos,
                    radius: if p.radius > 0.0 { p.radius } else { 0.5 },
                    side: p.side.unwrap_or(side),
                    net,
                    is_test_pad: false,
                });
            }
            part = V3Part::default();
            part_idx = Some(idx);
        } else if take_prefix(&mut c, "OUTLINE_POINTS ") {
            let mut pts = Vec::new();
            loop {
                let before = c.pos();
                let x = c.read_double();
                let y = c.read_double();
                if c.pos() == before {
                    break;
                }
                pts.push(Point::new(x.trunc() as f32, y.trunc() as f32));
            }
            b.add_outline_polyline(&pts);
            any = true;
        } else if take_prefix(&mut c, "OUTLINE_SEGMENTED ") {
            loop {
                let before = c.pos();
                let x1 = c.read_double();
                let y1 = c.read_double();
                let x2 = c.read_double();
                let y2 = c.read_double();
                if c.pos() == before {
                    break;
                }
                b.add_outline_segment(
                    Point::new(x1.trunc() as f32, y1.trunc() as f32),
                    Point::new(x2.trunc() as f32, y2.trunc() as f32),
                );
            }
            any = true;
        }
    }
    let _ = part_idx;

    if !any {
        return Err(ParseError::Unrecognized);
    }
    Ok(b.finish(BoardFormat::Bvr3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_key_value_records() {
        let s = "\
BVRAW_FORMAT_3
OUTLINE_SEGMENTED 0 0 1000 0 1000 0 1000 800
PART_NAME U5300
PART_SIDE T
PART_ORIGIN 100 200
PART_MOUNT SMD
PIN_ID 1
PIN_NUMBER 1
PIN_NAME VIN
PIN_SIDE T
PIN_ORIGIN 5 5
PIN_RADIUS 3
PIN_NET PPBUS_G3H
PIN_END
PART_END
";
        assert!(verify_v3(s.as_bytes()));
        let m = parse_v3(s.as_bytes()).unwrap();
        assert_eq!(m.parts.len(), 1);
        assert_eq!(m.pins.len(), 1);
        assert_eq!(m.pins[0].pos.x, 105.0);
        assert_eq!(m.pins[0].number, "1");
        assert_eq!(m.nets[0].name, "PPBUS_G3H");
        assert_eq!(m.outline.len(), 2);
    }

    #[test]
    fn v1_sections() {
        let s = "\
BVRAW_FORMAT_1
<<Layout>>
skip
0.0,0.0
10.0,8.0
<<Pin>>
skip
U5300 (T) 1 A1 1.0 2.0 1 PPBUS_G3H
U5300 (T) 2 A2 1.1 2.0 1 GND
C1 (B) 1 1 3.0 3.0 2 PPBUS_G3H
";
        assert!(verify_v1(s.as_bytes()));
        let m = parse_v1(s.as_bytes()).unwrap();
        assert_eq!(m.parts.len(), 2);
        assert_eq!(m.parts[0].pins.len(), 2);
        assert_eq!(m.pins[0].pos.x, 1000.0);
    }
}
