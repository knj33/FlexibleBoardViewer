//! Toptest `.bdv` — sections `<<format.asc>>`, `<<pins.asc>>`,
//! `<<nails.asc>>` with fixed header-line skips. Files in the wild are
//! obfuscated with a rolling-key byte transform (`decode_bdv` in OBV); the
//! transform is applied only when the encoded signature is present (the
//! encoded form of `<<format.asc>` is the magic string `dd:1.3?,r?-=bb`).

use crate::cursor::Cursor;
use crate::text::{add_nails_as_pins, ltrim, split_lines, RawNail};
use crate::{find_in_buf, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};

const ENCODED_SIG: &str = "dd:1.3?,r?-=bb";

pub fn verify(buf: &[u8]) -> bool {
    find_in_buf(ENCODED_SIG, buf)
        || (find_in_buf("<<format.asc>>", buf) && find_in_buf("<<pins.asc>>", buf))
}

/// Rolling-subtraction obfuscation; self-inverse because the key stream
/// depends only on `\r\n` positions, which the transform preserves.
fn decode(buf: &mut [u8]) {
    let mut count: i32 = 0xa0;
    for i in 0..buf.len() {
        if buf[i] == b'\r' && buf.get(i + 1) == Some(&b'\n') {
            count += 1;
        }
        let x = buf[i];
        let x = if x == b'\r' || x == b'\n' || x == 0 {
            x
        } else {
            (count - (x as i8 as i32)) as u8
        };
        if count > 285 {
            count = 159;
        }
        buf[i] = x;
    }
}

pub fn parse(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    let mut data = bytes.to_vec();
    if find_in_buf(ENCODED_SIG, &data) {
        decode(&mut data);
    }

    let lines = split_lines(&data);
    let mut b = BoardBuilder::new();
    let mut outline_points: Vec<Point> = Vec::new();
    let mut nails: Vec<RawNail> = Vec::new();
    let mut current_block = 0u8;
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
            b"<<format.asc>>" => {
                current_block = 1;
                i += 8; // fixed unused header lines
                continue;
            }
            b"<<pins.asc>>" => {
                current_block = 2;
                i += 8;
                continue;
            }
            b"<<nails.asc>>" => {
                current_block = 3;
                i += 7;
                continue;
            }
            _ => {}
        }

        let mut c = Cursor::new(line);
        match current_block {
            1 => {
                let x = c.read_double() * 1000.0;
                let y = c.read_double() * 1000.0;
                outline_points.push(Point::new(x as f32, y as f32));
            }
            2 => {
                if line.starts_with(b"Part") {
                    c.advance(4);
                    let name = c.read_token();
                    let loc = c.read_token();
                    let side = if loc == "(T)" { Side::Top } else { Side::Bottom };
                    last_part_side = side;
                    last_part = Some(b.add_part(Part {
                        refdes: name,
                        mfg_code: None,
                        value: None,
                        package: None,
                        side,
                        pins: vec![],
                        is_dummy: false,
                    }));
                } else if let Some(part) = last_part {
                    let _id = c.read_int();
                    let number = c.read_token();
                    let x = c.read_double() * 1000.0;
                    let y = c.read_double() * 1000.0;
                    let _layer = c.read_int();
                    let net_name = c.read_token();
                    let _probe = c.read_int();
                    let net = b.net_id(&net_name);
                    b.add_pin(Pin {
                        part,
                        number,
                        name: String::new(),
                        pos: Point::new(x as f32, y as f32),
                        radius: 0.5,
                        side: last_part_side,
                        net,
                        is_test_pad: false,
                    });
                }
            }
            3 => {
                let mut c = Cursor::new(&line[1.min(line.len())..]); // skip first char
                let probe = c.read_int();
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
                    probe,
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
    Ok(b.finish(BoardFormat::Bdv))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        let headers8 = "h\n".repeat(8);
        let headers7 = "h\n".repeat(7);
        format!(
            "<<format.asc>>\n{headers8}0.0 0.0\n10.5 0.0\n10.5 8.0\n0.0 8.0\n\
             <<pins.asc>>\n{headers8}Part U5300 (T)\n1 A1 1.0 2.0 1 PPBUS_G3H 0\n2 A2 1.1 2.0 1 GND 0\n\
             Part C7010 (B)\n1 1 5.0 3.0 2 PPBUS_G3H 0\n\
             <<nails.asc>>\n{headers7}$1 6.0 1.0 0 g1 (T) 5 PP3V3_S5\n"
        )
    }

    #[test]
    fn parses_plain_sample() {
        let s = sample();
        assert!(verify(s.as_bytes()));
        let m = parse(s.as_bytes()).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
        assert_eq!(m.parts[0].refdes, "U5300");
        assert_eq!(m.parts[1].side, Side::Bottom);
        assert_eq!(m.pins[0].pos.x, 1000.0); // inches * 1000 => mils
        let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
        assert_eq!(ppbus.pins.len(), 2);
        assert!(m.pins.last().unwrap().is_test_pad);
    }

    #[test]
    fn decode_is_self_inverse_and_encoded_sample_parses() {
        // Encoded files use \r\n line endings (the key stream advances on
        // them); the transform must round-trip and the encoded signature
        // must appear.
        let s = sample().replace('\n', "\r\n");
        let mut enc = s.as_bytes().to_vec();
        decode(&mut enc);
        assert!(
            find_in_buf(ENCODED_SIG, &enc),
            "encoded form of <<format.asc> must yield the magic signature"
        );
        let mut roundtrip = enc.clone();
        decode(&mut roundtrip);
        assert_eq!(roundtrip, s.as_bytes());

        let m = parse(&enc).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
    }
}
