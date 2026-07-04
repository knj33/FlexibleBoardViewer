//! Persistent settings: JSON file in the platform data dir, next to the
//! library database and the parsed-board cache.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Folders scanned (recursively) on "Rescan library".
    pub library_roots: Vec<PathBuf>,
    /// FZ key: 44 hex words, whitespace/comma separated, `0x` prefixes
    /// allowed — the same value OpenBoardView users put in `FZKey`.
    pub fz_key_text: String,
    /// XZZ key: one 64-bit hex value.
    pub xzz_key_text: String,
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FlexibleBoardViewer")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(settings_path(), json);
        }
    }

    pub fn fz_key(&self) -> Option<[u32; 44]> {
        parse_fz_key(&self.fz_key_text)
    }

    pub fn xzz_key(&self) -> Option<u64> {
        parse_hex_u64(&self.xzz_key_text)
    }

    pub fn keys(&self) -> fbv_index::Keys {
        fbv_index::Keys {
            fz_key: self.fz_key(),
            xzz_key: self.xzz_key(),
        }
    }
}

pub fn parse_fz_key(text: &str) -> Option<[u32; 44]> {
    let words: Vec<u32> = text
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let s = s.trim_start_matches("0x").trim_start_matches("0X");
            u32::from_str_radix(s, 16)
        })
        .collect::<Result<_, _>>()
        .ok()?;
    if words.len() != 44 {
        return None;
    }
    let mut key = [0u32; 44];
    key.copy_from_slice(&words);
    Some(key)
}

pub fn parse_hex_u64(text: &str) -> Option<u64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let t = t.trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(t, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fz_key_parsing() {
        let text = (0..44)
            .map(|i| format!("0x{i:08x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let key = parse_fz_key(&text).unwrap();
        assert_eq!(key[43], 43);
        assert!(parse_fz_key("0x1 0x2").is_none());
        assert!(parse_fz_key("").is_none());
    }

    #[test]
    fn xzz_key_parsing() {
        assert_eq!(parse_hex_u64("0x0100000000000000"), Some(0x0100000000000000));
        assert_eq!(parse_hex_u64("  "), None);
        assert_eq!(parse_hex_u64("zz"), None);
    }
}
