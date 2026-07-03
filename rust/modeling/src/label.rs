//! Contract type labels: the container grammar every projection reads.
//!
//! A label names a base type wrapped in containers — `Option<…>`, `Vec<…>`, and
//! `BTreeMap<String, …>`. The schema, parser, proto, and verification
//! projections all read labels through this one grammar, so a label means the
//! same thing on every surface.

/// The inner label of an `Option<…>`, when the label is one.
pub(crate) fn optional_inner(label: &str) -> Option<&str> {
    label.strip_prefix("Option<")?.strip_suffix('>')
}

/// The element label of a `Vec<…>`, when the label is one.
pub(crate) fn vec_inner(label: &str) -> Option<&str> {
    label.strip_prefix("Vec<")?.strip_suffix('>')
}

/// The value label of a `BTreeMap<String, …>`, when the label is one. Keys are
/// strings, so the value carries the label's type content.
pub(crate) fn map_value(label: &str) -> Option<&str> {
    let inner = label.strip_prefix("BTreeMap<")?.strip_suffix('>')?;
    inner.split_once(',').map(|(_, value)| value.trim())
}

/// One container layer's inner label, whichever container the label wraps.
pub(crate) fn container_inner(label: &str) -> Option<&str> {
    optional_inner(label)
        .or_else(|| vec_inner(label))
        .or_else(|| map_value(label))
}

/// A label's base type: the name left after stripping every container layer.
pub(crate) fn base_label(label: &str) -> &str {
    match container_inner(label) {
        Some(inner) => base_label(inner),
        None => label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammar_strips_containers_however_nested() {
        assert_eq!(base_label("String"), "String");
        assert_eq!(base_label("Option<String>"), "String");
        assert_eq!(base_label("Vec<Edit>"), "Edit");
        assert_eq!(base_label("BTreeMap<String, Edit>"), "Edit");
        assert_eq!(base_label("Option<BTreeMap<String, String>>"), "String");
        assert_eq!(base_label("Vec<Option<Edit>>"), "Edit");
        assert_eq!(
            map_value("BTreeMap<String, Vec<String>>"),
            Some("Vec<String>")
        );
    }
}
