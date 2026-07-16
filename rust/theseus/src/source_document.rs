use serde::{Deserialize, Serialize};

const SOURCE_CONTENT_CAP: usize = 8_000;

/// One workspace text file together with the revision required by authored edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDocument {
    /// Workspace-relative path that was read.
    pub path: String,
    /// Stable revision of the complete, uncapped source.
    pub revision: String,
    /// Leading source text retained for the tool result.
    pub contents: String,
    /// Whether `contents` omits the remainder of the file.
    pub truncated: bool,
}

impl SourceDocument {
    /// A document whose `contents` is the source's outline — its top-level item
    /// signatures — while its `revision` still covers the complete file, so a
    /// follow-up `edit_rust_item` uses the same revision. An outline is never
    /// truncated.
    pub(crate) fn outline(path: String, source: &str, outline: String) -> Self {
        Self {
            path,
            revision: theseus_modeling::rust_source_revision(source),
            contents: outline,
            truncated: false,
        }
    }

    pub(crate) fn new(path: String, source: &str) -> Self {
        let revision = theseus_modeling::rust_source_revision(source);
        match source.char_indices().nth(SOURCE_CONTENT_CAP) {
            None => Self {
                path,
                revision,
                contents: source.to_string(),
                truncated: false,
            },
            Some((byte, _)) => Self {
                path,
                revision,
                contents: source[..byte].to_string(),
                truncated: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_covers_text_omitted_from_the_tool_result() {
        let prefix = "a".repeat(SOURCE_CONTENT_CAP);
        let first = SourceDocument::new("source.rs".to_string(), &format!("{prefix}x"));
        let second = SourceDocument::new("source.rs".to_string(), &format!("{prefix}y"));

        assert!(first.truncated);
        assert_eq!(first.contents, second.contents);
        assert_ne!(first.revision, second.revision);
    }
}
