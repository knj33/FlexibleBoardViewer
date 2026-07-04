//! Board-identity heuristics: boardview files rarely contain the board's
//! model name, but repair-world filenames usually do (OEM board codes).
//! Everything here is a guess and stays user-overridable in the library.

/// Extracted identity hints for one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityGuess {
    /// OEM board code, e.g. "820-00281", "LA-E672P", "NM-B301".
    pub oem_code: Option<String>,
    /// Cleaned display name derived from the file stem.
    pub display_name: String,
}

/// Derives identity hints from a file path.
pub fn guess_identity(path: &std::path::Path) -> IdentityGuess {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut guess = IdentityGuess {
        oem_code: find_oem_code(&stem),
        display_name: clean_name(&stem),
    };
    // Fall back to the parent folder name, a common convention for
    // one-folder-per-board libraries.
    if guess.oem_code.is_none() {
        if let Some(parent) = path.parent().and_then(|p| p.file_name()) {
            guess.oem_code = find_oem_code(&parent.to_string_lossy());
        }
    }
    guess
}

fn clean_name(stem: &str) -> String {
    let cleaned: String = stem
        .chars()
        .map(|c| if c == '_' { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "unnamed board".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Scans a string for known OEM board-code patterns.
pub fn find_oem_code(s: &str) -> Option<String> {
    let upper = s.to_ascii_uppercase();
    let bytes = upper.as_bytes();

    // Apple logic boards: 820-XXXXX or 820-XXXX (older), optional -A/-B rev.
    if let Some(code) = scan(&upper, "820-", |rest| {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        (digits.len() >= 4).then(|| format!("820-{digits}"))
    }) {
        return Some(code);
    }
    // Compal: LA-XXXXX with optional trailing letter (LA-E672P).
    if let Some(code) = scan(&upper, "LA-", |rest| {
        let body: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        (body.len() >= 4).then(|| format!("LA-{body}"))
    }) {
        return Some(code);
    }
    // Lenovo/Wistron: NM-XXXXX.
    if let Some(code) = scan(&upper, "NM-", |rest| {
        let body: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        (body.len() >= 4).then(|| format!("NM-{body}"))
    }) {
        return Some(code);
    }
    // Quanta: DA0XXXXXXXX / DAXXXXXXXX (board code starts "DA0" or "DA" +
    // project code). Require length to avoid matching random words.
    if let Some(pos) = upper.find("DA0") {
        let rest = &upper[pos..];
        let body: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if body.len() >= 8 {
            return Some(body);
        }
    }
    let _ = bytes;
    None
}

fn scan(hay: &str, prefix: &str, parse: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut search_from = 0;
    while let Some(rel) = hay[search_from..].find(prefix) {
        let pos = search_from + rel;
        if let Some(code) = parse(&hay[pos + prefix.len()..]) {
            return Some(code);
        }
        search_from = pos + prefix.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn apple_code_from_filename() {
        let g = guess_identity(Path::new("/lib/apple/820-00281-A_MLB.brd"));
        assert_eq!(g.oem_code.as_deref(), Some("820-00281"));
        assert_eq!(g.display_name, "820-00281-A MLB");
    }

    #[test]
    fn compal_code_from_parent_folder() {
        let g = guess_identity(Path::new("/lib/LA-E672P Rev 1.0/mainboard.bdv"));
        assert_eq!(g.oem_code.as_deref(), Some("LA-E672P"));
    }

    #[test]
    fn quanta_and_lenovo() {
        assert_eq!(
            find_oem_code("DA0Z8MMB8D0 schematics"),
            Some("DA0Z8MMB8D0".into())
        );
        assert_eq!(find_oem_code("nm-b301 rev1"), Some("NM-B301".into()));
        assert_eq!(find_oem_code("random file"), None);
    }
}
