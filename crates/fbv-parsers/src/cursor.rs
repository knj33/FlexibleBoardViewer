//! A line cursor replicating the C `strtol`/`strtod`/token-scan semantics the
//! original OpenBoardView parsers rely on: skip leading whitespace, parse as
//! much as possible, return 0 (without advancing) when nothing parses.

pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    pub fn advance(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.buf.len());
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos.min(self.buf.len())..]
    }

    pub fn starts_with(&self, s: &str) -> bool {
        self.rest().starts_with(s.as_bytes())
    }

    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_space(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// `strtol(p, &p, 10)`: on no digits, returns 0 and does not move.
    pub fn read_int(&mut self) -> i64 {
        let start = self.pos;
        self.skip_ws();
        let mut p = self.pos;
        let mut sign = 1i64;
        if let Some(b) = self.buf.get(p) {
            if *b == b'-' {
                sign = -1;
                p += 1;
            } else if *b == b'+' {
                p += 1;
            }
        }
        let digits_start = p;
        let mut value: i64 = 0;
        while let Some(b) = self.buf.get(p) {
            if b.is_ascii_digit() {
                value = value.saturating_mul(10).saturating_add((b - b'0') as i64);
                p += 1;
            } else {
                break;
            }
        }
        if p == digits_start {
            self.pos = start;
            return 0;
        }
        self.pos = p;
        sign * value
    }

    /// `strtod(p, &p)`: parses the longest valid float prefix; 0.0 and no
    /// movement when nothing parses.
    pub fn read_double(&mut self) -> f64 {
        let start = self.pos;
        self.skip_ws();
        let begin = self.pos;
        let mut p = self.pos;
        if matches!(self.buf.get(p), Some(b'-') | Some(b'+')) {
            p += 1;
        }
        let mut saw_digit = false;
        while self.buf.get(p).map(|b| b.is_ascii_digit()).unwrap_or(false) {
            saw_digit = true;
            p += 1;
        }
        if self.buf.get(p) == Some(&b'.') {
            p += 1;
            while self.buf.get(p).map(|b| b.is_ascii_digit()).unwrap_or(false) {
                saw_digit = true;
                p += 1;
            }
        }
        if !saw_digit {
            self.pos = start;
            return 0.0;
        }
        // Optional exponent, only when followed by digits.
        if matches!(self.buf.get(p), Some(b'e') | Some(b'E')) {
            let mut q = p + 1;
            if matches!(self.buf.get(q), Some(b'-') | Some(b'+')) {
                q += 1;
            }
            if self.buf.get(q).map(|b| b.is_ascii_digit()).unwrap_or(false) {
                while self.buf.get(q).map(|b| b.is_ascii_digit()).unwrap_or(false) {
                    q += 1;
                }
                p = q;
            }
        }
        let text = std::str::from_utf8(&self.buf[begin..p]).unwrap_or("0");
        self.pos = p;
        text.parse::<f64>().unwrap_or(0.0)
    }

    /// `READ_STR`: skip whitespace, take the token up to the next whitespace,
    /// consume one delimiter char.
    pub fn read_token(&mut self) -> String {
        self.skip_ws();
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_space(b) {
                break;
            }
            self.pos += 1;
        }
        let tok = lossy(&self.buf[start..self.pos]);
        self.advance(1);
        tok
    }

    /// FZ `READ_STR`: skip whitespace, take everything up to `delim`,
    /// consume the delimiter.
    pub fn read_until(&mut self, delim: u8) -> String {
        self.skip_ws();
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == delim {
                break;
            }
            self.pos += 1;
        }
        let tok = lossy(&self.buf[start..self.pos]);
        self.advance(1);
        tok
    }

    /// Tab-delimited field (FZ description block): leading spaces (but not
    /// tabs) skipped, then everything up to the next tab.
    pub fn read_tab_field(&mut self) -> String {
        while let Some(b) = self.peek() {
            if is_space(b) && b != b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'\t' {
                break;
            }
            self.pos += 1;
        }
        let tok = lossy(&self.buf[start..self.pos]);
        self.advance(1);
        tok
    }

    /// FZ numeric reads consume a trailing '!' delimiter when present.
    pub fn eat_delim(&mut self, delim: u8) {
        if self.peek() == Some(delim) {
            self.pos += 1;
        }
    }

    /// BVR `nextfield`: skip to just past the next tab on the line.
    pub fn next_tab_field(&mut self) {
        while let Some(b) = self.peek() {
            if b == b'\t' || b == b'\r' || b == b'\n' {
                break;
            }
            self.pos += 1;
        }
        if self.peek() == Some(b'\t') {
            self.pos += 1;
        }
    }
}

pub fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_and_doubles() {
        let mut c = Cursor::new(b"  42 -7 3.25 1e3 nope");
        assert_eq!(c.read_int(), 42);
        assert_eq!(c.read_int(), -7);
        assert_eq!(c.read_double(), 3.25);
        assert_eq!(c.read_double(), 1000.0);
        // No digits: value 0, cursor unmoved, token still readable.
        assert_eq!(c.read_int(), 0);
        assert_eq!(c.read_token(), "nope");
    }

    #[test]
    fn tokens() {
        let mut c = Cursor::new(b"U5300  6 130");
        assert_eq!(c.read_token(), "U5300");
        assert_eq!(c.read_int(), 6);
        assert_eq!(c.read_int(), 130);
    }

    #[test]
    fn bang_fields() {
        let mut c = Cursor::new(b"PPBUS_G3H!U7000!A1!name with spaces!123.5!");
        assert_eq!(c.read_until(b'!'), "PPBUS_G3H");
        assert_eq!(c.read_until(b'!'), "U7000");
        assert_eq!(c.read_until(b'!'), "A1");
        assert_eq!(c.read_until(b'!'), "name with spaces");
        assert_eq!(c.read_double(), 123.5);
        c.eat_delim(b'!');
        assert!(c.at_end());
    }

    #[test]
    fn tab_fields() {
        let mut c = Cursor::new(b"TPS51225\t 3V buck \t4\tU7000 U7100\tTPS51225C");
        assert_eq!(c.read_tab_field(), "TPS51225");
        assert_eq!(c.read_tab_field(), "3V buck ");
        assert_eq!(c.read_int(), 4);
        assert_eq!(c.read_tab_field(), "");
        assert_eq!(c.read_tab_field(), "U7000 U7100");
    }
}
