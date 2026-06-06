//! Artifact path sequencing, file writing, and metadata helpers.

use std::fs;
use std::path::{Path, PathBuf};

use agentmux_core::{
    AgentmuxError, ArtifactId, ArtifactKind, DateTimeUtc, ProjectId, TaskId, error::Result,
};

use super::types::Artifact;

pub(crate) fn artifact_path(
    artifacts_dir: &Path,
    kind: &str,
    agent_segment: &str,
    sequence: u32,
    ext: &str,
) -> PathBuf {
    artifacts_dir.join(format!("{kind}-{agent_segment}-{sequence:03}.{ext}"))
}

pub(crate) fn next_artifact_sequence(
    artifacts_dir: &Path,
    kind: &str,
    agent_segment: &str,
    ext: &str,
) -> Result<u32> {
    let mut sequence = 1;
    if !artifacts_dir.exists() {
        return Ok(sequence);
    }

    let prefix = format!("{kind}-{agent_segment}-");
    let suffix = format!(".{ext}");
    for entry in fs::read_dir(artifacts_dir).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to read artifacts dir {}: {error}",
            artifacts_dir.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            AgentmuxError::Internal(format!("failed to read artifact dir entry: {error}"))
        })?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(raw_sequence) = file_name
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(&suffix))
        else {
            continue;
        };
        if let Ok(found) = raw_sequence.parse::<u32>() {
            sequence = sequence.max(found.saturating_add(1));
        }
    }

    Ok(sequence)
}

pub(crate) fn write_artifact_file(path: &Path, contents: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Err(AgentmuxError::Internal(format!(
            "artifact path has no parent: {}",
            path.display()
        )));
    };
    fs::create_dir_all(parent).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to create artifact dir {}: {error}",
            parent.display()
        ))
    })?;
    fs::write(path, contents).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to write artifact {}: {error}",
            path.display()
        ))
    })
}

pub(crate) fn artifact_metadata(
    project_id: ProjectId,
    task_id: Option<TaskId>,
    kind: ArtifactKind,
    path: PathBuf,
    title: String,
    mime_type: Option<String>,
) -> Result<Artifact> {
    let metadata = fs::metadata(&path).map_err(|error| {
        AgentmuxError::Internal(format!(
            "failed to read artifact metadata {}: {error}",
            path.display()
        ))
    })?;
    Ok(Artifact {
        id: ArtifactId::new(),
        project_id,
        task_id,
        kind,
        path,
        title,
        mime_type,
        size_bytes: metadata.len(),
        checksum: None,
        created_at: DateTimeUtc::now_utc(),
    })
}
