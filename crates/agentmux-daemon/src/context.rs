use crate::*;

impl DaemonRuntime {
    pub async fn create_context(&self, input: NewContextItem) -> Result<ContextItem> {
        let mut state = self.state.write().await;
        let item = state.contexts.create_item(input)?;
        drop(state);

        self.append_context_created_event(&item)?;
        self.publish(DaemonEvent::new(
            IpcEventKind::ContextCreated,
            context_payload(&item),
        ));
        Ok(item)
    }

    pub async fn list_contexts(&self) -> Vec<ContextItem> {
        let state = self.state.read().await;
        state
            .contexts
            .list_items(&state.default_project_id)
            .into_iter()
            .cloned()
            .collect()
    }

    pub async fn get_context(&self, id: &ContextItemId) -> Option<ContextItem> {
        let state = self.state.read().await;
        state.contexts.get_item(id).cloned()
    }

    pub async fn search_contexts(&self, query: &str) -> Vec<ContextItem> {
        let query = query.to_ascii_lowercase();
        self.list_contexts()
            .await
            .into_iter()
            .filter(|item| {
                item.title.to_ascii_lowercase().contains(&query)
                    || item.body.to_ascii_lowercase().contains(&query)
                    || item
                        .tags
                        .iter()
                        .any(|tag| tag.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    pub async fn attach_context_to_message(
        &self,
        context_id: &ContextItemId,
        message_id: &MessageId,
    ) -> Result<AgentMessage> {
        let mut state = self.state.write().await;
        if state.contexts.get_item(context_id).is_none() {
            return Err(AgentmuxError::UserError(format!(
                "unknown context item '{context_id}'"
            )));
        }
        state
            .messages
            .attach_context_ref(message_id, context_id.clone())
    }

    pub async fn inject_context(
        &self,
        context_id: &ContextItemId,
        agent_id: &AgentSessionId,
    ) -> Result<ContextItem> {
        let state = self.state.read().await;
        if !state.agents.contains_key(agent_id) {
            return Err(AgentmuxError::UserError(format!(
                "unknown agent session '{agent_id}'"
            )));
        }
        let item = state
            .contexts
            .get_item(context_id)
            .cloned()
            .ok_or_else(|| {
                AgentmuxError::UserError(format!("unknown context item '{context_id}'"))
            })?;
        drop(state);

        self.publish(DaemonEvent::new(
            IpcEventKind::ContextInjected,
            json!({
                "context_id": context_id.to_string(),
                "agent_id": agent_id.to_string(),
            }),
        ));
        Ok(item)
    }

    pub async fn export_contexts(&self, output: &Path) -> Result<usize> {
        let contexts = self.list_contexts().await;
        let payload = json!({
            "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
        });
        let bytes = serde_json::to_vec_pretty(&payload).map_err(json_error)?;
        std::fs::write(output, bytes).map_err(|error| {
            AgentmuxError::StoreError(format!(
                "failed to export contexts to '{}': {error}",
                output.display()
            ))
        })?;
        Ok(contexts.len())
    }

}

pub(crate) fn parse_context_item_id(value: &str) -> Option<ContextItemId> {
    value.parse::<ContextItemId>().ok()
}

pub(crate) async fn context_create_payload(
    payload: &serde_json::Value,
    runtime: &DaemonRuntime,
) -> Result<NewContextItem> {
    let project_id = {
        let state = runtime.state.read().await;
        state.default_project_id.clone()
    };
    let title = required_string(payload, "title", "context.create")?.to_string();
    let body = payload
        .get("body")
        .and_then(|value| value.as_str())
        .unwrap_or(&title)
        .to_string();
    let kind = payload
        .get("kind")
        .and_then(|value| value.as_str())
        .map(parse_context_kind)
        .transpose()?
        .unwrap_or(ContextKind::HandoffSummary);
    let visibility = payload
        .get("visibility")
        .and_then(|value| value.as_str())
        .map(parse_visibility)
        .transpose()?
        .unwrap_or(Visibility::Internal);
    let tags = payload
        .get("tags")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| AgentmuxError::UserError(format!("context.create tags invalid: {error}")))?
        .unwrap_or_default();

    Ok(NewContextItem {
        project_id,
        task_id: None,
        scope: ContextScope::Project,
        kind,
        title,
        body,
        source: ContextSource::Human,
        visibility,
        confidence: 1.0,
        tags,
        related_files: Vec::new(),
        artifact_refs: Vec::new(),
    })
}

pub(crate) fn context_search_payload(payload: &serde_json::Value) -> Result<ContextLookup> {
    if let Some(raw_context_id) = payload.get("context_id").and_then(|value| value.as_str()) {
        let context_id = parse_context_item_id(raw_context_id).ok_or_else(|| {
            AgentmuxError::UserError(format!("invalid context_id '{raw_context_id}'"))
        })?;
        return Ok(ContextLookup::Show(context_id));
    }
    let query = payload
        .get("query")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    if query.is_empty() {
        Ok(ContextLookup::List)
    } else {
        Ok(ContextLookup::Search(query))
    }
}

pub(crate) fn context_attach_payload(payload: &serde_json::Value) -> Result<(ContextItemId, MessageId)> {
    let context_id = required_string(payload, "context_id", "context.attach")?
        .parse::<ContextItemId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid context_id: {error}")))?;
    let message_id = required_string(payload, "message_id", "context.attach")?
        .parse::<MessageId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid message_id: {error}")))?;
    Ok((context_id, message_id))
}

pub(crate) fn context_inject_payload(payload: &serde_json::Value) -> Result<(ContextItemId, AgentSessionId)> {
    let context_id = required_string(payload, "context_id", "context.inject")?
        .parse::<ContextItemId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid context_id: {error}")))?;
    let agent_id = required_string(payload, "agent_id", "context.inject")?
        .parse::<AgentSessionId>()
        .map_err(|error| AgentmuxError::UserError(format!("invalid agent_id: {error}")))?;
    Ok((context_id, agent_id))
}

pub(crate) fn parse_context_kind(raw: &str) -> Result<ContextKind> {
    serde_json::from_value(json!(raw))
        .map_err(|error| AgentmuxError::UserError(format!("invalid context kind '{raw}': {error}")))
}

pub(crate) fn context_payload(item: &ContextItem) -> serde_json::Value {
    json!({
        "context_id": item.id.to_string(),
        "project_id": item.project_id.to_string(),
        "task_id": item.task_id.as_ref().map(ToString::to_string),
        "scope": item.scope,
        "kind": item.kind,
        "title": item.title,
        "body": item.body,
        "source": item.source,
        "visibility": item.visibility,
        "confidence": item.confidence,
        "tags": item.tags,
        "related_files": item
            .related_files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "artifact_refs": item.artifact_refs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "created_at": item.created_at.to_string(),
        "updated_at": item.updated_at.to_string(),
    })
}

