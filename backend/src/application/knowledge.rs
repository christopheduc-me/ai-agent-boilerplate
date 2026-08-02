//! Knowledge-base use case (ADR-063): upload/list/delete documents, receive the
//! agent's embedded chunks, and retrieve for grounding. The backend owns the
//! store; the agent embeds and retrieves through the internal API (ADR-006).

use std::sync::Arc;

use uuid::Uuid;

use crate::domain::document::{validate_content, DocumentError};
use crate::domain::ports::{DocumentRepository, EmbedDispatcher, JobRepository, PortError};
use crate::domain::{Document, NewChunk, RetrievedChunk};

#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    #[error("document not found")]
    DocumentNotFound,
    #[error("job not found")]
    JobNotFound,
    #[error(transparent)]
    Invalid(#[from] DocumentError),
    #[error(transparent)]
    Infrastructure(#[from] PortError),
}

pub struct Knowledge {
    documents: Arc<dyn DocumentRepository>,
    jobs: Arc<dyn JobRepository>,
    embed: Arc<dyn EmbedDispatcher>,
}

impl Knowledge {
    pub fn new(
        documents: Arc<dyn DocumentRepository>,
        jobs: Arc<dyn JobRepository>,
        embed: Arc<dyn EmbedDispatcher>,
    ) -> Self {
        Self {
            documents,
            jobs,
            embed,
        }
    }

    /// Uploads a document: persists it `pending` and dispatches the embedding to
    /// the agent (ADR-063). A dispatch failure marks it `failed` — the raw
    /// content is never stored, only its chunks once embedded.
    pub async fn upload(
        &self,
        user_id: Uuid,
        name: &str,
        content: &str,
    ) -> Result<Document, KnowledgeError> {
        validate_content(content)?;
        let document = Document::new(user_id, name)?;
        self.documents.insert(&document).await?;
        if let Err(e) = self
            .embed
            .dispatch_embed(document.id, &document.name, content)
            .await
        {
            tracing::error!(document_id = %document.id, error = %e, "embed dispatch failed");
            self.documents
                .mark_failed(document.id, &format!("embedding dispatch failed: {e}"))
                .await
                .ok();
        }
        Ok(document)
    }

    pub async fn list(&self, user_id: Uuid) -> Result<Vec<Document>, PortError> {
        self.documents.list_for_user(user_id).await
    }

    pub async fn delete(&self, user_id: Uuid, id: Uuid) -> Result<bool, PortError> {
        self.documents.delete(user_id, id).await
    }

    /// Stores the agent's embedded chunks and marks the document ready (ADR-063).
    pub async fn store_chunks(
        &self,
        document_id: Uuid,
        chunks: &[NewChunk],
    ) -> Result<(), KnowledgeError> {
        let document = self
            .documents
            .find(document_id)
            .await?
            .ok_or(KnowledgeError::DocumentNotFound)?;
        self.documents.store_chunks(&document, chunks).await?;
        Ok(())
    }

    pub async fn mark_failed(&self, document_id: Uuid, error: &str) -> Result<(), PortError> {
        self.documents.mark_failed(document_id, error).await
    }

    /// Retrieves the top-`k` chunks that ground a job's LLM reasoning (ADR-063).
    /// Scoped to the job's owner — the agent never passes a user id, so it can
    /// only reach the knowledge base of the job it is processing.
    pub async fn retrieve_for_job(
        &self,
        job_id: Uuid,
        embedding: &[f32],
        k: i64,
    ) -> Result<Vec<RetrievedChunk>, KnowledgeError> {
        let job = self
            .jobs
            .find(job_id)
            .await?
            .ok_or(KnowledgeError::JobNotFound)?;
        Ok(self.documents.retrieve(job.user_id, embedding, k).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::persistence::in_memory::{
        InMemoryDocumentRepository, InMemoryJobRepository,
    };
    use crate::domain::ResearchJob;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingEmbed {
        dispatched: Mutex<Vec<(Uuid, String)>>,
    }

    #[async_trait::async_trait]
    impl EmbedDispatcher for RecordingEmbed {
        async fn dispatch_embed(
            &self,
            document_id: Uuid,
            name: &str,
            _content: &str,
        ) -> Result<(), PortError> {
            self.dispatched
                .lock()
                .unwrap()
                .push((document_id, name.into()));
            Ok(())
        }
    }

    fn knowledge() -> (
        Knowledge,
        Arc<InMemoryDocumentRepository>,
        Arc<InMemoryJobRepository>,
        Arc<RecordingEmbed>,
    ) {
        let docs = Arc::new(InMemoryDocumentRepository::default());
        let jobs = Arc::new(InMemoryJobRepository::default());
        let embed = Arc::new(RecordingEmbed::default());
        (
            Knowledge::new(docs.clone(), jobs.clone(), embed.clone()),
            docs,
            jobs,
            embed,
        )
    }

    #[tokio::test]
    async fn upload_persists_pending_and_dispatches_embedding() {
        let (kb, docs, _, embed) = knowledge();
        let user = Uuid::new_v4();

        let doc = kb.upload(user, "notes.md", "hello world").await.unwrap();

        assert_eq!(doc.status.as_str(), "pending");
        assert_eq!(docs.list_for_user(user).await.unwrap().len(), 1);
        assert_eq!(embed.dispatched.lock().unwrap().len(), 1);
        assert_eq!(embed.dispatched.lock().unwrap()[0].0, doc.id);
    }

    #[tokio::test]
    async fn upload_validates_content() {
        let (kb, _, _, _) = knowledge();
        assert!(matches!(
            kb.upload(Uuid::new_v4(), "n", "  ").await.unwrap_err(),
            KnowledgeError::Invalid(DocumentError::EmptyContent)
        ));
    }

    #[tokio::test]
    async fn store_chunks_marks_ready_and_retrieval_is_scoped_to_the_job_owner() {
        let (kb, docs, jobs, _) = knowledge();
        let user = Uuid::new_v4();
        let doc = kb.upload(user, "notes.md", "content").await.unwrap();

        kb.store_chunks(
            doc.id,
            &[NewChunk {
                seq: 0,
                content: "the sky is blue".into(),
                embedding: vec![1.0, 0.0, 0.0],
            }],
        )
        .await
        .unwrap();
        assert_eq!(
            docs.find(doc.id).await.unwrap().unwrap().status.as_str(),
            "ready"
        );

        // A job owned by the user retrieves its chunks.
        let job = ResearchJob::new(user, "sky").unwrap();
        jobs.insert(&job).await.unwrap();
        let hits = kb
            .retrieve_for_job(job.id, &[1.0, 0.0, 0.0], 5)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].content, "the sky is blue");

        // A job owned by someone else sees nothing.
        let other = ResearchJob::new(Uuid::new_v4(), "sky").unwrap();
        jobs.insert(&other).await.unwrap();
        assert!(kb
            .retrieve_for_job(other.id, &[1.0, 0.0, 0.0], 5)
            .await
            .unwrap()
            .is_empty());
    }
}
