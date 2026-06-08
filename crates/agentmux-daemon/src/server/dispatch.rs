use crate::*;

pub(crate) async fn handle_request(
    runtime: &DaemonRuntime,
    client_id: &ClientSessionId,
    event_filter: &Arc<Mutex<Option<EventSubscribeFilter>>>,
    request: ClientRequest,
) -> DaemonResponse {
    if let Some(error) = protocol_error(request.protocol_compatibility()) {
        return DaemonResponse::error(request.id, error);
    }

    match request.command {
        IpcCommand::DaemonStatus => DaemonResponse::ok(request.id, runtime.status_payload().await),
        IpcCommand::EventSubscribe => {
            let filter =
                match serde_json::from_value::<EventSubscribeFilter>(request.payload.clone()) {
                    Ok(filter) => filter,
                    Err(error) => {
                        return DaemonResponse::error(
                            request.id,
                            ErrorBody::new(
                                "INVALID_EVENT_SUBSCRIBE_FILTER",
                                format!("event.subscribe filter is invalid: {error}"),
                            ),
                        );
                    }
                };
            match event_filter.lock() {
                Ok(mut current) => {
                    *current = Some(filter);
                    DaemonResponse::ok(request.id, json!({ "subscribed": true }))
                }
                Err(_) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "EVENT_SUBSCRIBE_FAILED",
                        "event.subscribe filter lock is poisoned",
                    ),
                ),
            }
        }
        IpcCommand::TaskRun => {
            let (body, team, project_path, runner) = match task_run_payload(&request.payload) {
                Ok(payload) => payload,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_TASK_RUN", error.to_string()),
                    );
                }
            };
            if runner == "arena" {
                let providers = match arena_providers_payload(&request.payload) {
                    Ok(providers) => providers,
                    Err(error) => {
                        return DaemonResponse::error(
                            request.id,
                            ErrorBody::new("INVALID_ARENA_PROVIDERS", error.to_string()),
                        );
                    }
                };
                let base_branch = request
                    .payload
                    .get("base_branch")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("main")
                    .to_string();
                return match runtime
                    .run_task_with_arena(body, providers, project_path, base_branch)
                    .await
                {
                    Ok(payload) => DaemonResponse::ok(request.id, payload),
                    Err(error) => DaemonResponse::error(
                        request.id,
                        ErrorBody::new("TASK_RUN_FAILED", error.to_string()),
                    ),
                };
            }
            if runner != "shell-stub" {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "TASK_RUNNER_UNAVAILABLE",
                        format!("unsupported task.run runner '{runner}'"),
                    )
                    .with_hint("use runner=shell-stub for deterministic test execution"),
                );
            }

            match runtime
                .run_task_with_shell_stubs(body, team, project_path)
                .await
            {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("TASK_RUN_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSpawn => {
            let name = request
                .payload
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("agent")
                .to_string();
            let role = match request
                .payload
                .get("role")
                .and_then(|value| value.as_str())
                .map(parse_agent_role)
                .transpose()
            {
                Ok(Some(role)) => role,
                Ok(None) => inferred_agent_role(&name),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("AGENT_SPAWN_FAILED", error.to_string()),
                    );
                }
            };
            let agent = match pty_spawn_spec_from_payload(&request.payload) {
                Ok(Some(spec)) => runtime.spawn_agent_with_role(name, role, spec).await,
                Ok(None) => Ok(runtime.register_agent_with_role(name, role).await),
                Err(error) => Err(error),
            };
            match agent {
                Ok(agent) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent.id.to_string(),
                        "name": agent.name,
                        "role": agent_role_label(&agent.role),
                        "process_id": agent.process_id,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_SPAWN_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ClientAttach => {
            let Some(agent_id) = request
                .payload
                .get("agent_id")
                .and_then(|value| value.as_str())
                .and_then(parse_agent_session_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_ATTACH_TARGET", "client.attach requires agent_id"),
                );
            };

            match runtime
                .attach_client(client_id.clone(), agent_id.clone())
                .await
            {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "client_id": client_id.to_string(),
                        "agent_id": agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_NOT_FOUND", error.to_string())
                        .with_hint("call agent.spawn or choose an agent from daemon.status"),
                ),
            }
        }
        IpcCommand::ClientDetach => {
            let agent_id = runtime.detach_client(client_id).await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "client_id": client_id.to_string(),
                    "agent_id": agent_id.map(|id| id.to_string()),
                }),
            )
        }
        IpcCommand::AgentStop => {
            let agent_id = match agent_id_payload(&request.payload, "agent.stop") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.stop_agent(&agent_id).await {
                Ok(agent) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent.id.to_string(),
                        "name": agent.name,
                        "stopped": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_STOP_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentFocus => {
            let agent_id = match agent_id_payload(&request.payload, "agent.focus") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime
                .attach_client(client_id.clone(), agent_id.clone())
                .await
            {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "client_id": client_id.to_string(),
                        "agent_id": agent_id.to_string(),
                        "focused": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_FOCUS_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentInterrupt => {
            let agent_id = match agent_id_payload(&request.payload, "agent.interrupt") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.interrupt_agent(&agent_id).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({ "agent_id": agent_id.to_string(), "interrupted": true }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_INTERRUPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentResize => {
            let agent_id = match agent_id_payload(&request.payload, "agent.resize") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            let size = match terminal_size_payload(&request.payload, "agent.resize") {
                Ok(size) => size,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_TERMINAL_SIZE", error.to_string()),
                    );
                }
            };
            match runtime.resize_agent(&agent_id, size).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "agent_id": agent_id.to_string(),
                        "rows": size.rows,
                        "cols": size.cols,
                        "resized": true,
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_RESIZE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSnapshot => {
            let agent_id = match agent_id_payload(&request.payload, "agent.snapshot") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            match runtime.snapshot_agent(&agent_id).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("AGENT_SNAPSHOT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::AgentSendInputScript => {
            let script = match serde_json::from_value::<InputScript>(request.payload) {
                Ok(script) => script,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new(
                            "INVALID_INPUT_SCRIPT",
                            format!("agent.send_input_script payload is invalid: {error}"),
                        ),
                    );
                }
            };
            match runtime.send_input_script(&script).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "input_script_id": script.id.to_string(),
                        "agent_id": script.target_agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INPUT_SCRIPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::MeetingOpen => {
            let Some(topic) = payload_string_field(&request.payload, "topic")
                .filter(|topic| !topic.trim().is_empty())
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MEETING_TOPIC", "meeting.open requires topic"),
                );
            };
            let participants: Vec<String> = request
                .payload
                .get("participants")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let kind = match request
                .payload
                .get("kind")
                .and_then(|value| value.as_str())
                .map(parse_message_kind)
                .transpose()
            {
                Ok(kind) => kind.unwrap_or(MessageKind::Question),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_MESSAGE_KIND", error.to_string()),
                    );
                }
            };
            let priority = match request
                .payload
                .get("priority")
                .and_then(|value| value.as_str())
                .map(parse_priority)
                .transpose()
            {
                Ok(priority) => priority.unwrap_or(Priority::Normal),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_PRIORITY", error.to_string()),
                    );
                }
            };
            let opened_by = request
                .payload
                .get("from_agent_id")
                .and_then(|value| value.as_str())
                .and_then(|raw| raw.trim().parse::<AgentSessionId>().ok())
                .map(MessageSource::Agent)
                .unwrap_or_else(|| MessageSource::User(ClientId::new()));
            let input = OpenMeetingInput {
                topic,
                participants,
                opened_by,
                max_messages_per_participant: request
                    .payload
                    .get("max_messages_per_participant")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as u32),
                kind,
                priority,
                body: payload_string_field(&request.payload, "body"),
            };
            match runtime.open_meeting(input).await {
                Ok((thread, kickoff)) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "thread": thread_payload(&thread, 1),
                        "kickoff_message": message_payload(&kickoff),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MEETING_OPEN_FAILED", error.to_string())
                        .with_hint("check participants with `agentmux sessions`"),
                ),
            }
        }
        IpcCommand::MeetingClose => {
            let Some(thread_id) = request
                .payload
                .get("thread_id")
                .and_then(|value| value.as_str())
                .and_then(|raw| raw.trim().parse::<ThreadId>().ok())
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_THREAD_ID", "meeting.close requires thread_id"),
                );
            };
            match runtime.close_meeting(&thread_id).await {
                Ok(thread) => {
                    let count = runtime
                        .list_meetings()
                        .await
                        .iter()
                        .find(|(t, _)| t.id == thread.id)
                        .map(|(_, count)| *count)
                        .unwrap_or(0);
                    DaemonResponse::ok(request.id, thread_payload(&thread, count))
                }
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MEETING_CLOSE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::MeetingList => {
            let threads = runtime.list_meetings().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "threads": threads
                        .iter()
                        .map(|(thread, count)| thread_payload(thread, *count))
                        .collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::MessageCreate => {
            let input = match message_create_payload(&request.payload) {
                Ok(input) => input,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_MESSAGE_CREATE", error.to_string()),
                    );
                }
            };
            match runtime.create_message(input).await {
                Ok(message) => {
                    // Deliver to an idle target PTY immediately so `message
                    // send` no longer requires a separate manual `inject`.
                    // Eligibility is delegated to the existing idle-delivery
                    // machinery (delivery_mode + agent status).
                    runtime
                        .trigger_idle_delivery_for_messages(std::slice::from_ref(&message))
                        .await;
                    DaemonResponse::ok(request.id, message_payload(&message))
                }
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MESSAGE_CREATE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::MessageList => {
            let messages = runtime.list_messages().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "messages": messages.iter().map(message_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::MessageShow => {
            let Some(message_id) = request
                .payload
                .get("message_id")
                .and_then(|value| value.as_str())
                .and_then(parse_message_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MESSAGE_ID", "message.show requires message_id"),
                );
            };
            match runtime.get_message(&message_id).await {
                Some(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "MESSAGE_NOT_FOUND",
                        format!("unknown message '{message_id}'"),
                    ),
                ),
            }
        }
        IpcCommand::MessageInject => {
            let Some(message_id) = request
                .payload
                .get("message_id")
                .and_then(|value| value.as_str())
                .and_then(parse_message_id)
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_MESSAGE_ID", "message.inject requires message_id"),
                );
            };
            let agent_id = request
                .payload
                .get("agent_id")
                .and_then(|value| value.as_str())
                .map(str::parse::<AgentSessionId>)
                .transpose();
            let agent_id = match agent_id {
                Ok(agent_id) => agent_id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_AGENT_ID", error.to_string()),
                    );
                }
            };
            let result = match agent_id {
                Some(agent_id) => {
                    runtime
                        .inject_message_to_agent(&message_id, &agent_id)
                        .await
                }
                None => runtime.inject_message(&message_id).await,
            };
            match result {
                Ok(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("MESSAGE_INJECT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextCreate => {
            let input = match context_create_payload(&request.payload, runtime).await {
                Ok(input) => input,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_CREATE", error.to_string()),
                    );
                }
            };
            match runtime.create_context(input).await {
                Ok(item) => DaemonResponse::ok(request.id, context_payload(&item)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_CREATE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextSearch => match context_search_payload(&request.payload) {
            Ok(ContextLookup::List) => {
                let contexts = runtime.list_contexts().await;
                DaemonResponse::ok(
                    request.id,
                    json!({
                        "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
                    }),
                )
            }
            Ok(ContextLookup::Show(context_id)) => match runtime.get_context(&context_id).await {
                Some(item) => DaemonResponse::ok(request.id, context_payload(&item)),
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "CONTEXT_NOT_FOUND",
                        format!("unknown context item '{context_id}'"),
                    ),
                ),
            },
            Ok(ContextLookup::Search(query)) => {
                let contexts = runtime.search_contexts(&query).await;
                DaemonResponse::ok(
                    request.id,
                    json!({
                        "contexts": contexts.iter().map(context_payload).collect::<Vec<_>>(),
                    }),
                )
            }
            Err(error) => DaemonResponse::error(
                request.id,
                ErrorBody::new("INVALID_CONTEXT_SEARCH", error.to_string()),
            ),
        },
        IpcCommand::ContextAttach => {
            let (context_id, message_id) = match context_attach_payload(&request.payload) {
                Ok(ids) => ids,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_ATTACH", error.to_string()),
                    );
                }
            };
            match runtime
                .attach_context_to_message(&context_id, &message_id)
                .await
            {
                Ok(message) => DaemonResponse::ok(request.id, message_payload(&message)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_ATTACH_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextInject => {
            let (context_id, agent_id) = match context_inject_payload(&request.payload) {
                Ok(ids) => ids,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_CONTEXT_INJECT", error.to_string()),
                    );
                }
            };
            match runtime.inject_context(&context_id, &agent_id).await {
                Ok(item) => DaemonResponse::ok(
                    request.id,
                    json!({
                        "context": context_payload(&item),
                        "agent_id": agent_id.to_string(),
                    }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_INJECT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ContextExport => {
            let Some(output) = request
                .payload
                .get("output")
                .and_then(|value| value.as_str())
            else {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new("INVALID_CONTEXT_EXPORT", "context.export requires output"),
                );
            };
            match runtime.export_contexts(Path::new(output)).await {
                Ok(count) => DaemonResponse::ok(
                    request.id,
                    json!({ "output": output, "context_count": count }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("CONTEXT_EXPORT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeList => {
            let worktrees = runtime.list_worktrees().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "worktrees": worktrees.iter().map(worktree_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::WorktreeDiff => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.diff") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.capture_worktree_diff(&worktree_id).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_DIFF_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeTest => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.test") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            let command = worktree_test_command_payload(&request.payload);
            // #14: gate the test command (passed to `/bin/sh -c` in
            // agentmux-worktree git.rs) through the policy engine and reject a
            // `Deny` decision before it can reach the shell.
            if runtime.command_is_denied(&command.command).await {
                return DaemonResponse::error(
                    request.id,
                    ErrorBody::new(
                        "WORKTREE_TEST_DENIED",
                        format!("test command denied by policy: {}", command.command),
                    ),
                );
            }
            match runtime.run_worktree_test(&worktree_id, command).await {
                Ok(payload) => DaemonResponse::ok(request.id, payload),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_TEST_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreePromote => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.promote") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.request_worktree_adoption(worktree_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_PROMOTE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeArchive => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.archive") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.archive_worktree(&worktree_id).await {
                Ok(worktree) => DaemonResponse::ok(request.id, worktree_payload(&worktree)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_ARCHIVE_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::WorktreeAdopt => {
            let worktree_id = match worktree_id_payload(&request.payload, "worktree.adopt") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_WORKTREE_ID", error.to_string()),
                    );
                }
            };
            match runtime.request_worktree_adoption(worktree_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("WORKTREE_ADOPT_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::ApprovalList => {
            let approvals = runtime.list_approvals().await;
            DaemonResponse::ok(
                request.id,
                json!({
                    "approvals": approvals.iter().map(approval_payload).collect::<Vec<_>>(),
                }),
            )
        }
        IpcCommand::ApprovalApprove => {
            let approval_id = match approval_id_payload(&request.payload, "approval.approve") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_APPROVAL_ID", error.to_string()),
                    );
                }
            };
            match runtime.approve_approval(&approval_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("APPROVAL_DECISION_FAILED", approval_queue_error(error)),
                ),
            }
        }
        IpcCommand::ApprovalReject => {
            let approval_id = match approval_id_payload(&request.payload, "approval.reject") {
                Ok(id) => id,
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_APPROVAL_ID", error.to_string()),
                    );
                }
            };
            match runtime.reject_approval(&approval_id).await {
                Ok(approval) => DaemonResponse::ok(request.id, approval_payload(&approval)),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("APPROVAL_DECISION_FAILED", approval_queue_error(error)),
                ),
            }
        }
        IpcCommand::LayoutSet => {
            let name = match required_string(&request.payload, "name", "layout.set") {
                Ok(name) => name.to_string(),
                Err(error) => {
                    return DaemonResponse::error(
                        request.id,
                        ErrorBody::new("INVALID_LAYOUT_SET", error.to_string()),
                    );
                }
            };
            let layout = request
                .payload
                .get("layout")
                .cloned()
                .unwrap_or_else(|| json!({ "name": name }));
            match runtime.save_layout(name.clone(), layout.clone()).await {
                Ok(()) => DaemonResponse::ok(
                    request.id,
                    json!({ "name": name, "layout": layout, "saved": true }),
                ),
                Err(error) => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("LAYOUT_SET_FAILED", error.to_string()),
                ),
            }
        }
        IpcCommand::LayoutGet => {
            let Some(name) = request.payload.get("name").and_then(|value| value.as_str()) else {
                let layouts = runtime.list_layouts().await;
                return DaemonResponse::ok(request.id, json!({ "layouts": layouts }));
            };
            match runtime.get_layout(name).await {
                Some(layout) => {
                    DaemonResponse::ok(request.id, json!({ "name": name, "layout": layout }))
                }
                None => DaemonResponse::error(
                    request.id,
                    ErrorBody::new("LAYOUT_NOT_FOUND", format!("unknown layout '{name}'")),
                ),
            }
        }
        _ => DaemonResponse::error(
            request.id,
            ErrorBody::new(
                "COMMAND_NOT_IMPLEMENTED",
                "command is not implemented by the Phase 2 daemon listener",
            ),
        ),
    }
}
