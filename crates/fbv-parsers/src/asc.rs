//! PADS-derived `.asc` dumps — a *directory* format: `format.asc` (outline),
//! `pins.asc` (parts + pins), `nails.asc` (test probes), each with fixed
//! header-line counts. The entry file can be any of them; siblings are
//! loaded through the `ParseContext::companion` callback so the parser stays
//! filesystem-agnostic (and the indexer can sandbox it).

use crate::cursor::Cursor;
use crate::text::{add_nails_as_pins, ltrim, split_lines, RawNail};
use crate::{ParseContext, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};

pub fn parse(entry: &[u8], ctx: &ParseContext) -> Result<BoardModel, ParseError> {
    let companion = ctx.companion.ok_or_else(|| {
        ParseError::Io("ASC directory format needs sibling files (format/pins/nails.asc)".into())
    })?;

    // The entry buffer stands in for whichever sibling matches it; simplest
    // robust approach: always request all three by name.
    let format_buf = companion("format.asc");
    let pins_buf = companion("pins.asc");
    let nails_buf = companion("nails.asc");
    let _ = entry;

    let pins_data = pins_buf.ok_or_else(|| {
        ParseError::Io("pins.asc not found next to the .asc file".into())
    })?;

    let mut b = BoardBuilder::new();
    let mut nails: Vec<RawNail> = Vec::new();

    // format.asc: 7+1 header lines skipped, then x/y pairs in inches.
    if let Some(data) = format_buf {
        let lines = split_lines(&data);
        let mut outline = Vec::new();
        let mut first = true;
        let mut i = 0usize;
        while i < lines.len() {
            let line = ltrim(lines[i]);
            i += 1;
            if line.is_empty() {
                continue;
            }
            if first {
                i += 7;
                first = false;
                continue;
            }
            let mut c = Cursor::new(line);
            let x = c.read_double() * 1000.0;
            let y = c.read_double() * 1000.0;
            outline.push(Point::new(x as f32, y as f32));
        }
        b.add_outline_polyline(&outline);
    }

    // pins.asc: 7+1 header lines, then "Part <name> (T|B)" / pin lines.
    {
        let lines = split_lines(&pins_data);
        let mut first = true;
        let mut last_part: Option<u32> = None;
        let mut last_side = Side::Top;
        let mut i = 0usize;
        while i < lines.len() {
            let line = ltrim(lines[i]);
            i += 1;
            if line.is_empty() {
                continue;
            }
            if first {
                i += 7;
                first = false;
                continue;
            }
            let mut c = Cursor::new(line);
            if line.starts_with(b"Part") {
                c.advance(4);
                let name = c.read_token();
                let loc = c.read_token();
                let side = if loc == "(T)" { Side::Top } else { Side::Bottom };
                last_side = side;
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
                    side: last_side,
                    net,
                    is_test_pad: false,
                });
            }
        }
    }

    // nails.asc: 6+1 header lines, then probe records.
    if let Some(data) = nails_buf {
        let lines = split_lines(&data);
        let mut first = true;
        let mut i = 0usize;
        while i < lines.len() {
            let line = ltrim(lines[i]);
            i += 1;
            if line.is_empty() {
                continue;
            }
            if first {
                i += 6;
                first = false;
                continue;
            }
            let mut c = Cursor::new(&line[1.min(line.len())..]);
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
    }

    if b.part_count() == 0 {
        return Err(ParseError::Malformed("pins.asc contained no parts".into()));
    }
    add_nails_as_pins(&mut b, &nails);
    b.generate_outline_from_pins();
    Ok(b.finish(BoardFormat::Asc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn loads_sibling_files() {
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        let h8 = "h\n".repeat(8);
        let h7 = "h\n".repeat(7);
        files.insert(
            "format.asc".into(),
            format!("first\n{h8}0.0 0.0\n10.0 0.0\n10.0 8.0\n").into_bytes(),
        );
        files.insert(
            "pins.asc".into(),
            format!(
                "first\n{h8}Part U1 (T)\n1 1 1.0 2.0 1 PPBUS_G3H 0\n1 2 1.1 2.0 1 GND 0\n"
            )
            .into_bytes(),
        );
        files.insert(
            "nails.asc".into(),
            format!("first\n{h7}$1 6.0 1.0 0 g (B) 5 PP3V3_S5\n").into_bytes(),
        );

        let loader = |name: &str| files.get(&name.to_ascii_lowercase()).cloned();
        let ctx = ParseContext {
            companion: Some(&loader),
            ..Default::default()
        };
        let m = parse(files["pins.asc"].as_slice(), &ctx).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 1);
        assert_eq!(m.parts[0].pins.len(), 2);
        assert_eq!(m.pins[0].pos.x, 1000.0);
        let tp = m.pins.iter().find(|p| p.is_test_pad).unwrap();
        assert_eq!(tp.side, Side::Bottom);
    }

    #[test]
    fn missing_siblings_is_io_error() {
        let loader = |_: &str| None;
        let ctx = ParseContext {
            companion: Some(&loader),
            ..Default::default()
        };
        assert!(matches!(
            parse(b"", &ctx),
            Err(ParseError::Io(_))
        ));
    }
}
