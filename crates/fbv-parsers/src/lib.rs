//! Boardview file parsers.
//!
//! Format knowledge is ported from OpenBoardView (MIT license,
//! https://github.com/OpenBoardView/OpenBoardView) — see the NOTICE file at
//! the repository root. Every parser is a pure function over a byte buffer
//! (plus an optional companion-file loader for multi-file formats like ASC),
//! so the indexer can run them on worker threads and a failing file can
//! never take down the application.

pub mod cursor;
mod text;

mod asc;
mod bdv;
mod brd;
mod brd2;
mod bvr;
mod cad;
mod cst;
mod fz;
mod gencad;
mod xzz;

/// Bumped whenever detection or a parser materially improves, so files that
/// previously landed in quarantine are retried instead of skipped forever.
/// Generation log: 1 = initial release; 2 = GenCAD support + relaxed
/// Samsung CAD detection.
pub const PARSER_GENERATION: i64 = 2;

use fbv_core::{BoardFormat, BoardModel};
use std::path::Path;

#[derive(Debug)]
pub enum ParseError {
    Unrecognized,
    Malformed(String),
    /// Encrypted format whose key is not configured (FZ / XZZ).
    KeyMissing(&'static str),
    Io(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Unrecognized => write!(f, "unrecognized boardview format"),
            ParseError::Malformed(m) => write!(f, "malformed file: {m}"),
            ParseError::KeyMissing(fmt) => write!(
                f,
                "{fmt} file is encrypted and no valid {fmt} key is configured in Settings"
            ),
            ParseError::Io(m) => write!(f, "I/O error: {m}"),
        }
    }
}
impl std::error::Error for ParseError {}

/// Context handed to parsers. Keys are user-supplied (Settings), matching
/// OpenBoardView's approach of not shipping decryption keys in the source.
pub struct ParseContext<'a> {
    /// RC6 key schedule for ASUS .fz files (44 words), as in OBV's `FZKey`.
    pub fz_key: Option<[u32; 44]>,
    /// DES key for XZZ .pcb files, as in OBV's XZZ key setting.
    pub xzz_key: Option<u64>,
    /// Loads a sibling file by (case-insensitive) name for multi-file
    /// formats. `None` outside a filesystem context.
    #[allow(clippy::type_complexity)]
    pub companion: Option<&'a dyn Fn(&str) -> Option<Vec<u8>>>,
}

impl Default for ParseContext<'_> {
    fn default() -> Self {
        Self {
            fz_key: None,
            xzz_key: None,
            companion: None,
        }
    }
}

/// All file extensions worth inspecting during library import.
pub const KNOWN_EXTENSIONS: &[&str] = &[
    "brd", "bdv", "bv", "bvr", "bvr2", "bvr3", "asc", "cad", "gcd", "cst", "fz", "pcb",
];

/// Content-based format detection; falls back to the extension only for
/// formats without a reliable signature (CST, FZ). Signature checks run
/// first because extensions are heavily overloaded in the wild (.brd is
/// used by three unrelated formats, .cad by two).
pub fn detect(bytes: &[u8], path: Option<&Path>) -> Option<BoardFormat> {
    if xzz::verify(bytes) {
        return Some(BoardFormat::XzzPcb);
    }
    if brd::verify(bytes) {
        return Some(BoardFormat::Brd);
    }
    if brd2::verify(bytes) {
        return Some(BoardFormat::Brd2);
    }
    if bvr::verify_v1(bytes) {
        return Some(BoardFormat::Bvr);
    }
    if bvr::verify_v3(bytes) {
        return Some(BoardFormat::Bvr3);
    }
    if bdv::verify(bytes) {
        return Some(BoardFormat::Bdv);
    }
    // GenCAD before Samsung CAD: both commonly ship as ".cad", but their
    // content signatures are mutually exclusive.
    if gencad::verify(bytes) {
        return Some(BoardFormat::GenCad);
    }
    if cad::verify(bytes) {
        return Some(BoardFormat::Cad);
    }
    let ext = path
        .and_then(|p| p.extension())
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("fz") => Some(BoardFormat::Fz),
        Some("cst") => Some(BoardFormat::Cst),
        Some("asc") => Some(BoardFormat::Asc),
        _ => None,
    }
}

pub fn parse(
    format: BoardFormat,
    bytes: &[u8],
    ctx: &ParseContext,
) -> Result<BoardModel, ParseError> {
    match format {
        BoardFormat::Brd => brd::parse(bytes),
        BoardFormat::Brd2 => brd2::parse(bytes),
        BoardFormat::Bdv => bdv::parse(bytes),
        BoardFormat::Bvr => bvr::parse_v1(bytes),
        BoardFormat::Bvr3 => bvr::parse_v3(bytes),
        BoardFormat::Asc => asc::parse(bytes, ctx),
        BoardFormat::Cad => cad::parse(bytes),
        BoardFormat::GenCad => gencad::parse(bytes),
        BoardFormat::Cst => cst::parse(bytes),
        BoardFormat::Fz => fz::parse(bytes, ctx),
        BoardFormat::XzzPcb => xzz::parse(bytes, ctx),
    }
}

/// Detect and parse in one step; the standard entry point for the indexer.
pub fn detect_and_parse(
    bytes: &[u8],
    path: Option<&Path>,
    ctx: &ParseContext,
) -> Result<(BoardFormat, BoardModel), ParseError> {
    let format = detect(bytes, path).ok_or(ParseError::Unrecognized)?;
    let model = parse(format, bytes, ctx)?;
    Ok((format, model))
}

/// `find_str_in_buf` equivalent: substring search over raw bytes.
pub(crate) fn find_in_buf(needle: &str, buf: &[u8]) -> bool {
    let n = needle.as_bytes();
    if n.is_empty() || buf.len() < n.len() {
        return false;
    }
    buf.windows(n.len()).any(|w| w == n)
}
