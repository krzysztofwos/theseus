//! The authored adapter implementing the generated contract.
//!
//! A method without a handler here falls through to the trait's `unimplemented`
//! default, and the coverage check reports it. The structured-edit tooling writes
//! the handlers into this file.

pub use crate::generated::TextUtilsService;

/// The TextUtils adapter.
pub struct TextUtils;

#[async_trait::async_trait]
impl TextUtilsService for TextUtils {
    async fn capitalize(
        &self,
        request: crate::generated::CapitalizeRequest,
    ) -> anyhow::Result<String> {
        let result = request
            .input
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => {
                        let upper: String = first.to_uppercase().collect();
                        upper + chars.as_str()
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        Ok(result)
    }

    async fn truncate(&self, request: crate::generated::TruncateRequest) -> anyhow::Result<String> {
        let max = request.max_chars as usize;
        let input = &request.input;
        if input.chars().count() <= max {
            Ok(input.clone())
        } else {
            // Truncate to max chars and append ellipsis
            let truncated: String = input.chars().take(max).collect();
            Ok(format!("{}…", truncated))
        }
    }

    async fn word_count(
        &self,
        request: crate::generated::WordCountRequest,
    ) -> anyhow::Result<String> {
        let count = request.input.split_whitespace().count() as u32;
        Ok(count.to_string())
    }

    async fn slugify(&self, request: crate::generated::SlugifyRequest) -> anyhow::Result<String> {
        let slug = request
            .input
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>();
        // Collapse consecutive hyphens and trim leading/trailing hyphens
        let slug = slug
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        Ok(slug)
    }
}
