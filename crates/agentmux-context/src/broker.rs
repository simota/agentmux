//! In-memory context broker.
//!
//! Persistence is a daemon/store responsibility in later slices. This module
//! keeps v0.1 context CRUD, pack selection, and mailbox file rendering small
//! and unit-testable.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use agentmux_core::{
    AgentmuxError, ArtifactId, ContextItemId, ContextKind, ContextScope, ContextSource,
    DateTimeUtc, ProjectId, TaskId, Visibility, error::Result,
};

use crate::ContextItem;

#[derive(Debug, Default)]
pub struct ContextBroker {
    items: BTreeMap<ContextItemId, ContextItem>,
}

impl ContextBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_item(&mut self, input: NewContextItem) -> Result<ContextItem> {
        validate_title_and_body(&input.title, &input.body)?;
        validate_confidence(input.confidence)?;

        let now = DateTimeUtc::now_utc();
        let item = ContextItem {
            id: ContextItemId::new(),
            project_id: input.project_id,
            task_id: input.task_id,
            scope: input.scope,
            kind: input.kind,
            title: input.title,
            body: input.body,
            source: input.source,
            visibility: input.visibility,
            confidence: input.confidence,
            tags: input.tags,
            related_files: input.related_files,
            artifact_refs: input.artifact_refs,
            created_at: now,
            updated_at: now,
        };

        self.items.insert(item.id.clone(), item.clone());
        Ok(item)
    }

    pub fn get_item(&self, id: &ContextItemId) -> Option<&ContextItem> {
        self.items.get(id)
    }

    pub fn list_items(&self, project_id: &ProjectId) -> Vec<&ContextItem> {
        self.items
            .values()
            .filter(|item| &item.project_id == project_id)
            .collect()
    }

    pub fn update_item(
        &mut self,
        id: &ContextItemId,
        update: ContextUpdate,
    ) -> Result<ContextItem> {
        let item = self.item_mut(id)?;

        if let Some(title) = update.title {
            if title.trim().is_empty() {
                return Err(AgentmuxError::UserError(
                    "context title must not be empty".to_string(),
                ));
            }
            item.title = title;
        }
        if let Some(body) = update.body {
            if body.trim().is_empty() {
                return Err(AgentmuxError::UserError(
                    "context body must not be empty".to_string(),
                ));
            }
            item.body = body;
        }
        if let Some(confidence) = update.confidence {
            validate_confidence(confidence)?;
            item.confidence = confidence;
        }
        if let Some(visibility) = update.visibility {
            item.visibility = visibility;
        }
        if let Some(tags) = update.tags {
            item.tags = tags;
        }
        if let Some(related_files) = update.related_files {
            item.related_files = related_files;
        }
        if let Some(artifact_refs) = update.artifact_refs {
            item.artifact_refs = artifact_refs;
        }
        item.updated_at = DateTimeUtc::now_utc();

        Ok(item.clone())
    }

    pub fn archive_item(&mut self, id: &ContextItemId) -> Result<ContextItem> {
        self.items
            .remove(id)
            .ok_or_else(|| unknown_context_item(id))
    }

    pub fn select_pack(&self, request: ContextPackRequest) -> Result<ContextPack> {
        self.select_pack_inner(&request, None)
    }

    pub fn select_pack_with_mailbox(
        &self,
        request: ContextPackRequest,
        mailbox: MailboxConfig,
    ) -> Result<ContextPack> {
        validate_mailbox_agent(&mailbox.agent_name)?;
        self.select_pack_inner(&request, Some(mailbox))
    }

    fn select_pack_inner(
        &self,
        request: &ContextPackRequest,
        mailbox: Option<MailboxConfig>,
    ) -> Result<ContextPack> {
        if request.max_inline_chars == 0 {
            return Err(AgentmuxError::UserError(
                "max_inline_chars must be greater than zero".to_string(),
            ));
        }

        let mut candidates = self.pack_candidates(request)?;
        candidates.sort_by_key(|item| selection_key(item, request));

        let mut inline_items = Vec::new();
        let mut mailbox_files = Vec::new();
        let mut omitted_items = Vec::new();
        let mut artifact_refs = BTreeSet::new();
        let mut used_chars = 0usize;

        for item in candidates {
            let item_chars = item.title.chars().count() + item.body.chars().count();
            if used_chars + item_chars <= request.max_inline_chars {
                used_chars += item_chars;
                for artifact_id in &item.artifact_refs {
                    artifact_refs.insert(artifact_id.clone());
                }
                inline_items.push(item.clone());
            } else if let Some(mailbox) = &mailbox {
                let path = write_mailbox_file(mailbox, item)?;
                for artifact_id in &item.artifact_refs {
                    artifact_refs.insert(artifact_id.clone());
                }
                mailbox_files.push(path);
            } else {
                omitted_items.push(item.id.clone());
            }
        }

        Ok(ContextPack {
            inline_items,
            mailbox_files,
            artifact_refs: artifact_refs.into_iter().collect(),
            omitted_items,
        })
    }

    fn pack_candidates(&self, request: &ContextPackRequest) -> Result<Vec<&ContextItem>> {
        let mut candidates: BTreeMap<ContextItemId, &ContextItem> = BTreeMap::new();

        for id in &request.attached_context_ids {
            let item = self.items.get(id).ok_or_else(|| unknown_context_item(id))?;
            if item.project_id != request.project_id {
                return Err(AgentmuxError::UserError(format!(
                    "context item '{id}' belongs to a different project"
                )));
            }
            candidates.insert(item.id.clone(), item);
        }

        for item in self.items.values() {
            if item.project_id != request.project_id {
                continue;
            }
            if item.visibility == Visibility::Restricted
                && !request.attached_context_ids.contains(&item.id)
            {
                continue;
            }
            if item.scope == ContextScope::Task && item.task_id != request.task_id {
                continue;
            }
            candidates.insert(item.id.clone(), item);
        }

        Ok(candidates.into_values().collect())
    }

    fn item_mut(&mut self, id: &ContextItemId) -> Result<&mut ContextItem> {
        self.items
            .get_mut(id)
            .ok_or_else(|| unknown_context_item(id))
    }
}

#[derive(Debug, Clone)]
pub struct NewContextItem {
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub scope: ContextScope,
    pub kind: ContextKind,
    pub title: String,
    pub body: String,
    pub source: ContextSource,
    pub visibility: Visibility,
    pub confidence: f32,
    pub tags: Vec<String>,
    pub related_files: Vec<PathBuf>,
    pub artifact_refs: Vec<ArtifactId>,
}

#[derive(Debug, Clone, Default)]
pub struct ContextUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
    pub visibility: Option<Visibility>,
    pub confidence: Option<f32>,
    pub tags: Option<Vec<String>>,
    pub related_files: Option<Vec<PathBuf>>,
    pub artifact_refs: Option<Vec<ArtifactId>>,
}

#[derive(Debug, Clone)]
pub struct ContextPackRequest {
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub attached_context_ids: Vec<ContextItemId>,
    pub max_inline_chars: usize,
}

#[derive(Debug, Clone)]
pub struct MailboxConfig {
    pub project_root: PathBuf,
    pub agent_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextPack {
    pub inline_items: Vec<ContextItem>,
    pub mailbox_files: Vec<PathBuf>,
    pub artifact_refs: Vec<ArtifactId>,
    pub omitted_items: Vec<ContextItemId>,
}

fn selection_key(item: &ContextItem, request: &ContextPackRequest) -> (u8, u8, u8, ContextItemId) {
    let attached_rank = if request.attached_context_ids.contains(&item.id) {
        0
    } else {
        1
    };
    let scope_rank = match item.scope {
        ContextScope::Agent => 0,
        ContextScope::Task => 1,
        ContextScope::Project => 2,
    };
    let kind_rank = match item.kind {
        ContextKind::Risk | ContextKind::Decision | ContextKind::CodingRule => 0,
        ContextKind::TaskBrief | ContextKind::HandoffSummary | ContextKind::ErrorLog => 1,
        _ => 2,
    };

    (attached_rank, scope_rank, kind_rank, item.id.clone())
}

fn validate_title_and_body(title: &str, body: &str) -> Result<()> {
    if title.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "context title must not be empty".to_string(),
        ));
    }
    if body.trim().is_empty() {
        return Err(AgentmuxError::UserError(
            "context body must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn validate_confidence(confidence: f32) -> Result<()> {
    if !(0.0..=1.0).contains(&confidence) {
        return Err(AgentmuxError::UserError(
            "context confidence must be between 0.0 and 1.0".to_string(),
        ));
    }
    Ok(())
}

fn unknown_context_item(id: &ContextItemId) -> AgentmuxError {
    AgentmuxError::UserError(format!("unknown context item '{id}'"))
}

fn validate_mailbox_agent(agent_name: &str) -> Result<()> {
    if agent_name.is_empty()
        || agent_name == "."
        || agent_name == ".."
        || agent_name.contains('/')
        || agent_name.contains('\\')
    {
        return Err(AgentmuxError::UserError(
            "mailbox agent name must be a single safe path segment".to_string(),
        ));
    }
    Ok(())
}

fn write_mailbox_file(mailbox: &MailboxConfig, item: &ContextItem) -> Result<PathBuf> {
    let relative_path = PathBuf::from(".agentmux")
        .join("inbox")
        .join(&mailbox.agent_name)
        .join(format!("ctx-{}.md", item.id));
    let absolute_path = mailbox.project_root.join(&relative_path);
    let parent = absolute_path.parent().ok_or_else(|| {
        AgentmuxError::Internal(format!(
            "mailbox path '{}' has no parent",
            absolute_path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(mailbox_io_error)?;

    let (body, redacted) = redact_basic(&item.body);
    let contents = render_mailbox_file(item, &body, redacted);
    fs::write(&absolute_path, contents).map_err(mailbox_io_error)?;

    Ok(relative_path)
}

fn render_mailbox_file(item: &ContextItem, body: &str, redacted: bool) -> String {
    format!(
        "---\ncontext_id: {}\nkind: {:?}\ncreated_at: {}\nsource: {}\nredacted: {}\n---\n\n# {:?}: {}\n\n{}\n",
        item.id,
        item.kind,
        item.created_at,
        source_label(&item.source),
        redacted,
        item.kind,
        item.title,
        body
    )
}

fn source_label(source: &ContextSource) -> String {
    match source {
        ContextSource::Human => "human".to_string(),
        ContextSource::Agent(id) => format!("agent:{id}"),
        ContextSource::System => "system".to_string(),
        ContextSource::Import => "import".to_string(),
    }
}

fn mailbox_io_error(error: std::io::Error) -> AgentmuxError {
    AgentmuxError::StoreError(format!("mailbox write failed: {error}"))
}

fn redact_basic(input: &str) -> (String, bool) {
    let mut redacted = false;
    let mut output = Vec::new();

    for line in input.lines() {
        let lower = line.to_ascii_lowercase();
        if contains_secret_key(&lower) {
            redacted = true;
            output.push(redact_secret_line(line));
        } else {
            output.push(line.to_string());
        }
    }

    let mut text = output.join("\n");
    for marker in ["sk-", "ghp_", "gho_", "github_pat_"] {
        if text.contains(marker) {
            text = redact_marker_tokens(&text, marker);
            redacted = true;
        }
    }

    (text, redacted)
}

fn contains_secret_key(lower: &str) -> bool {
    [
        "api_key", "apikey", "token", "password", "secret", "cookie", "session",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn redact_secret_line(line: &str) -> String {
    for separator in ['=', ':'] {
        if let Some(index) = line.find(separator) {
            let (prefix, _) = line.split_at(index + separator.len_utf8());
            return format!("{prefix} [REDACTED]");
        }
    }
    "[REDACTED]".to_string()
}

fn redact_marker_tokens(input: &str, marker: &str) -> String {
    input
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|word| {
                    if word.contains(marker) {
                        "[REDACTED]"
                    } else {
                        word
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn new_item(
        project_id: ProjectId,
        task_id: Option<TaskId>,
        scope: ContextScope,
        kind: ContextKind,
        title: &str,
        body: &str,
    ) -> NewContextItem {
        NewContextItem {
            project_id,
            task_id,
            scope,
            kind,
            title: title.to_string(),
            body: body.to_string(),
            source: ContextSource::Human,
            visibility: Visibility::Internal,
            confidence: 0.9,
            tags: Vec::new(),
            related_files: Vec::new(),
            artifact_refs: Vec::new(),
        }
    }

    #[test]
    fn create_read_update_and_archive_context_item() {
        let mut broker = ContextBroker::new();
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let item = broker
            .create_item(new_item(
                project_id.clone(),
                Some(task_id.clone()),
                ContextScope::Task,
                ContextKind::TaskBrief,
                "Task brief",
                "Fix the failing parser test.",
            ))
            .expect("context item is created");

        assert_eq!(broker.get_item(&item.id).unwrap().title, "Task brief");
        assert_eq!(broker.list_items(&project_id).len(), 1);

        let updated = broker
            .update_item(
                &item.id,
                ContextUpdate {
                    title: Some("Updated brief".to_string()),
                    confidence: Some(0.75),
                    tags: Some(vec!["parser".to_string()]),
                    ..ContextUpdate::default()
                },
            )
            .expect("context item is updated");

        assert_eq!(updated.title, "Updated brief");
        assert_eq!(updated.confidence, 0.75);
        assert_eq!(updated.tags, vec!["parser"]);

        let archived = broker
            .archive_item(&item.id)
            .expect("context item is archived");
        assert_eq!(archived.id, item.id);
        assert!(broker.get_item(&item.id).is_none());
    }

    #[test]
    fn create_rejects_empty_body_and_invalid_confidence() {
        let mut broker = ContextBroker::new();
        let project_id = ProjectId::new();

        let empty_body = broker.create_item(new_item(
            project_id.clone(),
            None,
            ContextScope::Project,
            ContextKind::CodingRule,
            "Rule",
            " ",
        ));
        assert!(empty_body.is_err());

        let mut invalid_confidence = new_item(
            project_id,
            None,
            ContextScope::Project,
            ContextKind::CodingRule,
            "Rule",
            "Keep public APIs stable.",
        );
        invalid_confidence.confidence = 1.5;
        assert!(broker.create_item(invalid_confidence).is_err());
    }

    #[test]
    fn select_pack_prioritizes_attached_and_task_scoped_items() {
        let mut broker = ContextBroker::new();
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let other_task_id = TaskId::new();

        let project_rule = broker
            .create_item(new_item(
                project_id.clone(),
                None,
                ContextScope::Project,
                ContextKind::CodingRule,
                "Coding rule",
                "Do not break public APIs.",
            ))
            .unwrap();
        let task_brief = broker
            .create_item(new_item(
                project_id.clone(),
                Some(task_id.clone()),
                ContextScope::Task,
                ContextKind::TaskBrief,
                "Task brief",
                "Implement context pack selection.",
            ))
            .unwrap();
        let other_task = broker
            .create_item(new_item(
                project_id.clone(),
                Some(other_task_id),
                ContextScope::Task,
                ContextKind::Risk,
                "Other task risk",
                "Do not include this automatically.",
            ))
            .unwrap();

        let pack = broker
            .select_pack(ContextPackRequest {
                project_id,
                task_id: Some(task_id),
                attached_context_ids: vec![other_task.id.clone()],
                max_inline_chars: 500,
            })
            .expect("pack is selected");

        let ids: Vec<_> = pack
            .inline_items
            .iter()
            .map(|item| item.id.clone())
            .collect();
        assert_eq!(ids[0], other_task.id);
        assert!(ids.contains(&task_brief.id));
        assert!(ids.contains(&project_rule.id));
    }

    #[test]
    fn select_pack_omits_items_that_exceed_inline_limit() {
        let mut broker = ContextBroker::new();
        let project_id = ProjectId::new();
        let artifact_id = ArtifactId::new();

        let short = broker
            .create_item({
                let mut item = new_item(
                    project_id.clone(),
                    None,
                    ContextScope::Project,
                    ContextKind::Decision,
                    "Decision",
                    "Use mailbox files for long logs.",
                );
                item.artifact_refs = vec![artifact_id.clone()];
                item
            })
            .unwrap();
        let long = broker
            .create_item(new_item(
                project_id.clone(),
                None,
                ContextScope::Project,
                ContextKind::TestResult,
                "Long test result",
                "x".repeat(200).as_str(),
            ))
            .unwrap();

        let pack = broker
            .select_pack(ContextPackRequest {
                project_id,
                task_id: None,
                attached_context_ids: Vec::new(),
                max_inline_chars: 80,
            })
            .expect("pack is selected");

        assert_eq!(pack.inline_items, vec![short]);
        assert_eq!(pack.artifact_refs, vec![artifact_id]);
        assert_eq!(pack.omitted_items, vec![long.id]);
        assert!(pack.mailbox_files.is_empty());
    }

    #[test]
    fn restricted_context_requires_explicit_attachment() {
        let mut broker = ContextBroker::new();
        let project_id = ProjectId::new();
        let mut restricted = new_item(
            project_id.clone(),
            None,
            ContextScope::Project,
            ContextKind::Risk,
            "Sensitive risk",
            "Contains private investigation notes.",
        );
        restricted.visibility = Visibility::Restricted;
        let item = broker.create_item(restricted).unwrap();

        let without_attachment = broker
            .select_pack(ContextPackRequest {
                project_id: project_id.clone(),
                task_id: None,
                attached_context_ids: Vec::new(),
                max_inline_chars: 500,
            })
            .unwrap();
        assert!(without_attachment.inline_items.is_empty());

        let with_attachment = broker
            .select_pack(ContextPackRequest {
                project_id,
                task_id: None,
                attached_context_ids: vec![item.id.clone()],
                max_inline_chars: 500,
            })
            .unwrap();
        assert_eq!(with_attachment.inline_items[0].id, item.id);
    }

    #[test]
    fn select_pack_with_mailbox_writes_long_items_and_returns_prompt_paths() {
        let mut broker = ContextBroker::new();
        let project_id = ProjectId::new();
        let artifact_id = ArtifactId::new();
        let project_root = temp_project_root();

        let short = broker
            .create_item(new_item(
                project_id.clone(),
                None,
                ContextScope::Project,
                ContextKind::Decision,
                "Decision",
                "Keep the parser strict.",
            ))
            .unwrap();
        let long = broker
            .create_item({
                let mut item = new_item(
                    project_id.clone(),
                    None,
                    ContextScope::Project,
                    ContextKind::TestResult,
                    "Failing test log",
                    "line 1\napi_key = should-not-leak\nline 3 with sk-secret-token",
                );
                item.artifact_refs = vec![artifact_id.clone()];
                item
            })
            .unwrap();

        let pack = broker
            .select_pack_with_mailbox(
                ContextPackRequest {
                    project_id,
                    task_id: None,
                    attached_context_ids: Vec::new(),
                    max_inline_chars: 60,
                },
                MailboxConfig {
                    project_root: project_root.clone(),
                    agent_name: "impl-codex".to_string(),
                },
            )
            .expect("pack writes mailbox files");

        assert_eq!(pack.inline_items, vec![short]);
        assert_eq!(pack.mailbox_files.len(), 1);
        assert_eq!(pack.artifact_refs, vec![artifact_id]);
        assert!(pack.omitted_items.is_empty());
        assert!(pack.mailbox_files[0].starts_with(".agentmux/inbox/impl-codex"));

        let contents = fs::read_to_string(project_root.join(&pack.mailbox_files[0]))
            .expect("mailbox file is readable");
        assert!(contents.contains(&format!("context_id: {}", long.id)));
        assert!(contents.contains("kind: TestResult"));
        assert!(contents.contains("redacted: true"));
        assert!(contents.contains("# TestResult: Failing test log"));
        assert!(contents.contains("api_key = [REDACTED]"));
        assert!(!contents.contains("should-not-leak"));
        assert!(!contents.contains("sk-secret-token"));
    }

    #[test]
    fn select_pack_with_mailbox_rejects_unsafe_agent_path_segment() {
        let broker = ContextBroker::new();
        let result = broker.select_pack_with_mailbox(
            ContextPackRequest {
                project_id: ProjectId::new(),
                task_id: None,
                attached_context_ids: Vec::new(),
                max_inline_chars: 80,
            },
            MailboxConfig {
                project_root: temp_project_root(),
                agent_name: "../impl".to_string(),
            },
        );

        assert!(result.is_err());
    }

    fn temp_project_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("agentmux-context-test-{nanos}"))
    }
}
