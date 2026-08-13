//! Extract flattened numeric literals from named C array declarations, and
//! cross-check them against the equivalent hand-transcribed Rust `const`.
//!
//! This exists because spec-mandated numeric tables (CABAC context-init
//! tables, VLC tables, scan orders, ...) are currently transcribed once by
//! hand from a reference C decoder and then never re-checked — which is
//! exactly how `TRANS_IDX_LPS[28]` stayed wrong for a long time (see
//! `tpt-kinetix-h264/src/entropy.rs`). Both sides are flattened to a plain
//! `Vec<i64>` in source order, sidestepping the need to understand either
//! language's exact type/shape (`[[u32; 4]; 64]` vs `[(i8, i8); 1024]` vs a
//! packed subrange of a larger C array) — a mismatched flat sequence is still
//! definitive evidence of a transcription error.

use std::path::Path;

use tree_sitter::Node;

use crate::ingestion::CAst;

// ── C-side extraction ───────────────────────────────────────────────────────

/// Find the top-level array/struct declaration named `symbol` in `ast` and
/// return every numeric literal in its initializer, flattened in source
/// order (nested braces are walked but not otherwise preserved).
pub fn extract_c_symbol_numbers(ast: &CAst, symbol: &str) -> anyhow::Result<Vec<i64>> {
    let source = ast.source().as_bytes();
    let mut found = None;

    visit_all(ast.root_node(), &mut |node| {
        if found.is_some() || node.kind() != "init_declarator" {
            return;
        }
        let Some(declarator) = node.child_by_field_name("declarator") else {
            return;
        };
        if declarator_identifier(declarator, source) != Some(symbol) {
            return;
        }
        if let Some(value) = node.child_by_field_name("value") {
            let mut numbers = Vec::new();
            flatten_numbers(value, source, &mut numbers);
            found = Some(numbers);
        }
    });

    found.ok_or_else(|| anyhow::anyhow!("symbol `{symbol}` not found (or has no initializer)"))
}

/// Call `visitor` on `node` and every descendant, depth-first.
fn visit_all<'a, F: FnMut(Node<'a>)>(node: Node<'a>, visitor: &mut F) {
    visitor(node);
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            visit_all(cursor.node(), visitor);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Unwrap `array_declarator`/`pointer_declarator` chains down to the base
/// identifier, e.g. `cabac_context_init_PB[3][1024][2]` -> `cabac_context_init_PB`.
fn declarator_identifier<'a>(node: Node<'a>, source: &'a [u8]) -> Option<&'a str> {
    match node.kind() {
        "identifier" => node.utf8_text(source).ok(),
        "array_declarator" | "pointer_declarator" => {
            declarator_identifier(node.child_by_field_name("declarator")?, source)
        }
        _ => None,
    }
}

/// Recursively collect every `number_literal` leaf (handling unary `-`) under
/// `node`, in source order.
fn flatten_numbers(node: Node<'_>, source: &[u8], out: &mut Vec<i64>) {
    match node.kind() {
        // tree-sitter-c tokenizes a negative literal like `-15` as a single
        // `number_literal` (no separate unary-minus node), so a leading `-`
        // only needs to be kept when it's the first character.
        "number_literal" => {
            if let Ok(text) = node.utf8_text(source) {
                let digits: String = text
                    .char_indices()
                    .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
                    .map(|(_, c)| c)
                    .collect();
                if let Ok(v) = digits.parse::<i64>() {
                    out.push(v);
                }
            }
        }
        // Cover the case where the parser *does* emit an explicit unary
        // minus (e.g. `-(2)`), even though the common `-N` literal case
        // above already handles the token form actually seen in practice.
        "unary_expression" => {
            let is_negative = node
                .child_by_field_name("operator")
                .and_then(|op| op.utf8_text(source).ok())
                == Some("-");
            if let Some(arg) = node.child_by_field_name("argument") {
                let start = out.len();
                flatten_numbers(arg, source, out);
                if is_negative {
                    for v in &mut out[start..] {
                        *v = -*v;
                    }
                }
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                flatten_numbers(child, source, out);
            }
        }
    }
}

// ── Rust-side extraction ────────────────────────────────────────────────────

/// Find `const <name>` (or `pub const <name>`) in `rust_source`, locate its
/// `= [ ... ];` initializer via bracket balancing, and flatten every integer
/// literal inside it in source order. Deliberately naive (no real Rust
/// parser) — sufficient because the initializer contains nothing but nested
/// array/tuple literals and integers.
pub fn extract_rust_const_numbers(rust_source: &str, const_name: &str) -> anyhow::Result<Vec<i64>> {
    let needle = format!("const {const_name}");
    let decl_start = rust_source
        .find(&needle)
        .ok_or_else(|| anyhow::anyhow!("`const {const_name}` not found in Rust source"))?;

    let eq_rel = rust_source[decl_start..]
        .find('=')
        .ok_or_else(|| anyhow::anyhow!("no `=` after `const {const_name}`"))?;
    let bracket_open_rel = rust_source[decl_start + eq_rel..]
        .find('[')
        .ok_or_else(|| anyhow::anyhow!("no `[` initializer after `const {const_name} =`"))?;
    let body_start = decl_start + eq_rel + bracket_open_rel;

    let mut depth = 0i32;
    let mut body_end = None;
    for (i, ch) in rust_source[body_start..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    body_end = Some(body_start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let body_end = body_end
        .ok_or_else(|| anyhow::anyhow!("unbalanced `[` in `const {const_name}` initializer"))?;

    Ok(extract_ints(&rust_source[body_start..body_end]))
}

/// Scan `text` for integer tokens (optionally sign-prefixed), ignoring digits
/// that are part of a larger identifier (e.g. the `8` in `i8`).
fn extract_ints(text: &str) -> Vec<i64> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let preceded_by_ident =
                i > 0 && (chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
            if preceded_by_ident {
                i += 1;
                continue;
            }
            let negative = i > 0
                && chars[i - 1] == '-'
                && !(i > 1 && (chars[i - 2].is_ascii_alphanumeric() || chars[i - 2] == '_'));

            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let digits: String = chars[start..i].iter().collect();
            if let Ok(mut v) = digits.parse::<i64>() {
                if negative {
                    v = -v;
                }
                out.push(v);
            }
        } else {
            i += 1;
        }
    }
    out
}

// ── marker-driven verification ──────────────────────────────────────────────

/// A `// verify-tables: ...` annotation found above a Rust `const`.
#[derive(Debug, Clone)]
pub struct TableMarker {
    pub rust_name: String,
    pub symbol: String,
    pub commit: String,
    pub file: String,
    /// Optional `start:end` half-open slice into the flattened C symbol, for
    /// Rust consts that only cover part of a larger combined C array (e.g.
    /// `cabac_context_init_PB[3][1024][2]` split into three Rust consts).
    pub range: Option<(usize, usize)>,
}

/// Parse every `// verify-tables: key=value ...` line in `source`.
pub fn parse_markers(source: &str) -> Vec<TableMarker> {
    let mut markers = Vec::new();
    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("// verify-tables:") else {
            continue;
        };
        let mut rust_name = None;
        let mut symbol = None;
        let mut commit = None;
        let mut file = None;
        let mut range = None;
        for kv in rest.split_whitespace() {
            if let Some((k, v)) = kv.split_once('=') {
                match k {
                    "rust" => rust_name = Some(v.to_string()),
                    "symbol" => symbol = Some(v.to_string()),
                    "commit" => commit = Some(v.to_string()),
                    "file" => file = Some(v.to_string()),
                    "range" => {
                        if let Some((a, b)) = v.split_once(':') {
                            if let (Ok(a), Ok(b)) = (a.parse(), b.parse()) {
                                range = Some((a, b));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if let (Some(rust_name), Some(symbol), Some(commit), Some(file)) =
            (rust_name, symbol, commit, file)
        {
            markers.push(TableMarker {
                rust_name,
                symbol,
                commit,
                file,
                range,
            });
        }
    }
    markers
}

/// Outcome of checking one marker.
pub struct VerifyOutcome {
    pub rust_name: String,
    /// `(index, rust_value, c_value)` for every entry that differs. Empty
    /// means the two sides match exactly.
    pub mismatches: Vec<(usize, i64, i64)>,
}

/// Verify every marker found in `rust_file`, fetching referenced C source
/// files (pinned by commit) into `cache_dir` as needed.
pub fn verify_file(rust_file: &Path, cache_dir: &Path) -> anyhow::Result<Vec<VerifyOutcome>> {
    let rust_source = std::fs::read_to_string(rust_file)?;
    let markers = parse_markers(&rust_source);
    let mut outcomes = Vec::new();

    for marker in markers {
        let c_path =
            crate::fetch_source::fetch_pinned_file(&marker.commit, &marker.file, cache_dir)?;
        let c_ast = CAst::from_file(&c_path)?;
        let mut c_numbers = extract_c_symbol_numbers(&c_ast, &marker.symbol)?;
        if let Some((start, end)) = marker.range {
            c_numbers = c_numbers
                .get(start..end)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "range {start}:{end} out of bounds for `{}` ({} numbers)",
                        marker.symbol,
                        c_numbers.len()
                    )
                })?
                .to_vec();
        }

        let rust_numbers = extract_rust_const_numbers(&rust_source, &marker.rust_name)?;

        let mut mismatches = Vec::new();
        if rust_numbers.len() != c_numbers.len() {
            anyhow::bail!(
                "`{}`: length mismatch — Rust has {} numbers, C symbol `{}` (after any range slice) has {}",
                marker.rust_name,
                rust_numbers.len(),
                marker.symbol,
                c_numbers.len()
            );
        }
        for (i, (r, c)) in rust_numbers.iter().zip(c_numbers.iter()).enumerate() {
            if r != c {
                mismatches.push((i, *r, *c));
            }
        }

        outcomes.push(VerifyOutcome {
            rust_name: marker.rust_name,
            mismatches,
        });
    }

    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_simple_c_array() {
        let ast = CAst::from_source("static const int8_t tab[3][2] = { {1,-2}, {3,4}, {-5,6} };")
            .unwrap();
        let nums = extract_c_symbol_numbers(&ast, "tab").unwrap();
        assert_eq!(nums, vec![1, -2, 3, 4, -5, 6]);
    }

    #[test]
    fn missing_symbol_errors() {
        let ast = CAst::from_source("static const int8_t tab[1] = { 1 };").unwrap();
        assert!(extract_c_symbol_numbers(&ast, "nope").is_err());
    }

    #[test]
    fn extracts_rust_const_ints_ignoring_type_annotation() {
        let src =
            "pub const TAB0: [(i8, i8); 3] = [(1, -2), (3, 4), (-5, 6)];\nconst OTHER: u8 = 9;";
        let nums = extract_rust_const_numbers(src, "TAB0").unwrap();
        assert_eq!(nums, vec![1, -2, 3, 4, -5, 6]);
    }

    #[test]
    fn extract_ints_skips_digits_inside_identifiers() {
        // The `8` in `i8` is preceded by an alphanumeric char and must be
        // skipped, so it must not leak into the flattened data values.
        assert_eq!(extract_ints("[i8; 1024] = [1, 2]"), vec![1024, 1, 2]);
    }

    #[test]
    fn parses_marker_comment() {
        let src = "// verify-tables: rust=TAB symbol=tab commit=abc123 file=libavcodec/x.c range=0:4\npub const TAB: [i8; 4] = [1,2,3,4];";
        let markers = parse_markers(src);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].rust_name, "TAB");
        assert_eq!(markers[0].symbol, "tab");
        assert_eq!(markers[0].commit, "abc123");
        assert_eq!(markers[0].file, "libavcodec/x.c");
        assert_eq!(markers[0].range, Some((0, 4)));
    }
}
