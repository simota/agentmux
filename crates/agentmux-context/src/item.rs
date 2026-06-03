//! `ContextItem` domain struct.
//!
//! See `docs/spec/03_domain_model.md §9`.
//!
//! `ContextKind` is defined in `agentmux-core::enums` and re-exported from
//! this crate's root for convenience.

use std::path::PathBuf;

use agentmux_core::{
    ArtifactId, ContextItemId, ContextKind, ContextScope, ContextSource, DateTimeUtc, ProjectId,
    TaskId, Visibility,
};
use serde::{Deserialize, Serialize};

/// A piece of shared knowledge attached to a project, task, or agent session.
///
/// Context items are the primary mechanism for sharing information between
/// agents without coupling them directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub id: ContextItemId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub scope: ContextScope,
    pub kind: ContextKind,
    pub title: String,
    /// Plain-text or Markdown body. If large, the body may be stored on disk
    /// and this field holds a summary; use the mailbox file for the full content.
    pub body: String,
    pub source: ContextSource,
    pub visibility: Visibility,
    /// Detector confidence that this item is still accurate (0.0–1.0).
    pub confidence: f32,
    pub tags: Vec<String>,
    pub related_files: Vec<PathBuf>,
    pub artifact_refs: Vec<ArtifactId>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}
