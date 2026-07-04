//! ASUS `.fz` — RC6-CFB-style stream cipher over the whole file (some files
//! ship unencrypted, detectable by the zlib magic at offset 4), then two
//! independent zlib streams: pin/part content and a part-description table.
//! The 44-word RC6 key schedule is user-supplied (Settings), exactly like
//! OpenBoardView's `FZKey` config entry; only a parity fingerprint of the
//! real key ships in source.

use crate::cursor::Cursor;
use crate::text::{add_nails_as_pins, ltrim, split_lines, RawNail};
use crate::{ParseContext, ParseError};
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};
use flate2::read::ZlibDecoder;
use std::collections::HashMap;
use std::io::Read;

/// Per-word parity of the genuine key (from OpenBoardView), used to verify a
/// user-entered key without embedding the key itself.
const KEY_PARITY: [u32; 44] = [
    0, 1, 1, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 0, 0, 0, 1, 0,
    0, 0, 1, 0, 0, 0, 1, 0, 0, 1, 1, 0, 1,
];

pub fn check_key(key: &[u32; 44]) -> bool {
    key.iter().zip(KEY_PARITY.iter()).all(|(&w, &p)| {
        let mut t = w;
        t ^= t >> 16;
        t ^= t >> 8;
        t ^= t >> 4;
        t ^= t >> 2;
        t ^= t >> 1;
        ((!t) & 1) == p
    })
}

/// RC6-based self-synchronizing stream decrypt, ported 1:1 from OBV.
#[allow(unused_assignments)] // the final C update mirrors the original algorithm
fn decode(data: &mut [u8], key: &[u32; 44]) {
    const LOGW: u32 = 5;
    const R: usize = 20;
    let (mut a, mut b, mut c, mut d) = (0u32, 0u32, 0u32, 0u32);
    let mut ibuf = [0u8; 16];

    for pos in 0..data.len() {
        b = b.wrapping_add(key[0]);
        d = d.wrapping_add(key[1]);
        for i in 1..=R {
            let t = b.wrapping_mul(b.wrapping_mul(2).wrapping_add(1)).rotate_left(LOGW);
            let u = d.wrapping_mul(d.wrapping_mul(2).wrapping_add(1)).rotate_left(LOGW);
            a = (a ^ t).rotate_left(u & 31).wrapping_add(key[2 * i]);
            c = (c ^ u).rotate_left(t & 31).wrapping_add(key[2 * i + 1]);
            let tmp = a;
            a = b;
            b = c;
            c = d;
            d = tmp;
        }
        a = a.wrapping_add(key[2 * R + 2]);
        c = c.wrapping_add(key[2 * R + 3]);

        let current = data[pos];
        data[pos] = current ^ (a & 0xff) as u8;

        ibuf.copy_within(1.., 0);
        ibuf[15] = current;
        a = u32::from_le_bytes([ibuf[0], ibuf[1], ibuf[2], ibuf[3]]);
        b = u32::from_le_bytes([ibuf[4], ibuf[5], ibuf[6], ibuf[7]]);
        c = u32::from_le_bytes([ibuf[8], ibuf[9], ibuf[10], ibuf[11]]);
        d = u32::from_le_bytes([ibuf[12], ibuf[13], ibuf[14], ibuf[15]]);
    }
}

fn inflate(data: &[u8]) -> Result<Vec<u8>, ParseError> {
    let mut out = Vec::new();
    let mut dec = ZlibDecoder::new(data);
    dec.read_to_end(&mut out)
        .map_err(|e| ParseError::Malformed(format!("zlib: {e}")))?;
    Ok(out)
}

pub fn parse(bytes: &[u8], ctx: &ParseContext) -> Result<BoardModel, ParseError> {
    if bytes.len() < 16 {
        return Err(ParseError::Malformed("file too short".into()));
    }
    let mut data = bytes.to_vec();

    let already_plain = data[4] == 0x78 && (data[5] == 0x9c || data[5] == 0xda);
    if !already_plain {
        let key = ctx.fz_key.ok_or(ParseError::KeyMissing("FZ"))?;
        if !check_key(&key) {
            return Err(ParseError::KeyMissing("FZ"));
        }
        decode(&mut data, &key);
    }

    // Trailing LE32 gives the description-block size; content follows the
    // 4-byte header, description sits at the tail.
    let n = data.len();
    let descr_size = u32::from_le_bytes([data[n - 4], data[n - 3], data[n - 2], data[n - 1]]) as usize;
    if descr_size > n {
        return Err(ParseError::Malformed(
            "bad FZ trailer (wrong key or corrupt file)".into(),
        ));
    }
    let content_end = n - descr_size + 4;
    if content_end < 4 || content_end > n {
        return Err(ParseError::Malformed("bad FZ segmentation".into()));
    }
    let content = inflate(&data[4..content_end])?;
    let descr = inflate(&data[content_end..])?;

    // Regional quirk: some boards use ',' as the decimal separator.
    let content: Vec<u8> = content
        .iter()
        .map(|&b| if b == b',' { b'.' } else { b })
        .collect();

    let mut b = BoardBuilder::new();
    let mut parts_id: HashMap<String, u32> = HashMap::new();
    let mut part_sides: Vec<Side> = Vec::new();
    let mut nails: Vec<RawNail> = Vec::new();
    let mut current_block = 0i8;
    let mut multiplier = 1.0f64;

    for line in split_lines(&content) {
        let line = ltrim(line);
        if line.is_empty() {
            continue;
        }
        if line == b"UNIT:millimeters" {
            multiplier = 25.4; // as in OpenBoardView
        }
        if line[0] == b'A' {
            let rest = &line[2.min(line.len())..];
            current_block = if rest.starts_with(b"REFDES") {
                1
            } else if rest.starts_with(b"NET_NAME") {
                2
            } else if rest.starts_with(b"TESTVIA") {
                3
            } else if rest.starts_with(b"GRAPHIC_DATA_NAME") {
                4
            } else if rest.starts_with(b"CLASS") {
                5
            } else if rest.starts_with(b"LOGOInfo") {
                6
            } else if rest.starts_with(b"UnDrawSym") {
                7
            } else {
                -1
            };
            continue;
        }
        if line[0] != b'S' {
            continue;
        }
        let mut c = Cursor::new(&line[2.min(line.len())..]);

        match current_block {
            1 => {
                let name = c.read_until(b'!');
                let _cic = c.read_until(b'!');
                let _sname = c.read_until(b'!');
                let smirror = c.read_until(b'!');
                let _srotate = c.read_until(b'!');
                let side = if smirror == "YES" { Side::Bottom } else { Side::Top };
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
            }
            2 => {
                let net_name = c.read_until(b'!');
                let part_name = c.read_until(b'!');
                let Some(&part) = parts_id.get(&part_name) else {
                    return Err(ParseError::Malformed(format!(
                        "pin references unknown part {part_name}"
                    )));
                };
                let snum = c.read_until(b'!');
                let name = c.read_until(b'!');
                // Some FZ variants put the BGA position in PIN_NAME with
                // PIN_NUMBER == "0"; use name as the number then.
                let use_name_as_number = snum.is_empty() || snum == "0";
                let x = c.read_double() * multiplier;
                c.eat_delim(b'!');
                let y = c.read_double() * multiplier;
                c.eat_delim(b'!');
                let _probe = c.read_int();
                c.eat_delim(b'!');
                let mut radius = c.read_double() / 100.0;
                if radius < 0.5 {
                    radius = 0.5;
                }
                let side = part_sides[part as usize];
                let net = b.net_id(&net_name);
                b.add_pin(Pin {
                    part,
                    number: if use_name_as_number { name.clone() } else { snum },
                    name: if use_name_as_number { String::new() } else { name },
                    pos: Point::new(x as f32, y as f32),
                    radius: (radius * multiplier) as f32,
                    side,
                    net,
                    is_test_pad: false,
                });
            }
            3 => {
                // Line starts with "Y!" after the "S!" prefix.
                c.eat_delim(b'Y');
                c.eat_delim(b'!');
                let net = c.read_until(b'!');
                let _refdes = c.read_until(b'!');
                let _pinnumber = c.read_int();
                c.eat_delim(b'!');
                let _pinname = c.read_until(b'!');
                let x = c.read_double() * multiplier;
                c.eat_delim(b'!');
                let y = c.read_double() * multiplier;
                c.eat_delim(b'!');
                let loc = c.read_until(b'!');
                let side = if loc == "T" { Side::Top } else { Side::Bottom };
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
        return Err(ParseError::Malformed(
            "no recognizable FZ content blocks (wrong key?)".into(),
        ));
    }

    // Description table: PARTNUMBER \t DESCRIPTION \t QTY \t LOCATIONS \t PARTNUMBER2
    for line in split_lines(&descr).iter().skip(2) {
        let line = ltrim(line);
        if line.is_empty() || line[0] == b's' {
            continue;
        }
        let mut c = Cursor::new(line);
        let partno = c.read_tab_field();
        let description = c.read_tab_field();
        let _qty = c.read_int();
        c.eat_delim(b'\t');
        let locations = c.read_tab_field();
        let _partno2 = c.read_tab_field();
        for loc in locations.split_whitespace() {
            if let Some(&idx) = parts_id.get(loc) {
                if let Some(p) = b.part_mut(idx) {
                    if !partno.is_empty() {
                        p.mfg_code = Some(partno.clone());
                    }
                    if !description.is_empty() {
                        p.value = Some(description.clone());
                    }
                }
            }
        }
    }

    add_nails_as_pins(&mut b, &nails);
    b.generate_outline_from_pins();
    Ok(b.finish(BoardFormat::Fz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    const CONTENT: &str = "\
A!REFDES!CIC!SNAME!MIRROR!ROTATE!
S!U7000!x!sname!NO!0!
S!C7010!x!sname!YES!0!
A!NET_NAME!REFDES!PIN_NUMBER!PIN_NAME!PIN_X!PIN_Y!TEST_POINT!RADIUS!
S!PPBUS_G3H!U7000!1!VIN!100.5!200.0!!600!
S!GND!U7000!2!GND!110.5!200.0!!600!
S!PPBUS_G3H!C7010!0!A1!300.0!50.0!!748!
A!TESTVIA!
S!Y!PP3V3_S5!U7000!0!!400.0!80.0!T!6!
";

    const DESCR: &str = "\
board description line
PARTNUMBER\tDESCRIPTION\tQTY\tLOCATIONS\tPARTNUMBER2
TPS51225\tbuck controller 2ch\t1\tU7000\tTPS51225C\
";

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// Builds an unencrypted FZ image: 4 junk bytes, zlib(content),
    /// zlib(descr), trailing LE32 descr length.
    fn build_plain_fz() -> Vec<u8> {
        let content_z = deflate(CONTENT.as_bytes());
        let descr_z = deflate(DESCR.as_bytes());
        let mut v = vec![0u8; 4];
        v.extend_from_slice(&content_z);
        v.extend_from_slice(&descr_z);
        // The split convention: descr starts at n - trailer + 4, so the
        // trailer stores descr-stream length + 8 (4 junk header bytes worth
        // of slack + the trailer itself).
        let descr_size = (descr_z.len() + 8) as u32;
        v.extend_from_slice(&descr_size.to_le_bytes());
        v
    }

    #[test]
    fn parses_plain_fz_with_part_numbers() {
        let ctx = ParseContext::default();
        let m = parse(&build_plain_fz(), &ctx).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);
        let u7000 = &m.parts[0];
        assert_eq!(u7000.refdes, "U7000");
        assert_eq!(u7000.mfg_code.as_deref(), Some("TPS51225"));
        assert_eq!(u7000.value.as_deref(), Some("buck controller 2ch"));
        assert_eq!(m.parts[1].side, Side::Bottom);
        // Variant pin naming: snum "0" -> BGA name used as number.
        let c7010_pin = &m.pins[m.parts[1].pins[0] as usize];
        assert_eq!(c7010_pin.number, "A1");
        let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
        assert_eq!(ppbus.pins.len(), 2);
        // Test via became a test pad.
        assert!(m.pins.iter().any(|p| p.is_test_pad));
    }

    /// Encrypt helper: the inverse direction of `decode` (cipher feedback:
    /// keystream depends on previous ciphertext, which encrypt must emit).
    #[allow(unused_assignments)]
    fn encode(data: &mut [u8], key: &[u32; 44]) {
        const LOGW: u32 = 5;
        const R: usize = 20;
        let (mut a, mut b, mut c, mut d) = (0u32, 0u32, 0u32, 0u32);
        let mut ibuf = [0u8; 16];
        for pos in 0..data.len() {
            b = b.wrapping_add(key[0]);
            d = d.wrapping_add(key[1]);
            for i in 1..=R {
                let t = b.wrapping_mul(b.wrapping_mul(2).wrapping_add(1)).rotate_left(LOGW);
                let u = d.wrapping_mul(d.wrapping_mul(2).wrapping_add(1)).rotate_left(LOGW);
                a = (a ^ t).rotate_left(u & 31).wrapping_add(key[2 * i]);
                c = (c ^ u).rotate_left(t & 31).wrapping_add(key[2 * i + 1]);
                let tmp = a;
                a = b;
                b = c;
                c = d;
                d = tmp;
            }
            a = a.wrapping_add(key[2 * R + 2]);
            c = c.wrapping_add(key[2 * R + 3]);
            let cipher = data[pos] ^ (a & 0xff) as u8;
            data[pos] = cipher;
            ibuf.copy_within(1.., 0);
            ibuf[15] = cipher; // feedback uses ciphertext
            a = u32::from_le_bytes([ibuf[0], ibuf[1], ibuf[2], ibuf[3]]);
            b = u32::from_le_bytes([ibuf[4], ibuf[5], ibuf[6], ibuf[7]]);
            c = u32::from_le_bytes([ibuf[8], ibuf[9], ibuf[10], ibuf[11]]);
            d = u32::from_le_bytes([ibuf[12], ibuf[13], ibuf[14], ibuf[15]]);
        }
    }

    /// Any 44-word key adjusted so each word matches the parity table.
    fn test_key() -> [u32; 44] {
        let mut key = [0u32; 44];
        for (i, w) in key.iter_mut().enumerate() {
            let mut v = 0x9e37_79b9u32.wrapping_mul(i as u32 + 1);
            let parity_ok = |x: u32| {
                let mut t = x;
                t ^= t >> 16;
                t ^= t >> 8;
                t ^= t >> 4;
                t ^= t >> 2;
                t ^= t >> 1;
                ((!t) & 1) == KEY_PARITY[i]
            };
            if !parity_ok(v) {
                v ^= 1;
            }
            *w = v;
        }
        key
    }

    #[test]
    fn parses_encrypted_fz() {
        let key = test_key();
        assert!(check_key(&key));
        let mut data = build_plain_fz();
        // Avoid the "already plain" sniff: encryption scrambles bytes 4/5.
        encode(&mut data, &key);
        assert!(!(data[4] == 0x78 && (data[5] == 0x9c || data[5] == 0xda)));

        let ctx = ParseContext {
            fz_key: Some(key),
            ..Default::default()
        };
        let m = parse(&data, &ctx).unwrap();
        assert_eq!(m.parts.iter().filter(|p| !p.is_dummy).count(), 2);

        // Without a key: KeyMissing, not a crash.
        let no_key = ParseContext::default();
        assert!(matches!(
            parse(&data, &no_key),
            Err(ParseError::KeyMissing("FZ"))
        ));
    }
}
