//! The Journal service

mod generated;
mod service;

pub use generated::*;

/// A [`Store`] over one plain-text file: an entry per line. The journal's
/// shared adapter for its inbound binaries.
pub struct FileStore {
    path: std::path::PathBuf,
}

impl FileStore {
    /// A store at `JOURNAL_FILE`, or `journal.log` beside the current directory.
    pub fn from_env() -> Self {
        let path = std::env::var("JOURNAL_FILE").unwrap_or_else(|_| "journal.log".to_string());
        Self { path: path.into() }
    }
}

#[async_trait::async_trait]
impl Store for FileStore {
    async fn append(&self, request: &str) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_to_string(&self.path)
            .await
            .unwrap_or_default();
        entries.push_str(request);
        entries.push('\n');
        tokio::fs::write(&self.path, entries).await?;
        Ok(())
    }

    async fn read_all(&self) -> anyhow::Result<String> {
        Ok(tokio::fs::read_to_string(&self.path).await.unwrap_or_default())
    }
}

