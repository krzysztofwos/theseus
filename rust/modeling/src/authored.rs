//! Governed edits to named top-level items in authored Rust source.

use std::{fmt, ops::Range};

use syn::spanned::Spanned;
use thiserror::Error;

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const SOURCE_REVISION_BYTES: usize = 16;
const MAX_AUTHORED_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RUST_ITEM_BYTES: usize = 256 * 1024;

/// Whether a governed edit adds a new item or replaces an existing one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustItemMode {
    Insert,
    Replace,
}

/// One named top-level Rust item to insert or replace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustItemEdit {
    pub mode: RustItemMode,
    pub item: String,
}

/// The complete source and revision produced by an authored-item edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustItemEditOutcome {
    pub identity: String,
    pub source: String,
    pub new_revision: String,
}

/// Why an authored Rust item could not be edited.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum RustItemEditError {
    #[error("authored source revision must be 16 lowercase hexadecimal characters")]
    InvalidRevision,
    #[error("authored Rust source is {length} bytes; the maximum is {maximum}")]
    SourceTooLarge { length: usize, maximum: usize },
    #[error("edited Rust item is {length} bytes; the maximum is {maximum}")]
    ItemTooLarge { length: usize, maximum: usize },
    #[error("authored source revision is stale: expected {expected}, found {actual}")]
    StaleRevision { expected: String, actual: String },
    #[error("parsing authored Rust source: {message}")]
    InvalidSource { message: String },
    #[error("parsing edited Rust item: {message}")]
    InvalidItem { message: String },
    #[error("top-level Rust item kind `{kind}` is not supported")]
    UnsupportedItem { kind: &'static str },
    #[error("top-level `{kind}` item must have a name")]
    UnnamedItem { kind: &'static str },
    #[error("cannot insert `{identity}` because it already exists")]
    AlreadyExists { identity: String },
    #[error("cannot replace `{identity}` because it does not exist")]
    NotFound { identity: String },
    #[error("cannot edit `{identity}` because {count} matching items exist")]
    Ambiguous { identity: String, count: usize },
    #[error("the source span for `{identity}` is invalid")]
    InvalidSpan { identity: String },
    #[error("the edited Rust source is invalid: {message}")]
    InvalidEditedSource { message: String },
}

/// Return a stable, raw-byte revision for authored Rust source.
pub fn rust_source_revision(source: &str) -> String {
    let mut hash = FNV_OFFSET;
    for byte in source.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// Insert or replace one named top-level item without rewriting unrelated bytes.
pub fn edit_rust_item(
    source: &str,
    expected_revision: &str,
    edit: &RustItemEdit,
) -> Result<RustItemEditOutcome, RustItemEditError> {
    if expected_revision.len() != SOURCE_REVISION_BYTES
        || !expected_revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RustItemEditError::InvalidRevision);
    }
    if source.len() > MAX_AUTHORED_SOURCE_BYTES {
        return Err(RustItemEditError::SourceTooLarge {
            length: source.len(),
            maximum: MAX_AUTHORED_SOURCE_BYTES,
        });
    }
    if edit.item.len() > MAX_RUST_ITEM_BYTES {
        return Err(RustItemEditError::ItemTooLarge {
            length: edit.item.len(),
            maximum: MAX_RUST_ITEM_BYTES,
        });
    }
    let actual_revision = rust_source_revision(source);
    if expected_revision != actual_revision {
        return Err(RustItemEditError::StaleRevision {
            expected: expected_revision.to_string(),
            actual: actual_revision,
        });
    }

    let file = syn::parse_file(source).map_err(|error| RustItemEditError::InvalidSource {
        message: error.to_string(),
    })?;
    let item = syn::parse_str::<syn::Item>(&edit.item).map_err(|error| {
        RustItemEditError::InvalidItem {
            message: error.to_string(),
        }
    })?;
    let identity = ItemIdentity::from_item(&item)?;
    let matches: Vec<_> = file
        .items
        .iter()
        .filter(|candidate| ItemIdentity::from_source_item(candidate).as_ref() == Some(&identity))
        .collect();

    let edited = match edit.mode {
        RustItemMode::Insert => match matches.len() {
            0 => insert_item(source, &edit.item),
            1 => {
                return Err(RustItemEditError::AlreadyExists {
                    identity: identity.to_string(),
                });
            }
            count => {
                return Err(RustItemEditError::Ambiguous {
                    identity: identity.to_string(),
                    count,
                });
            }
        },
        RustItemMode::Replace => match matches.as_slice() {
            [] => {
                return Err(RustItemEditError::NotFound {
                    identity: identity.to_string(),
                });
            }
            [target] => replace_item(source, target, &edit.item, &identity)?,
            many => {
                return Err(RustItemEditError::Ambiguous {
                    identity: identity.to_string(),
                    count: many.len(),
                });
            }
        },
    };

    syn::parse_file(&edited).map_err(|error| RustItemEditError::InvalidEditedSource {
        message: error.to_string(),
    })?;
    Ok(RustItemEditOutcome {
        identity: identity.to_string(),
        new_revision: rust_source_revision(&edited),
        source: edited,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ItemIdentity {
    kind: &'static str,
    name: String,
}

impl ItemIdentity {
    fn from_item(item: &syn::Item) -> Result<Self, RustItemEditError> {
        let (kind, name) = match item {
            syn::Item::Const(item) => ("const", item.ident.to_string()),
            syn::Item::Enum(item) => ("enum", item.ident.to_string()),
            syn::Item::Fn(item) => ("fn", item.sig.ident.to_string()),
            syn::Item::Mod(item) => ("mod", item.ident.to_string()),
            syn::Item::Static(item) => ("static", item.ident.to_string()),
            syn::Item::Struct(item) => ("struct", item.ident.to_string()),
            syn::Item::Trait(item) => ("trait", item.ident.to_string()),
            syn::Item::Type(item) => ("type", item.ident.to_string()),
            unsupported => {
                return Err(RustItemEditError::UnsupportedItem {
                    kind: unsupported_kind(unsupported),
                });
            }
        };
        if name == "_" {
            return Err(RustItemEditError::UnnamedItem { kind });
        }
        Ok(Self { kind, name })
    }

    fn from_source_item(item: &syn::Item) -> Option<Self> {
        Self::from_item(item).ok()
    }
}

impl fmt::Display for ItemIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.kind, self.name)
    }
}

fn unsupported_kind(item: &syn::Item) -> &'static str {
    match item {
        syn::Item::ExternCrate(_) => "extern-crate",
        syn::Item::ForeignMod(_) => "foreign-mod",
        syn::Item::Impl(_) => "impl",
        syn::Item::Macro(_) => "macro",
        syn::Item::TraitAlias(_) => "trait-alias",
        syn::Item::Union(_) => "union",
        syn::Item::Use(_) => "use",
        syn::Item::Verbatim(_) => "verbatim",
        _ => "unknown",
    }
}

fn insert_item(source: &str, item: &str) -> String {
    let separator = if source.is_empty() || source.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let mut edited = String::with_capacity(source.len() + separator.len() + item.len());
    edited.push_str(source);
    edited.push_str(separator);
    edited.push_str(item);
    edited
}

fn replace_item(
    source: &str,
    target: &syn::Item,
    replacement: &str,
    identity: &ItemIdentity,
) -> Result<String, RustItemEditError> {
    let range = target.span().byte_range();
    validate_range(source, &range).ok_or_else(|| RustItemEditError::InvalidSpan {
        identity: identity.to_string(),
    })?;
    let mut edited = String::with_capacity(source.len() - range.len() + replacement.len());
    edited.push_str(&source[..range.start]);
    edited.push_str(replacement);
    edited.push_str(&source[range.end..]);
    Ok(edited)
}

fn validate_range(source: &str, range: &Range<usize>) -> Option<()> {
    (range.start <= range.end
        && range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end))
    .then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(mode: RustItemMode, item: &str) -> RustItemEdit {
        RustItemEdit {
            mode,
            item: item.to_string(),
        }
    }

    fn apply(source: &str, edit: &RustItemEdit) -> RustItemEditOutcome {
        edit_rust_item(source, &rust_source_revision(source), edit).unwrap()
    }

    #[test]
    fn source_revisions_are_stable_and_byte_sensitive() {
        assert_eq!(rust_source_revision(""), "cbf29ce484222325");
        assert_eq!(
            rust_source_revision("fn one() {}"),
            rust_source_revision("fn one() {}")
        );
        assert_ne!(
            rust_source_revision("fn one() {}"),
            rust_source_revision("fn one() {}\n")
        );
    }

    #[test]
    fn stale_revision_is_rejected_before_source_or_item_parsing() {
        let error = edit_rust_item(
            "not Rust",
            "0000000000000000",
            &edit(RustItemMode::Insert, "also bad"),
        )
        .unwrap_err();
        assert!(matches!(error, RustItemEditError::StaleRevision { .. }));
    }

    #[test]
    fn revisions_and_parser_inputs_are_bounded() {
        assert_eq!(
            edit_rust_item(
                "",
                "not-a-revision",
                &edit(RustItemMode::Insert, "fn item() {}")
            )
            .unwrap_err(),
            RustItemEditError::InvalidRevision
        );
        let source = " ".repeat(MAX_AUTHORED_SOURCE_BYTES + 1);
        assert!(matches!(
            edit_rust_item(
                &source,
                &rust_source_revision(&source),
                &edit(RustItemMode::Insert, "fn item() {}")
            ),
            Err(RustItemEditError::SourceTooLarge { .. })
        ));
        let item = " ".repeat(MAX_RUST_ITEM_BYTES + 1);
        assert!(matches!(
            edit_rust_item(
                "",
                &rust_source_revision(""),
                &edit(RustItemMode::Insert, &item)
            ),
            Err(RustItemEditError::ItemTooLarge { .. })
        ));
    }

    #[test]
    fn inserts_a_named_item_without_rewriting_existing_bytes() {
        let source = "// header\nfn existing() { /* exact */ }\n";
        let outcome = apply(
            source,
            &edit(
                RustItemMode::Insert,
                "#[cfg(test)]\nmod tests { #[test] fn works() {} }",
            ),
        );

        assert_eq!(outcome.identity, "mod:tests");
        assert!(outcome.source.starts_with(source));
        assert!(
            outcome
                .source
                .ends_with("#[cfg(test)]\nmod tests { #[test] fn works() {} }")
        );
        assert_eq!(outcome.new_revision, rust_source_revision(&outcome.source));
        syn::parse_file(&outcome.source).unwrap();
    }

    #[test]
    fn insertion_separates_an_item_from_an_unterminated_final_line() {
        let source = "fn existing() {}";
        let outcome = apply(source, &edit(RustItemMode::Insert, "struct Added;"));
        assert_eq!(outcome.source, "fn existing() {}\nstruct Added;");
    }

    #[test]
    fn replaces_exactly_one_item_and_its_attributes() {
        let source =
            "// keep before\n#[cfg(test)]\nfn target() { old(); }\n\nfn keep() { /* exact */ }\n";
        let outcome = apply(
            source,
            &edit(RustItemMode::Replace, "fn target() { new(); }"),
        );

        assert_eq!(outcome.identity, "fn:target");
        assert_eq!(
            outcome.source,
            "// keep before\nfn target() { new(); }\n\nfn keep() { /* exact */ }\n"
        );
    }

    #[test]
    fn all_initial_named_item_kinds_have_stable_identities() {
        let cases = [
            ("fn item() {}", "fn:item"),
            ("mod item {}", "mod:item"),
            ("struct Item;", "struct:Item"),
            ("enum Item { One }", "enum:Item"),
            ("trait Item {}", "trait:Item"),
            ("type Item = ();", "type:Item"),
            ("const ITEM: () = ();", "const:ITEM"),
            ("static ITEM: () = ();", "static:ITEM"),
        ];
        for (item, identity) in cases {
            let outcome = apply("", &edit(RustItemMode::Insert, item));
            assert_eq!(outcome.identity, identity);
        }
    }

    #[test]
    fn replacement_ranges_cover_each_supported_item_shape() {
        let cases = [
            ("fn Item() { old(); }", "fn Item() { new(); }"),
            ("mod Item { fn old() {} }", "mod Item { fn new() {} }"),
            ("struct Item;", "struct Item { value: u8 }"),
            ("enum Item { Old }", "enum Item { New }"),
            ("trait Item { fn old(); }", "trait Item { fn new(); }"),
            ("type Item = u8;", "type Item = u16;"),
            ("const Item: u8 = 1;", "const Item: u8 = 2;"),
            ("static Item: u8 = 1;", "static Item: u8 = 2;"),
        ];
        for (before, replacement) in cases {
            let source = format!("// before\n{before}\n// after\n");
            let outcome = apply(&source, &edit(RustItemMode::Replace, replacement));
            assert_eq!(
                outcome.source,
                format!("// before\n{replacement}\n// after\n")
            );
        }
    }

    #[test]
    fn insertion_requires_the_identity_to_be_absent() {
        let source = "fn same() {}\n";
        let error = edit_rust_item(
            source,
            &rust_source_revision(source),
            &edit(RustItemMode::Insert, "fn same() { changed(); }"),
        )
        .unwrap_err();
        assert_eq!(
            error,
            RustItemEditError::AlreadyExists {
                identity: "fn:same".to_string()
            }
        );
    }

    #[test]
    fn replacement_requires_the_identity_to_exist() {
        let source = "fn other() {}\n";
        let error = edit_rust_item(
            source,
            &rust_source_revision(source),
            &edit(RustItemMode::Replace, "fn missing() {}"),
        )
        .unwrap_err();
        assert_eq!(
            error,
            RustItemEditError::NotFound {
                identity: "fn:missing".to_string()
            }
        );
    }

    #[test]
    fn duplicate_source_identities_are_ambiguous() {
        let source = "fn same() {}\nfn same() {}\n";
        let error = edit_rust_item(
            source,
            &rust_source_revision(source),
            &edit(RustItemMode::Replace, "fn same() { changed(); }"),
        )
        .unwrap_err();
        assert_eq!(
            error,
            RustItemEditError::Ambiguous {
                identity: "fn:same".to_string(),
                count: 2
            }
        );
    }

    #[test]
    fn unsupported_invalid_and_multiple_items_are_rejected() {
        let source = "";
        let revision = rust_source_revision(source);
        assert_eq!(
            edit_rust_item(
                source,
                &revision,
                &edit(RustItemMode::Insert, "use crate::Thing;")
            )
            .unwrap_err(),
            RustItemEditError::UnsupportedItem { kind: "use" }
        );
        assert!(matches!(
            edit_rust_item(source, &revision, &edit(RustItemMode::Insert, "fn")),
            Err(RustItemEditError::InvalidItem { .. })
        ));
        assert!(matches!(
            edit_rust_item(
                source,
                &revision,
                &edit(RustItemMode::Insert, "fn one() {} fn two() {}")
            ),
            Err(RustItemEditError::InvalidItem { .. })
        ));
    }

    #[test]
    fn invalid_authored_source_is_rejected_structurally() {
        let source = "fn broken(";
        assert!(matches!(
            edit_rust_item(
                source,
                &rust_source_revision(source),
                &edit(RustItemMode::Insert, "fn valid() {}")
            ),
            Err(RustItemEditError::InvalidSource { .. })
        ));
    }
}
