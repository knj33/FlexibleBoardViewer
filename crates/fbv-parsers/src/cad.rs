//! Samsung `.cad` — plain text with per-line record types (`COMP`, `C_PIN`,
//! `NET`, `N_VIA`). Coordinates in inches (x1000 to mils). No board outline
//! in the file; one is generated around the pins.

use crate::cursor::Cursor;
use crate::text::{add_nails_as_pins, ltrim, split_lines, RawNail};
use crate::{find_in_buf, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};
use std::collections::HashMap;

pub fn verify(buf: &[u8]) -> bool {
    find_in_buf("###Panel Added", buf) && find_in_buf("C_PIN", buf)
}

/// Samsung CAD writes nets as "/NETNAME"; the leading slash is dropped.
fn clean_net(mut net: String) -> String {
    if net.contains('/') {
        net.remove(0);
    }
    net
}

pub fn parse(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    const MULTIPLIER: f64 = 1000.0;
    let mut b = BoardBuilder::new();
    let mut parts_id: HashMap<String, u32> = HashMap::new();
    let mut part_sides: Vec<Side> = Vec::new();
    let mut nails: Vec<RawNail> = Vec::new();
    let mut nail_net = String::new();
    let mut any = false;

    for line in split_lines(bytes) {
        let line = ltrim(line);
        if line.is_empty() {
            continue;
        }
        let mut c = Cursor::new(line);

        if line.starts_with(b"COMP") {
            let _type = c.read_token();
            let name = c.read_token();
            let _part_nr = c.read_token();
            let _u1 = c.read_token();
            let _u2 = c.read_token();
            let _x = c.read_token();
            let _y = c.read_token();
            let loc = c.read_token();
            let side = if loc == "1" { Side::Top } else { Side::Bottom };
            let idx = b.add_part(Part {
                refdes: name.clone(),
                mfg_code: None,
                value: None,
                package: None,
                side,
                pins: vec![],
                is_dummy: false,
            });
            parts_id.insert(name, idx);
            part_sides.push(side);
            any = true;
        } else if line.starts_with(b"C_PIN") {
            let _type = c.read_token();
            let pin_id = c.read_token();
            // "U5300-A7" -> part "U5300"
            let part_name = pin_id.split('-').next().unwrap_or("").to_string();
            let Some(&part) = parts_id.get(&part_name) else {
                return Err(ParseError::Malformed(format!(
                    "C_PIN references unknown part {part_name}"
                )));
            };
            let x = c.read_double() * MULTIPLIER;
            let y = c.read_double() * MULTIPLIER;
            let _u1 = c.read_double();
            let _u2 = c.read_double();
            let _u3 = c.read_double();
            let _u4 = c.read_token();
            let net_name = clean_net(c.read_token());
            let side = part_sides[part as usize];
            let net = b.net_id(&net_name);
            b.add_pin(Pin {
                part,
                number: pin_id.split('-').nth(1).unwrap_or("").to_string(),
                name: String::new(),
                pos: Point::new(x as f32, y as f32),
                radius: 0.5,
                side,
                net,
                is_test_pad: false,
            });
            any = true;
        } else if line.starts_with(b"NET ") {
            let _type = c.read_token();
            nail_net = clean_net(c.read_token());
        } else if line.starts_with(b"N_VIA") {
            let _type = c.read_token();
            let x = c.read_double() * MULTIPLIER;
            let y = c.read_double() * MULTIPLIER;
            let _s = c.read_token();
            let side = if c.read_double() == 1.0 { Side::Top } else { Side::Bottom };
            nails.push(RawNail {
                pos: Point::new(x as f32, y as f32),
                side,
                net: nail_net.clone(),
                probe: 0,
            });
        }
    }

    if !any {
        return Err(ParseError::Unrecognized);
    }
    add_nails_as_pins(&mut b, &nails);
    b.generate_outline_from_pins();
    Ok(b.finish(BoardFormat::Cad))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
###Panel Added
COMP U5300 TPS51225 x x 1.0 2.0 1 0
C_PIN U5300-1 1.00 2.00 0 0 0 x /PPBUS_G3H
C_PIN U5300-2 1.05 2.00 0 0 0 x /GND
COMP C100 CAP x x 3.0 2.0 2 0
C_PIN C100-1 3.00 2.00 0 0 0 x /PPBUS_G3H
NET /PP3V3_S5 x
N_VIA 4.0 1.0 x 1 0
";

    #[test]
    fn parses_sample() {
        assert!(verify(SAMPLE.as_bytes()));
        let m = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
        assert_eq!(m.pins[0].pos.x, 1000.0);
        assert_eq!(m.pins[0].number, "1");
        let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
        assert_eq!(ppbus.pins.len(), 2);
        let via_net = m.nets.iter().find(|n| n.name == "PP3V3_S5").unwrap();
        assert_eq!(via_net.pins.len(), 1);
        assert!(!m.outline.is_empty());
    }
}
