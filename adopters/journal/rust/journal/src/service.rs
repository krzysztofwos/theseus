//! The authored adapter implementing the generated contract.
//!
//! A method without a handler here falls through to the trait's `unimplemented`
//! default, and the coverage check reports it. The structured-edit tooling writes
//! the handlers into this file.

pub use crate::generated::{JournalService, Ctx};

#[async_trait::async_trait]
impl JournalService for Ctx<'_> {
    async fn add(&self, request: crate::generated::AddRequest) -> anyhow::Result<String> {
        self.store.append(&request.text).await?;
        Ok(format!("recorded: {}", request.text))
    }

    async fn list(&self) -> anyhow::Result<String> {
        self.store.read_all().await
    }

    async fn search(&self, request: crate::generated::SearchRequest) -> anyhow::Result<String> {
        let entries = self.store.read_all().await?;
        Ok(entries
            .lines()
            .filter(|line| line.contains(&request.term))
            .collect::<Vec<_>>()
            .join("\n"))
    }
}
