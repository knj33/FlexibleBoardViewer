//! Cross-board search: a tiny query grammar compiled to FTS5 MATCH plus a
//! ranked join back to the metadata tables, and the same grammar applied
//! in-memory to a single open board.
//!
//! Grammar: whitespace-separated terms, implicit AND. A term may carry a
//! field prefix (`ref:U5300`, `net:PPBUS*`, `pn:TPS51225`, `val:1uF`,
//! `pkg:0402`, `board:820-00281`, `model:820*`). Every term is a prefix
//! match unless it is quoted.

use anyhow::Result;
use fbv_core::BoardModel;
use fbv_data::Database;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Part,
    Net,
}

/// One row of the cross-board result list; enough to render the result and
/// to open-and-center without touching the cache yet.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub kind: HitKind,
    pub board_id: i64,
    pub board_name: String,
    pub oem_code: Option<String>,
    pub condition: String,
    pub entity_idx: i64,
    /// Refdes for parts, net name for nets.
    pub label: String,
    pub value: Option<String>,
    pub part_number: Option<String>,
    pub package: Option<String>,
    pub side: Option<String>,
    pub x: f64,
    pub y: f64,
    pub pin_count: i64,
    pub harvested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    column: Option<&'static str>,
    text: String,
    exact: bool,
}

fn field_column(field: &str) -> Option<&'static str> {
    match field.to_ascii_lowercase().as_str() {
        "ref" | "refdes" => Some("refdes"),
        "net" => Some("net"),
        "val" | "value" => Some("value"),
        "pn" | "part" | "partnumber" => Some("part_number"),
        "pkg" | "package" => Some("package"),
        "board" | "name" => Some("board_name"),
        "model" | "oem" => Some("model"),
        _ => None,
    }
}

fn tokenize(query: &str) -> Vec<Term> {
    let mut out = Vec::new();
    for raw in query.split_whitespace() {
        let (column, rest) = match raw.split_once(':') {
            Some((f, r)) if field_column(f).is_some() && !r.is_empty() => {
                (field_column(f), r)
            }
            _ => (None, raw),
        };
        let quoted = rest.len() >= 2 && rest.starts_with('"') && rest.ends_with('"');
        let text = rest.trim_matches('"').trim_end_matches('*').to_string();
        if text.is_empty() {
            continue;
        }
        out.push(Term {
            column,
            text,
            exact: quoted,
        });
    }
    out
}

/// Builds the FTS5 MATCH expression. Every term value is double-quoted (with
/// embedded quotes doubled) so user input can never break out of the string.
fn build_match(terms: &[Term]) -> String {
    let mut parts = Vec::new();
    for t in terms {
        let escaped = t.text.replace('"', "\"\"");
        let quoted = if t.exact {
            format!("\"{escaped}\"")
        } else {
            format!("\"{escaped}\"*")
        };
        match t.column {
            Some(col) => parts.push(format!("{col}: {quoted}")),
            None => parts.push(quoted),
        }
    }
    parts.join(" ")
}

/// Runs a library-wide search. `limit` bounds the result page.
pub fn search_library(db: &Database, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let match_expr = build_match(&terms);
    // First bare/least-specific term drives the exact-match boost.
    let boost = terms
        .first()
        .map(|t| t.text.to_lowercase())
        .unwrap_or_default();

    let conn = db.connection();
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT s.kind,
               CAST(s.board_id AS INTEGER),
               CAST(s.entity_idx AS INTEGER),
               b.display_name, b.oem_code, b.condition,
               COALESCE(p.refdes, n.name, ''),
               p.value, p.part_number, p.package, p.side,
               COALESCE(p.x, 0.0), COALESCE(p.y, 0.0),
               COALESCE(p.pin_count, n.pin_count, 0),
               COALESCE(p.harvested, 0)
        FROM search_fts s
        JOIN boards b ON b.board_id = CAST(s.board_id AS INTEGER)
        LEFT JOIN parts p ON s.kind = 'part'
             AND p.board_id = CAST(s.board_id AS INTEGER)
             AND p.part_idx = CAST(s.entity_idx AS INTEGER)
        LEFT JOIN nets n ON s.kind = 'net'
             AND n.board_id = CAST(s.board_id AS INTEGER)
             AND n.net_idx = CAST(s.entity_idx AS INTEGER)
        WHERE search_fts MATCH ?1
        ORDER BY
            CASE WHEN LOWER(COALESCE(p.refdes, n.name, '')) = ?2 THEN 0 ELSE 1 END,
            bm25(search_fts),
            b.favorite DESC,
            b.display_name COLLATE NOCASE
        LIMIT ?3
        "#,
    )?;

    let rows = stmt
        .query_map(
            rusqlite::params![match_expr, boost, limit as i64],
            |r| {
                let kind: String = r.get(0)?;
                Ok(SearchHit {
                    kind: if kind == "net" { HitKind::Net } else { HitKind::Part },
                    board_id: r.get(1)?,
                    entity_idx: r.get(2)?,
                    board_name: r.get(3)?,
                    oem_code: r.get(4)?,
                    condition: r.get(5)?,
                    label: r.get(6)?,
                    value: r.get(7)?,
                    part_number: r.get(8)?,
                    package: r.get(9)?,
                    side: r.get(10)?,
                    x: r.get(11)?,
                    y: r.get(12)?,
                    pin_count: r.get(13)?,
                    harvested: r.get::<_, i64>(14)? != 0,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// In-board hit: index into the open model's parts or nets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InBoardHit {
    Part(usize),
    Net(usize),
}

/// Applies the same grammar to the open board, in memory. A hit must satisfy
/// every term (AND), matching any applicable field.
pub fn search_board(model: &BoardModel, query: &str) -> Vec<InBoardHit> {
    let terms = tokenize(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();

    let matches = |field_text: &str, t: &Term| -> bool {
        let hay = field_text.to_lowercase();
        let needle = t.text.to_lowercase();
        if t.exact {
            hay == needle
        } else {
            hay.contains(&needle)
        }
    };

    'parts: for (i, part) in model.parts.iter().enumerate() {
        if part.is_dummy {
            continue;
        }
        for t in &terms {
            let ok = match t.column {
                Some("refdes") => matches(&part.refdes, t),
                Some("value") => part.value.as_deref().map(|v| matches(v, t)).unwrap_or(false),
                Some("part_number") => part
                    .mfg_code
                    .as_deref()
                    .map(|v| matches(v, t))
                    .unwrap_or(false),
                Some("package") => part
                    .package
                    .as_deref()
                    .map(|v| matches(v, t))
                    .unwrap_or(false),
                Some("net") => part.pins.iter().any(|&pi| {
                    let net = model.pins[pi as usize].net;
                    net != fbv_core::NO_NET && matches(&model.nets[net as usize].name, t)
                }),
                Some(_) => false,
                None => {
                    matches(&part.refdes, t)
                        || part.value.as_deref().map(|v| matches(v, t)).unwrap_or(false)
                        || part
                            .mfg_code
                            .as_deref()
                            .map(|v| matches(v, t))
                            .unwrap_or(false)
                        || part
                            .package
                            .as_deref()
                            .map(|v| matches(v, t))
                            .unwrap_or(false)
                }
            };
            if !ok {
                continue 'parts;
            }
        }
        hits.push(InBoardHit::Part(i));
    }

    'nets: for (i, net) in model.nets.iter().enumerate() {
        for t in &terms {
            let ok = match t.column {
                Some("net") | None => matches(&net.name, t),
                Some(_) => false,
            };
            if !ok {
                continue 'nets;
            }
        }
        hits.push(InBoardHit::Net(i));
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbv_core::{BoardBuilder, BoardFormat, Part, Pin, Point, Side};
    use fbv_data::NewBoard;
    use std::path::Path;

    fn model_with(parts: &[(&str, Option<&str>, &str)]) -> BoardModel {
        // (refdes, part_number, net)
        let mut b = BoardBuilder::new();
        for (refdes, pn, net_name) in parts {
            let part = b.add_part(Part {
                refdes: refdes.to_string(),
                mfg_code: pn.map(|s| s.to_string()),
                value: None,
                package: Some("BGA".into()),
                side: Side::Top,
                pins: vec![],
                is_dummy: false,
            });
            let net = b.net_id(net_name);
            b.add_pin(Pin {
                part,
                number: "1".into(),
                name: String::new(),
                pos: Point::new(50.0, 60.0),
                radius: 1.0,
                side: Side::Top,
                net,
                is_test_pad: false,
            });
        }
        b.finish(BoardFormat::Brd)
    }

    fn seed() -> Database {
        let mut db = Database::open_in_memory().unwrap();
        let m1 = model_with(&[
            ("U5300", Some("ISL9239"), "PPBUS_G3H"),
            ("U7000", Some("TPS51225"), "PP3V3_S5"),
        ]);
        let m2 = model_with(&[("U5300", None, "PPBUS_G3H"), ("Q6001", None, "PPBUS_G3H")]);
        db.insert_board(&NewBoard {
            sha256: "h1",
            display_name: "820-00281 MLB",
            oem_code: Some("820-00281"),
            format: "BRD (Test_Link)",
            model: &m1,
            path: Path::new("/lib/a.brd"),
            size: 1,
            mtime: 1,
        })
        .unwrap();
        db.insert_board(&NewBoard {
            sha256: "h2",
            display_name: "820-00840 MLB",
            oem_code: Some("820-00840"),
            format: "BRD (Test_Link)",
            model: &m2,
            path: Path::new("/lib/b.brd"),
            size: 1,
            mtime: 1,
        })
        .unwrap();
        db
    }

    #[test]
    fn refdes_search_across_boards() {
        let db = seed();
        let hits = search_library(&db, "U5300", 100).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.label == "U5300"));
        let boards: Vec<&str> = hits.iter().map(|h| h.board_name.as_str()).collect();
        assert!(boards.contains(&"820-00281 MLB") && boards.contains(&"820-00840 MLB"));
    }

    #[test]
    fn part_number_and_prefix() {
        let db = seed();
        let hits = search_library(&db, "TPS51", 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].label, "U7000");
        assert_eq!(hits[0].part_number.as_deref(), Some("TPS51225"));

        let hits = search_library(&db, "pn:ISL9239", 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].board_name, "820-00281 MLB");
    }

    #[test]
    fn net_search_lists_every_board_with_that_rail() {
        let db = seed();
        let hits = search_library(&db, "net:PPBUS_G3H", 100).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.kind == HitKind::Net));
        // Pin counts per board: 1 on board 1, 2 on board 2.
        let mut pin_counts: Vec<i64> = hits.iter().map(|h| h.pin_count).collect();
        pin_counts.sort();
        assert_eq!(pin_counts, vec![1, 2]);
    }

    #[test]
    fn field_filter_and_board_filter() {
        let db = seed();
        let hits = search_library(&db, "board:820-00840 U5300", 100).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].board_name, "820-00840 MLB");

        // Exact term does not prefix-expand.
        let hits = search_library(&db, "ref:\"U53\"", 100).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn malicious_input_cannot_break_the_match_string() {
        let db = seed();
        for q in [
            "\" OR 1=1 --",
            "a\"b",
            "NEAR(",
            "refdes:*",
            "((((",
            "net:\"",
        ] {
            // Must not error out, just return whatever legitimately matches.
            let _ = search_library(&db, q, 10).unwrap();
        }
    }

    #[test]
    fn in_board_search_matches_grammar() {
        let m = model_with(&[
            ("U5300", Some("ISL9239"), "PPBUS_G3H"),
            ("C1234", None, "PPBUS_G3H"),
        ]);
        let hits = search_board(&m, "U53");
        assert_eq!(hits, vec![InBoardHit::Part(0)]);
        let hits = search_board(&m, "PPBUS");
        // Both the net and nothing else (parts match nets only via net:).
        assert_eq!(hits, vec![InBoardHit::Net(0)]);
        let hits = search_board(&m, "net:PPBUS_G3H");
        assert_eq!(
            hits,
            vec![
                InBoardHit::Part(0),
                InBoardHit::Part(1),
                InBoardHit::Net(0)
            ]
        );
    }
}
