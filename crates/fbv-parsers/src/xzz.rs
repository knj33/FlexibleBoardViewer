//! XZZ / XinZhiZao `.pcb` — binary container: "XZZPCB" magic (optionally
//! XOR-obfuscated with the byte at 0x10), offset table, typed blocks (arcs,
//! line segments, parts, test pads) plus a net-name table. Part blocks are
//! DES/ECB-encrypted with a user-supplied 64-bit key (parity-verified, not
//! shipped — same policy as the FZ key).
//!
//! Format understanding credited to @inflex, @MuertoGB and @huertas via
//! OpenBoardView.

use crate::text::add_nails_as_pins;
use crate::{ParseContext, ParseError};
use des::cipher::{BlockDecrypt, KeyInit};
use des::Des;
use fbv_core::{BoardBuilder, BoardFormat, BoardModel, Part, Pin, Point, Side};
use std::collections::HashMap;

const SCALE: u32 = 10_000;
const KEY_PARITY: [u8; 8] = [1, 1, 1, 1, 1, 1, 1, 0];

pub fn check_key(key: u64) -> bool {
    (0..8).all(|i| {
        let mut t = ((key >> (i * 8)) & 0xff) as u8;
        t ^= t >> 4;
        t ^= t >> 2;
        t ^= t >> 1;
        ((!t) & 1) == KEY_PARITY[i]
    })
}

pub fn verify(buf: &[u8]) -> bool {
    if buf.len() < 6 {
        return false;
    }
    if &buf[..6] == b"XZZPCB" {
        return true;
    }
    if buf.len() > 0x10 && buf[0x10] != 0 {
        let k = buf[0x10];
        return buf[..6].iter().map(|b| b ^ k).eq(*b"XZZPCB");
    }
    false
}

fn rd_u32(buf: &[u8], pos: usize) -> Result<u32, ParseError> {
    buf.get(pos..pos + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| ParseError::Malformed("unexpected end of XZZ file".into()))
}

fn des_decrypt(data: &[u8], key: u64) -> Vec<u8> {
    let des = Des::new_from_slice(&key.to_be_bytes()).expect("8-byte DES key");
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(8) {
        if chunk.len() == 8 {
            let mut block = *des::cipher::generic_array::GenericArray::from_slice(chunk);
            des.decrypt_block(&mut block);
            out.extend_from_slice(&block);
        } else {
            out.extend_from_slice(chunk); // trailing partial block stays raw
        }
    }
    out
}

/// 10-point arc approximation, as in OBV's xzz_arc_to_segments.
fn arc_segments(
    mut start_deg: i32,
    mut end_deg: i32,
    r: f64,
    c: Point,
) -> Vec<(Point, Point)> {
    const N: i32 = 10;
    if start_deg > end_deg {
        std::mem::swap(&mut start_deg, &mut end_deg);
    }
    if end_deg - start_deg > 180 {
        start_deg += 360;
    }
    let (s, e) = (
        (start_deg as f64).to_radians(),
        (end_deg as f64).to_radians(),
    );
    let step = (e - s) / (N as f64 - 1.0);
    let mut out = Vec::new();
    let mut prev = Point::new(
        (c.x as f64 + r * s.cos()) as f32,
        (c.y as f64 + r * s.sin()) as f32,
    );
    for i in 1..N {
        let a = s + step * i as f64;
        let p = Point::new(
            (c.x as f64 + r * a.cos()) as f32,
            (c.y as f64 + r * a.sin()) as f32,
        );
        out.push((prev, p));
        prev = p;
    }
    out
}

struct PinRec {
    pos: Point,
    number: String,
    net_index: u32,
}

fn parse_pin_block(buf: &[u8], pos: &mut usize) -> Result<PinRec, ParseError> {
    let block_size = rd_u32(buf, *pos)? as usize;
    let block_end = *pos + block_size + 4;
    *pos += 4;
    *pos += 4; // unknown
    let x = rd_u32(buf, *pos)?;
    *pos += 4;
    let y = rd_u32(buf, *pos)?;
    *pos += 4;
    *pos += 8; // unknown
    let name_len = rd_u32(buf, *pos)? as usize;
    *pos += 4;
    let name_bytes = buf
        .get(*pos..*pos + name_len)
        .ok_or_else(|| ParseError::Malformed("XZZ pin name out of range".into()))?;
    let number = String::from_utf8_lossy(name_bytes).into_owned();
    *pos += name_len;
    *pos += 32;
    let net_index = rd_u32(buf, *pos)?;
    *pos = block_end.min(buf.len());
    Ok(PinRec {
        pos: Point::new((x / SCALE) as f32, (y / SCALE) as f32),
        number,
        net_index,
    })
}

pub fn parse(bytes: &[u8], ctx: &ParseContext) -> Result<BoardModel, ParseError> {
    if !verify(bytes) {
        return Err(ParseError::Unrecognized);
    }
    let key = match ctx.xzz_key {
        Some(k) if check_key(k) => Some(k),
        Some(_) => return Err(ParseError::KeyMissing("XZZ")),
        None => None,
    };

    let mut buf = bytes.to_vec();
    // De-XOR everything before the "v6v6555v6v6" marker (diode readings
    // section, stored unobfuscated).
    let marker = b"v6v6555v6v6";
    let marker_pos = buf
        .windows(marker.len())
        .position(|w| w == marker)
        .unwrap_or(buf.len());
    if buf.len() > 0x10 && buf[0x10] != 0 {
        let k = buf[0x10];
        for b in buf[..marker_pos].iter_mut() {
            *b ^= k;
        }
    }

    let main_data_start = rd_u32(&buf, 0x20)? as usize + 0x20;
    let net_data_start = rd_u32(&buf, 0x28)? as usize + 0x20;
    let main_size = rd_u32(&buf, main_data_start)? as usize;
    let net_size = rd_u32(&buf, net_data_start)? as usize;

    // Net table: [size u32][index u32][name (size-8)]...
    let mut net_dict: HashMap<u32, String> = HashMap::new();
    {
        let start = net_data_start + 4;
        let end = start
            .checked_add(net_size)
            .filter(|&e| e <= buf.len())
            .ok_or_else(|| ParseError::Malformed("XZZ net block out of range".into()))?;
        let block = &buf[start..end];
        let mut p = 0usize;
        while p + 8 <= block.len() {
            let entry_size = rd_u32(block, p)? as usize;
            let index = rd_u32(block, p + 4)?;
            p += 8;
            if entry_size < 8 || p + entry_size - 8 > block.len() {
                return Err(ParseError::Malformed("XZZ net entry out of range".into()));
            }
            let name = String::from_utf8_lossy(&block[p..p + entry_size - 8]).into_owned();
            p += entry_size - 8;
            net_dict.insert(index, name);
        }
    }

    let mut b = BoardBuilder::new();
    let mut min = Point::new(f32::MAX, f32::MAX);
    let mut have_outline = false;
    let mut outline: Vec<(Point, Point)> = Vec::new();
    let mut parts_pins: Vec<(String, Vec<PinRec>)> = Vec::new();
    let mut test_pads: Vec<PinRec> = Vec::new();

    // Main blocks: [type u8][size u32][data]...
    {
        let mut p = main_data_start + 4;
        let end = p
            .checked_add(main_size)
            .filter(|&e| e <= buf.len())
            .ok_or_else(|| ParseError::Malformed("XZZ main block out of range".into()))?;
        while p < end {
            let block_type = buf[p];
            p += 1;
            let size = rd_u32(&buf, p)? as usize;
            p += 4;
            if p + size > buf.len() {
                return Err(ParseError::Malformed("XZZ block overruns file".into()));
            }
            let block = &buf[p..p + size];
            p += size;

            match block_type {
                0x01 => {
                    // Arc: layer,x,y,r,angle_start,angle_end,scale
                    let layer = rd_u32(block, 0)?;
                    if layer == 28 {
                        let x = rd_u32(block, 4)? / SCALE;
                        let y = rd_u32(block, 8)? / SCALE;
                        let r = rd_u32(block, 12)? / SCALE;
                        let a0 = (rd_u32(block, 16)? / SCALE) as i32;
                        let a1 = (rd_u32(block, 20)? / SCALE) as i32;
                        outline.extend(arc_segments(
                            a0,
                            a1,
                            r as f64,
                            Point::new(x as f32, y as f32),
                        ));
                        have_outline = true;
                    }
                }
                0x05 => {
                    // Line segment: layer,x1,y1,x2,y2
                    let layer = rd_u32(block, 0)?;
                    if layer == 28 {
                        let x1 = rd_u32(block, 4)? / SCALE;
                        let y1 = rd_u32(block, 8)? / SCALE;
                        let x2 = rd_u32(block, 12)? / SCALE;
                        let y2 = rd_u32(block, 16)? / SCALE;
                        outline.push((
                            Point::new(x1 as f32, y1 as f32),
                            Point::new(x2 as f32, y2 as f32),
                        ));
                        have_outline = true;
                    }
                }
                0x07 => {
                    // Part block, DES-encrypted.
                    let Some(key) = key else {
                        return Err(ParseError::KeyMissing("XZZ"));
                    };
                    let dec = des_decrypt(block, key);
                    let part_size = rd_u32(&dec, 0)? as usize;
                    let mut q = 4usize + 18;
                    let group_len = rd_u32(&dec, q)? as usize;
                    q += 4 + group_len;
                    if dec.get(q) != Some(&0x06) {
                        return Err(ParseError::Malformed(
                            "XZZ part block: expected 0x06 sub-block (wrong key?)".into(),
                        ));
                    }
                    q += 31;
                    let name_len = rd_u32(&dec, q)? as usize;
                    q += 4;
                    let name_bytes = dec.get(q..q + name_len).ok_or_else(|| {
                        ParseError::Malformed("XZZ part name out of range".into())
                    })?;
                    let refdes = String::from_utf8_lossy(name_bytes).into_owned();
                    q += name_len;

                    let mut pins = Vec::new();
                    let sub_end = (part_size + 4).min(dec.len());
                    while q < sub_end {
                        let sub_type = dec[q];
                        q += 1;
                        match sub_type {
                            0x01 | 0x05 | 0x06 => {
                                let sz = rd_u32(&dec, q)? as usize;
                                q += sz + 4;
                            }
                            0x09 => {
                                pins.push(parse_pin_block(&dec, &mut q)?);
                            }
                            _ => {}
                        }
                    }
                    parts_pins.push((refdes, pins));
                }
                0x09 => {
                    // Test pad: number,x,y,(8),name_len,name ... net at tail.
                    let x = rd_u32(block, 4)?;
                    let y = rd_u32(block, 8)?;
                    let name_len = rd_u32(block, 20)? as usize;
                    let name_bytes = block.get(24..24 + name_len).ok_or_else(|| {
                        ParseError::Malformed("XZZ test pad name out of range".into())
                    })?;
                    let number = String::from_utf8_lossy(name_bytes).into_owned();
                    if block.len() < 4 {
                        return Err(ParseError::Malformed("XZZ test pad too small".into()));
                    }
                    let net_index = rd_u32(block, block.len() - 4)?;
                    test_pads.push(PinRec {
                        pos: Point::new((x / SCALE) as f32, (y / SCALE) as f32),
                        number,
                        net_index,
                    });
                }
                _ => {}
            }
        }
    }

    // Translate everything so the outline minimum sits at the origin.
    for (a, bp) in &outline {
        min.x = min.x.min(a.x).min(bp.x);
        min.y = min.y.min(a.y).min(bp.y);
    }
    if !have_outline {
        min = Point::new(0.0, 0.0);
    }
    for (a, bp) in outline {
        b.add_outline_segment(
            Point::new(a.x - min.x, a.y - min.y),
            Point::new(bp.x - min.x, bp.y - min.y),
        );
    }

    let lookup_net = |b: &mut BoardBuilder, dict: &HashMap<u32, String>, idx: u32| {
        match dict.get(&idx) {
            Some(name) if name == "NC" => fbv_core::NO_NET,
            Some(name) => b.net_id(name),
            None => fbv_core::NO_NET,
        }
    };

    for (refdes, pins) in parts_pins {
        let part = b.add_part(Part {
            refdes,
            mfg_code: None,
            value: None,
            package: None,
            side: Side::Top, // XZZ ships boards pre-split; sides live side by side
            pins: vec![],
            is_dummy: false,
        });
        for pin in pins {
            let net = lookup_net(&mut b, &net_dict, pin.net_index);
            b.add_pin(Pin {
                part,
                number: pin.number.clone(),
                name: pin.number,
                pos: Point::new(pin.pos.x - min.x, pin.pos.y - min.y),
                radius: 0.5,
                side: Side::Top,
                net,
                is_test_pad: false,
            });
        }
    }

    let nails: Vec<crate::text::RawNail> = test_pads
        .into_iter()
        .map(|pad| crate::text::RawNail {
            pos: Point::new(pad.pos.x - min.x, pad.pos.y - min.y),
            side: Side::Top,
            net: net_dict.get(&pad.net_index).cloned().unwrap_or_default(),
            probe: 0,
        })
        .collect();
    add_nails_as_pins(&mut b, &nails);

    b.generate_outline_from_pins();
    Ok(b.finish(BoardFormat::XzzPcb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use des::cipher::BlockEncrypt;

    /// DES key whose bytes satisfy the parity table: the table indexes
    /// bytes from the LSB, so bytes 0..=6 (low) need even popcount and
    /// byte 7 (MSB) needs odd popcount.
    const TEST_KEY: u64 = 0x0100_0000_0000_0000;

    fn des_encrypt(data: &[u8], key: u64) -> Vec<u8> {
        let des = Des::new_from_slice(&key.to_be_bytes()).unwrap();
        let mut out = Vec::new();
        for chunk in data.chunks(8) {
            let mut block = [0u8; 8];
            block[..chunk.len()].copy_from_slice(chunk);
            let mut ga = *des::cipher::generic_array::GenericArray::from_slice(&block);
            des.encrypt_block(&mut ga);
            out.extend_from_slice(&ga);
        }
        out
    }

    fn u32le(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn build_part_block(refdes: &str, pins: &[(&str, u32, u32, u32)]) -> Vec<u8> {
        // Decrypted layout: [part_size u32][18 unknown][group_len u32][group]
        // [0x06][30 more header][name_len u32][name][pin blocks...]
        let mut body = Vec::new();
        body.extend_from_slice(&[0u8; 18]);
        body.extend_from_slice(&u32le(0)); // group name len
        body.push(0x06);
        body.extend_from_slice(&[0u8; 30]);
        body.extend_from_slice(&u32le(refdes.len() as u32));
        body.extend_from_slice(refdes.as_bytes());
        for (num, x, y, net) in pins {
            let name = num.as_bytes();
            // pin block content after the leading size u32:
            let mut pb = Vec::new();
            pb.extend_from_slice(&[0u8; 4]); // unknown
            pb.extend_from_slice(&u32le(*x));
            pb.extend_from_slice(&u32le(*y));
            pb.extend_from_slice(&[0u8; 8]); // unknown
            pb.extend_from_slice(&u32le(name.len() as u32));
            pb.extend_from_slice(name);
            pb.extend_from_slice(&[0u8; 32]);
            pb.extend_from_slice(&u32le(*net));
            body.push(0x09);
            body.extend_from_slice(&u32le(pb.len() as u32));
            body.extend_from_slice(&pb);
        }
        let mut plain = Vec::new();
        plain.extend_from_slice(&u32le(body.len() as u32));
        plain.extend_from_slice(&body);
        des_encrypt(&plain, TEST_KEY)
    }

    fn build_sample(xor_key: u8) -> Vec<u8> {
        // Net table
        let mut net_block = Vec::new();
        for (idx, name) in [(1u32, "PPBUS_G3H"), (2, "GND"), (3, "NC")] {
            net_block.extend_from_slice(&u32le(8 + name.len() as u32));
            net_block.extend_from_slice(&u32le(idx));
            net_block.extend_from_slice(name.as_bytes());
        }

        // Main blocks
        let mut main = Vec::new();
        // outline line segment on layer 28
        let mut seg = Vec::new();
        seg.extend_from_slice(&u32le(28));
        for v in [0u32, 0, 10_000_000, 8_000_000] {
            seg.extend_from_slice(&u32le(v));
        }
        seg.extend_from_slice(&u32le(1)); // scale field (ignored)
        seg.extend_from_slice(&u32le(0));
        main.push(0x05);
        main.extend_from_slice(&u32le(seg.len() as u32));
        main.extend_from_slice(&seg);
        // one part with two pins
        let part = build_part_block(
            "U5300",
            &[("A1", 1_000_000, 2_000_000, 1), ("A2", 1_100_000, 2_000_000, 2)],
        );
        main.push(0x07);
        main.extend_from_slice(&u32le(part.len() as u32));
        main.extend_from_slice(&part);
        // one test pad on net 1
        let mut pad = Vec::new();
        pad.extend_from_slice(&u32le(7)); // pad number
        pad.extend_from_slice(&u32le(4_000_000));
        pad.extend_from_slice(&u32le(500_000));
        pad.extend_from_slice(&[0u8; 8]);
        pad.extend_from_slice(&u32le(3));
        pad.extend_from_slice(b"TP1");
        pad.extend_from_slice(&u32le(1)); // net index at tail
        main.push(0x09);
        main.extend_from_slice(&u32le(pad.len() as u32));
        main.extend_from_slice(&pad);

        // Assemble file
        let mut v = Vec::new();
        v.extend_from_slice(b"XZZPCB");
        v.resize(0x30, 0);
        let main_start = 0x40usize;
        let net_start = main_start + 4 + main.len();
        v[0x10] = 0; // patched below when xor_key != 0
        v[0x20..0x24].copy_from_slice(&u32le((main_start - 0x20) as u32));
        v[0x28..0x2c].copy_from_slice(&u32le((net_start - 0x20) as u32));
        v.resize(main_start, 0);
        v.extend_from_slice(&u32le(main.len() as u32));
        v.extend_from_slice(&main);
        v.extend_from_slice(&u32le(net_block.len() as u32));
        v.extend_from_slice(&net_block);

        if xor_key != 0 {
            for b in v.iter_mut() {
                *b ^= xor_key;
            }
            v[0x10] = xor_key; // the XOR key byte itself (0 ^ k)
        }
        v
    }

    #[test]
    fn parses_plain_and_xored() {
        for xor_key in [0u8, 0x5a] {
            let data = build_sample(xor_key);
            assert!(verify(&data), "verify failed for xor {xor_key:#x}");
            let ctx = ParseContext {
                xzz_key: Some(TEST_KEY),
                ..Default::default()
            };
            let m = parse(&data, &ctx).unwrap();
            let real: Vec<_> = m.parts.iter().filter(|p| !p.is_dummy).collect();
            assert_eq!(real.len(), 1);
            assert_eq!(real[0].refdes, "U5300");
            assert_eq!(real[0].pins.len(), 2);
            assert_eq!(m.pins[0].pos.x, 100.0); // 1_000_000 / 10_000
            assert_eq!(m.pins[0].number, "A1");
            let ppbus = m.nets.iter().find(|n| n.name == "PPBUS_G3H").unwrap();
            assert_eq!(ppbus.pins.len(), 2); // pin A1 + test pad
            assert!(m.pins.iter().any(|p| p.is_test_pad));
        }
    }

    #[test]
    fn missing_key_reports_key_missing() {
        let data = build_sample(0);
        let ctx = ParseContext::default();
        assert!(matches!(
            parse(&data, &ctx),
            Err(ParseError::KeyMissing("XZZ"))
        ));
    }

    #[test]
    fn key_parity() {
        assert!(check_key(TEST_KEY));
        assert!(!check_key(0xdead_beef_dead_beef));
    }
}
