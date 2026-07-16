//! Source outline: the top-level item signatures of a Rust file, without bodies.
//!
//! An agent maps a file's shape by reading its signatures rather than its whole
//! text. Each top-level item contributes one line — the source as authored, from
//! the item's start up to its body's opening brace (or its whole text when it has
//! no brace body), collapsed to a single line. Imports are omitted as noise.

use syn::{Item, spanned::Spanned};

/// Why an outline could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum OutlineError {
    /// The source did not parse as a Rust file.
    #[error("parsing the source as Rust: {0}")]
    Parse(String),
}

/// The longest one signature line runs before it is elided, so a large const
/// value or macro body cannot bloat the outline.
const SIGNATURE_CAP: usize = 200;

/// The top-level item signatures of a Rust source, one per line, in source order.
pub fn outline(source: &str) -> Result<String, OutlineError> {
    let file = syn::parse_file(source).map_err(|error| OutlineError::Parse(error.to_string()))?;
    let lines: Vec<String> = file
        .items
        .iter()
        .filter_map(|item| signature(source, item))
        .collect();
    Ok(lines.join("\n"))
}

/// One item's signature line, or `None` for an import.
fn signature(source: &str, item: &Item) -> Option<String> {
    if matches!(item, Item::Use(_) | Item::ExternCrate(_)) {
        return None;
    }
    let span = item.span().byte_range();
    // A doc comment and attributes lead the span; the signature starts after them.
    let start = attrs_end(item)
        .unwrap_or(span.start)
        .clamp(span.start, span.end);
    let cut = body_open(item)
        .map(|open| open.byte_range().start)
        .unwrap_or(span.end)
        .clamp(start, span.end);
    let text = source.get(start..cut)?;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(cap(trimmed))
}

/// The byte after an item's leading attributes and doc comments, where its
/// signature begins. `None` when the item carries none.
fn attrs_end(item: &Item) -> Option<usize> {
    let attrs = match item {
        Item::Fn(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Const(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        _ => return None,
    };
    attrs.last().map(|attr| attr.span().byte_range().end)
}

/// The opening brace of an item's body, when it has a braced body — the point a
/// signature is cut before. An item terminated by `;` (a const, a type alias, a
/// tuple struct) has none, and its whole text is its signature.
fn body_open(item: &Item) -> Option<proc_macro2::Span> {
    Some(match item {
        Item::Fn(item) => item.block.brace_token.span.open(),
        Item::Enum(item) => item.brace_token.span.open(),
        Item::Struct(item) => match &item.fields {
            syn::Fields::Named(fields) => fields.brace_token.span.open(),
            _ => return None,
        },
        Item::Union(item) => item.fields.brace_token.span.open(),
        Item::Impl(item) => item.brace_token.span.open(),
        Item::Trait(item) => item.brace_token.span.open(),
        Item::Mod(item) => item.content.as_ref()?.0.span.open(),
        _ => return None,
    })
}

/// A signature capped at a readable length on a char boundary.
fn cap(signature: &str) -> String {
    match signature.char_indices().nth(SIGNATURE_CAP) {
        None => signature.to_string(),
        Some((byte, _)) => format!("{}…", &signature[..byte]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outline_is_the_top_level_signatures_without_bodies() {
        let source = r#"
            use std::collections::BTreeMap;

            /// A doc comment is not part of the signature.
            pub async fn run(input: &str, count: usize) -> anyhow::Result<String> {
                let noise = 1;
                Ok(input.to_string())
            }

            pub struct Config {
                pub name: String,
                pub retries: u32,
            }

            enum State { Idle, Busy }

            impl Config {
                fn new() -> Self { todo!() }
            }

            const CAP: usize = 8_000;
        "#;
        let outline = outline(source).unwrap();
        let lines: Vec<&str> = outline.lines().collect();

        // Imports are omitted; each other top-level item is one signature line.
        assert_eq!(lines.len(), 5, "{outline}");
        assert_eq!(
            lines[0],
            "pub async fn run(input: &str, count: usize) -> anyhow::Result<String>"
        );
        assert_eq!(lines[1], "pub struct Config");
        assert_eq!(lines[2], "enum State");
        assert_eq!(lines[3], "impl Config");
        assert_eq!(lines[4], "const CAP: usize = 8_000");
        // No body text leaked into the outline.
        assert!(!outline.contains("noise"), "{outline}");
        assert!(!outline.contains("retries"), "{outline}");
    }

    #[test]
    fn a_tuple_struct_and_a_type_alias_keep_their_whole_signature() {
        let source = "pub struct Id(u64);\ntype Pair = (u8, u8);\n";
        let outline = outline(source).unwrap();
        assert_eq!(outline, "pub struct Id(u64)\ntype Pair = (u8, u8)");
    }

    #[test]
    fn a_multi_line_signature_collapses_to_one_line() {
        let source = "fn wide(\n    a: i32,\n    b: i32,\n) -> i32 {\n    a + b\n}\n";
        let outline = outline(source).unwrap();
        assert_eq!(outline, "fn wide( a: i32, b: i32, ) -> i32");
    }

    #[test]
    fn unparseable_source_is_an_error() {
        assert!(matches!(outline("fn ("), Err(OutlineError::Parse(_))));
    }
}
