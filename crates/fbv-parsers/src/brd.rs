//! Test_Link / Landrex `.brd` — the classic MacBook boardview format.
//! Sections: `str_length:`, `var_data:` (counts), `Format:` (outline
//! polyline), `Parts:`, `Pins:`, `Nails:`. Files may be obfuscated with a
//! fixed byte transform announced by a 4-byte signature.

use crate::cursor::Cursor;
use crate::text::{add_nails_as_pins, ltrim, split_lines, RawNail};
use crate::{find_in_buf, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};

const ENCODED_HEADER: [u8; 4] = [0x23, 0xe2, 0x63, 0x28];

pub fn verify(buf: &[u8]) -> bool {
    buf.starts_with(&ENCODED_HEADER)
        || (find_in_buf("str_length:", buf) && find_in_buf("var_data:", buf))
}

fn decode(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        let x = *b;
        if !(x == b'\r' || x == b'\n' || x == 0) {
            *b = !(((x >> 6) & 3) | (x << 2));
        }
    }
}

struct RawPart {
    name: String,
    side: Side,
    is_th: bool,
}

pub fn parse(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    let mut data = bytes.to_vec();
    if data.starts_with(&ENCODED_HEADER) {
        decode(&mut data);
    }

    let mut current_block = 0u8;
    let mut num_parts = 0i64;
    let mut num_pins = 0i64;

    let mut outline_points: Vec<Point> = Vec::new();
    let mut raw_parts: Vec<RawPart> = Vec::new();
    // (pos, probe, part_index_1based, net_name)
    let mut raw_pins: Vec<(Point, i64, i64, String)> = Vec::new();
    let mut nails: Vec<RawNail> = Vec::new();

    for line in split_lines(&data) {
        let line = ltrim(line);
        if line.is_empty() {
            continue;
        }
        match line {
            b"str_length:" => {
                current_block = 1;
                continue;
            }
            b"var_data:" => {
                current_block = 2;
                continue;
            }
            b"Format:" | b"format:" => {
                current_block = 3;
                continue;
            }
            b"Parts:" | b"Pins1:" => {
                current_block = 4;
                continue;
            }
            b"Pins:" | b"Pins2:" => {
                current_block = 5;
                continue;
            }
            b"Nails:" => {
                current_block = 6;
                continue;
            }
            _ => {}
        }

        let mut c = Cursor::new(line);
        match current_block {
            2 => {
                let _num_format = c.read_int();
                num_parts = c.read_int();
                num_pins = c.read_int();
                let _num_nails = c.read_int();
            }
            3 => {
                let x = c.read_int() as f32;
                let y = c.read_int() as f32;
                outline_points.push(Point::new(x, y));
            }
            4 => {
                if (raw_parts.len() as i64) >= num_parts && num_parts > 0 {
                    return Err(ParseError::Malformed("more parts than declared".into()));
                }
                let name = c.read_token();
                let tmp = c.read_int();
                let _end_of_pins = c.read_int();
                let is_th = (tmp & 0xc) == 0;
                let side = if tmp == 1 || (4..8).contains(&tmp) {
                    Side::Top
                } else if tmp == 2 || tmp >= 8 {
                    Side::Bottom
                } else {
                    Side::Both
                };
                raw_parts.push(RawPart { name, side, is_th });
            }
            5 => {
                if (raw_pins.len() as i64) >= num_pins && num_pins > 0 {
                    return Err(ParseError::Malformed("more pins than declared".into()));
                }
                let x = c.read_int() as f32;
                let y = c.read_int() as f32;
                let probe = c.read_int(); // can be -99
                let part = c.read_int();
                let net = c.read_token();
                raw_pins.push((Point::new(x, y), probe, part, net));
            }
            6 => {
                let probe = c.read_int();
                let x = c.read_int() as f32;
                let y = c.read_int() as f32;
                let side = if c.read_int() == 1 {
                    Side::Top
                } else {
                    Side::Bottom
                };
                let net = c.read_token();
                nails.push(RawNail {
                    pos: Point::new(x, y),
                    side,
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

    for rp in &raw_parts {
        b.add_part(Part {
            refdes: rp.name.clone(),
            mfg_code: None,
            value: None,
            package: None,
            side: rp.side,
            pins: vec![],
            is_dummy: false,
        });
    }

    // Lenovo variant: pins with empty nets inherit the net of the nail with
    // the same probe number.
    let nail_nets: std::collections::HashMap<i64, &str> = nails
        .iter()
        .map(|n| (n.probe, n.net.as_str()))
        .collect();

    for (pos, probe, part_1based, net_name) in &raw_pins {
        let part_idx = (part_1based - 1) as usize;
        if part_idx >= raw_parts.len() {
            return Err(ParseError::Malformed(
                "pin references nonexistent part".into(),
            ));
        }
        let effective_net = if net_name.is_empty() {
            nail_nets.get(probe).copied().unwrap_or("")
        } else {
            net_name.as_str()
        };
        let rp = &raw_parts[part_idx];
        let side = if rp.is_th { Side::Both } else { rp.side };
        let net = b.net_id(effective_net);
        b.add_pin(Pin {
            part: part_idx as u32,
            number: String::new(),
            name: String::new(),
            pos: *pos,
            radius: 0.5,
            side,
            net,
            is_test_pad: false,
        });
    }

    add_nails_as_pins(&mut b, &nails);
    Ok(b.finish(BoardFormat::Brd))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
str_length:
800
var_data:
4 2 3 1

Format:
0 0
1000 0
1000 800
0 800

Parts:
U5300 5 2
C0402 10 3

Pins:
100 200 1 1 PPBUS_G3H
110 200 2 1 GND
500 300 3 2 PPBUS_G3H

Nails:
7 600 100 1 PP3V3_S5
";

    /// Inverse of `decode` (decode is rotl2-then-complement per byte).
    fn encode(buf: &mut [u8]) {
        for b in buf.iter_mut() {
            let x = *b;
            if !(x == b'\r' || x == b'\n' || x == 0) {
                let inv = !x;
                *b = (inv >> 2) | (inv << 6);
            }
        }
    }

    #[test]
    fn verify_plaintext_and_encoded() {
        assert!(verify(SAMPLE.as_bytes()));
        assert!(!verify(b"random junk file"));
    }

    #[test]
    fn decode_roundtrips_and_encoded_files_parse() {
        let mut enc = SAMPLE.as_bytes().to_vec();
        encode(&mut enc);
        let mut dec = enc.clone();
        decode(&mut dec);
        assert_eq!(dec, SAMPLE.as_bytes());

        // A real encoded file begins with the signature; splice the sample
        // in after it the way the decoder sees it: signature bytes decode to
        // garbage on the first line, so prepend a junk first line instead.
        let mut plain_with_junk_header = Vec::new();
        plain_with_junk_header.extend_from_slice(b"####\r\n");
        plain_with_junk_header.extend_from_slice(SAMPLE.as_bytes());
        let mut encoded = plain_with_junk_header.clone();
        encode(&mut encoded);
        // Force the signature over the first four encoded bytes.
        encoded[..4].copy_from_slice(&ENCODED_HEADER);
        assert!(verify(&encoded));
        let m = parse(&encoded).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
    }

    #[test]
    fn parses_sample() {
        let m = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
        assert_eq!(m.parts[0].refdes, "U5300");
        assert_eq!(m.parts[0].side, Side::Top);
        assert_eq!(m.parts[1].side, Side::Bottom);
        // 3 part pins + 1 nail-as-testpad
        assert_eq!(m.pins.len(), 4);
        let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
        assert_eq!(ppbus.pins.len(), 2);
        assert!(m.pins.last().unwrap().is_test_pad);
        assert_eq!(m.outline.len(), 3);
    }

}
