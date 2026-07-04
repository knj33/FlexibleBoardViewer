//! Shared plumbing for the line-oriented formats.

use fbv_core::{BoardBuilder, Part, Pin, Point, Side};

/// Splits a buffer into lines exactly like OpenBoardView's `stringfile()`
/// (from stb.h): a `\r` or `\n` ends a line, and one immediately following
/// `\r` or `\n` is swallowed with it. Several parsers skip fixed numbers of
/// header lines, so this quirk-compatibility matters.
pub fn split_lines(buf: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < buf.len() {
        let b = buf[i];
        if b == b'\r' || b == b'\n' {
            lines.push(&buf[start..i]);
            i += 1;
            if i < buf.len() && (buf[i] == b'\r' || buf[i] == b'\n') {
                i += 1;
            }
            start = i;
        } else {
            i += 1;
        }
    }
    if start < buf.len() {
        lines.push(&buf[start..]);
    }
    lines
}

/// Trims leading ASCII whitespace, mirroring the `while isspace(*line) line++`
/// prologue of every OBV parser loop.
pub fn ltrim(line: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < line.len() && (line[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    &line[i..]
}

/// A test-probe point ("nail") collected during parsing.
pub struct RawNail {
    pub pos: Point,
    pub side: Side,
    pub net: String,
    pub probe: i64,
}

/// Attaches nails as probe-able test-pad pins grouped under synthetic "..."
/// parts (one per side), the convention OpenBoardView renderers expect.
pub fn add_nails_as_pins(b: &mut BoardBuilder, nails: &[RawNail]) {
    if nails.is_empty() {
        return;
    }
    let bottom_part = b.add_part(Part {
        refdes: "...".into(),
        mfg_code: None,
        value: None,
        package: None,
        side: Side::Bottom,
        pins: vec![],
        is_dummy: true,
    });
    let top_part = b.add_part(Part {
        refdes: "...".into(),
        mfg_code: None,
        value: None,
        package: None,
        side: Side::Top,
        pins: vec![],
        is_dummy: true,
    });
    for nail in nails {
        let net = b.net_id(&nail.net);
        let (part, side) = match nail.side {
            Side::Bottom => (bottom_part, Side::Bottom),
            Side::Top => (top_part, Side::Top),
            Side::Both => (top_part, Side::Both),
        };
        b.add_pin(Pin {
            part,
            number: if nail.probe != 0 {
                nail.probe.to_string()
            } else {
                String::new()
            },
            name: String::new(),
            pos: nail.pos,
            radius: 0.5,
            side,
            net,
            is_test_pad: true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_splitting_matches_stringfile() {
        // CRLF pairs collapse; lone breaks split; "\n\n" also collapses
        // (stringfile swallows any second break char).
        let buf = b"a\r\nb\nc\n\nd\r\re";
        let lines: Vec<&[u8]> = split_lines(buf);
        let strs: Vec<&str> = lines
            .iter()
            .map(|l| std::str::from_utf8(l).unwrap())
            .collect();
        assert_eq!(strs, vec!["a", "b", "c", "d", "e"]);
    }
}
