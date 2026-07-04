//! CST BoardViewer `.cst` — little-endian binary: a parts section, a nets
//! name table, then a "CPad" pin section of fixed 16-byte records. The
//! outline is not stored; one is generated around the pins. Detection is by
//! extension only (no reliable magic), so the parser is defensive.

use crate::text::add_nails_as_pins;
use crate::ParseError;
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};

struct Bin<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Bin<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn u16(&mut self) -> Result<u16, ParseError> {
        let b = self
            .buf
            .get(self.pos..self.pos + 2)
            .ok_or_else(|| ParseError::Malformed("unexpected end of CST file".into()))?;
        self.pos += 2;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16, ParseError> {
        Ok(self.u16()? as i16)
    }
    fn u8(&mut self) -> Result<u8, ParseError> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| ParseError::Malformed("unexpected end of CST file".into()))?;
        self.pos += 1;
        Ok(b)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let b = self
            .buf
            .get(self.pos..self.pos + n)
            .ok_or_else(|| ParseError::Malformed("unexpected end of CST file".into()))?;
        self.pos += n;
        Ok(b)
    }
    fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        if self.pos + n > self.buf.len() {
            return Err(ParseError::Malformed("unexpected end of CST file".into()));
        }
        self.pos += n;
        Ok(())
    }
    fn back(&mut self, n: usize) -> Result<(), ParseError> {
        if n > self.pos {
            return Err(ParseError::Malformed("CST section underflow".into()));
        }
        self.pos -= n;
        Ok(())
    }
}

pub fn parse(bytes: &[u8]) -> Result<BoardModel, ParseError> {
    let mut r = Bin::new(bytes);
    let num_parts = r.u16()? as usize;
    if num_parts == 0 || num_parts > 100_000 {
        return Err(ParseError::Malformed(format!(
            "implausible CST part count {num_parts}"
        )));
    }
    r.skip(4)?; // section signature
    let section_name_len = r.u16()? as usize;
    r.skip(section_name_len)?;

    let mut b = BoardBuilder::new();
    let mut part_sides: Vec<Side> = Vec::new();

    for _ in 0..num_parts {
        let name_len = r.u8()? as usize;
        let name = String::from_utf8_lossy(r.bytes(name_len)?).into_owned();
        r.skip(4)?;
        let layer = r.u8()?;
        let side = match layer {
            0x0c => Side::Top,
            0x01 => Side::Bottom,
            _ => Side::Both,
        };
        b.add_part(Part {
            refdes: name,
            mfg_code: None,
            value: None,
            package: None,
            side,
            pins: vec![],
            is_dummy: false,
        });
        part_sides.push(side);
        r.skip(6)?;
    }

    r.back(2)?; // last skip overlaps the net-count word
    let num_nets = r.u16()? as usize;
    if num_nets > 1_000_000 {
        return Err(ParseError::Malformed("implausible CST net count".into()));
    }
    let mut net_names: Vec<String> = Vec::with_capacity(num_nets);
    for _ in 0..num_nets {
        let name_len = r.u8()? as usize;
        // In the C parser each name's length byte doubles as the previous
        // name's terminator; lengths are exact here.
        let name = String::from_utf8_lossy(r.bytes(name_len)?).into_owned();
        net_names.push(name);
    }

    // Dummy part for orphan pins, mirroring OBV.
    let dummy = b.add_part(Part {
        refdes: "...".into(),
        mfg_code: None,
        value: None,
        package: None,
        side: Side::Both,
        pins: vec![],
        is_dummy: true,
    });
    part_sides.push(Side::Both);

    // Find the "CPad" section marker.
    let marker = b"CPad";
    let start = r.pos;
    let rel = bytes[start..]
        .windows(marker.len())
        .position(|w| w == marker)
        .ok_or_else(|| ParseError::Malformed("CST CPad section not found".into()))?;
    r.pos = start + rel;
    r.back(8)?;
    let num_pins = r.u16()? as usize;
    if num_pins > 2_000_000 {
        return Err(ParseError::Malformed("implausible CST pin count".into()));
    }
    r.skip(10)?;

    for _ in 0..num_pins {
        let part_id = r.i16()?;
        let probe = r.i16()?;
        let net_id = r.i16()? as usize;
        let x = r.i16()? as f32;
        let y = r.i16()? as f32;
        let _shape = r.i16()?;
        r.skip(4)?;

        let part = if part_id >= 0 {
            let idx = part_id as u32;
            if (idx as usize) >= part_sides.len() {
                return Err(ParseError::Malformed("CST pin part out of range".into()));
            }
            idx
        } else {
            dummy
        };
        let net_name = net_names
            .get(net_id)
            .ok_or_else(|| ParseError::Malformed("CST pin net out of range".into()))?;
        let net = b.net_id(net_name);
        let side = part_sides[part as usize];
        b.add_pin(Pin {
            part,
            number: if probe > 0 { probe.to_string() } else { String::new() },
            name: String::new(),
            pos: Point::new(x, y),
            radius: 0.5,
            side,
            net,
            is_test_pad: part == dummy,
        });
    }

    add_nails_as_pins(&mut b, &[]);
    b.generate_outline_from_pins();
    Ok(b.finish(BoardFormat::Cst))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal synthetic CST image matching the parser's walk.
    fn build_sample() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&2u16.to_le_bytes()); // num_parts
        v.extend_from_slice(&[0; 4]); // section signature
        v.extend_from_slice(&4u16.to_le_bytes()); // section name len
        v.extend_from_slice(b"Comp");
        // part 0
        v.push(5);
        v.extend_from_slice(b"U5300");
        v.extend_from_slice(&[0; 4]);
        v.push(0x0c); // top
        v.extend_from_slice(&[0; 6]);
        // part 1
        v.push(4);
        v.extend_from_slice(b"C100");
        v.extend_from_slice(&[0; 4]);
        v.push(0x01); // bottom
        v.extend_from_slice(&[0; 6]);
        // net table: parser backs up 2 bytes into the last skip
        let net_count_pos = v.len() - 2;
        v[net_count_pos..net_count_pos + 2].copy_from_slice(&2u16.to_le_bytes());
        v.push(9);
        v.extend_from_slice(b"PPBUS_G3H");
        v.push(3);
        v.extend_from_slice(b"GND");
        // pin section header: [num_pins u16 @ marker-8][6 pad][CPad], pins
        // start at marker+4 (the parser does back(8), u16, skip(10)).
        v.extend_from_slice(&3u16.to_le_bytes());
        v.extend_from_slice(&[0; 6]);
        v.extend_from_slice(b"CPad");
        // pins: part_id, probe, net_id, x, y, shape, 4 pad
        for (part, net, x) in [(0i16, 0i16, 100i16), (0, 1, 110), (1, 0, 500)] {
            v.extend_from_slice(&part.to_le_bytes());
            v.extend_from_slice(&1i16.to_le_bytes());
            v.extend_from_slice(&net.to_le_bytes());
            v.extend_from_slice(&x.to_le_bytes());
            v.extend_from_slice(&200i16.to_le_bytes());
            v.extend_from_slice(&0i16.to_le_bytes());
            v.extend_from_slice(&[0; 4]);
        }
        v
    }

    #[test]
    fn parses_synthetic_cst() {
        let m = parse(&build_sample()).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
        assert_eq!(m.parts[0].refdes, "U5300");
        let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
        assert_eq!(ppbus.pins.len(), 2);
        assert!(!m.outline.is_empty());
    }

    #[test]
    fn garbage_is_rejected_not_panicking() {
        assert!(parse(b"").is_err());
        assert!(parse(&[0xff; 64]).is_err());
        assert!(parse(b"\x02\x00short").is_err());
    }
}
