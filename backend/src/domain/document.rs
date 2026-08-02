//! Knowledge-base documents (ADR-063): a user uploads text, it is chunked and
//! embedded (by the agent, ADR-006), and the chunks ground the agent's LLM
//! reasoning via retrieval. Pure domain: the raw content is never stored here —
//! only the document metadata and, in the repo, its embedded chunks.

use chrono::{DateTime, Utc};
use uuid::Uuid;

pub const MAX_DOCUMENT_NAME_LEN: usize = 200;
pub const MAX_DOCUMENT_CONTENT_LEN: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentStatus {
    /// Embedding dispatched to the agent, chunks not yet stored.
    Pending,
    /// Chunks embedded and stored — retrievable.
    Ready,
    Failed,
}

impl DocumentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DocumentError {
    #[error("document name must not be empty")]
    EmptyName,
    #[error("document name must be at most 200 characters")]
    NameTooLong,
    #[error("document content must not be empty")]
    EmptyContent,
    #[error("document content must be at most 100000 characters")]
    ContentTooLong,
}

/// Validates raw upload content (ADR-063): not stored on the document, embedded
/// by the agent then discarded, so it is only bounded, not persisted here.
pub fn validate_content(content: &str) -> Result<(), DocumentError> {
    if content.trim().is_empty() {
        return Err(DocumentError::EmptyContent);
    }
    if content.chars().count() > MAX_DOCUMENT_CONTENT_LEN {
        return Err(DocumentError::ContentTooLong);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub status: DocumentStatus,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Document {
    pub fn new(user_id: Uuid, name: &str) -> Result<Self, DocumentError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DocumentError::EmptyName);
        }
        if name.chars().count() > MAX_DOCUMENT_NAME_LEN {
            return Err(DocumentError::NameTooLong);
        }
        Ok(Self {
            id: Uuid::new_v4(),
            user_id,
            name: name.to_string(),
            status: DocumentStatus::Pending,
            error: None,
            created_at: super::now_utc(),
        })
    }
}

/// A chunk produced by the agent's embedder (ADR-063), sent back to be stored.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChunk {
    pub seq: i32,
    pub content: String,
    pub embedding: Vec<f32>,
}

/// A chunk returned by a similarity search (ADR-063) — what grounds the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievedChunk {
    pub content: String,
    pub document_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_document_starts_pending_with_trimmed_name() {
        let doc = Document::new(Uuid::new_v4(), "  notes.md  ").unwrap();
        assert_eq!(doc.name, "notes.md");
        assert_eq!(doc.status, DocumentStatus::Pending);
    }

    #[test]
    fn name_is_validated() {
        assert_eq!(
            Document::new(Uuid::new_v4(), "   ").unwrap_err(),
            DocumentError::EmptyName
        );
        assert_eq!(
            Document::new(Uuid::new_v4(), &"x".repeat(MAX_DOCUMENT_NAME_LEN + 1)).unwrap_err(),
            DocumentError::NameTooLong
        );
    }

    #[test]
    fn content_is_validated() {
        assert!(validate_content("hello").is_ok());
        assert_eq!(validate_content("  "), Err(DocumentError::EmptyContent));
        assert_eq!(
            validate_content(&"x".repeat(MAX_DOCUMENT_CONTENT_LEN + 1)),
            Err(DocumentError::ContentTooLong)
        );
    }
}
