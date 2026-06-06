//! Daemon-status and event application logic for `TuiSessionState`.

use super::*;

impl TuiSessionState {
    /// Seed panes from a `daemon.status` response payload.
    ///
    /// This mirrors the daemon-owned agent list without doing any IPC itself.
    /// Unknown or malformed agent entries are skipped.
    pub fn apply_daemon_status(&mut self, payload: &serde_json::Value) -> usize {
        self.daemon_protocol_version = payload
            .get("protocol_version")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .or(self.daemon_protocol_version);

        let Some(agents) = payload.get("agents").and_then(|value| value.as_array()) else {
            #[cfg(feature = "arena")]
            self.apply_arena_candidates_payload(payload);
            return 0;
        };

        let mut applied = 0;
        for agent in agents {
            let Some(agent_id) =
                string_field(agent, "id").or_else(|| string_field(agent, "agent_id"))
            else {
                continue;
            };
            let name = string_field(agent, "name").unwrap_or_else(|| agent_id.clone());
            let process_id = agent
                .get("process_id")
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok());
            let status = string_field(agent, "status");
            let role = string_field(agent, "role");

            if let Some(pane) = self.panes.get_mut(&agent_id) {
                pane.name = name;
                pane.role = role;
                pane.process_id = process_id;
                pane.status = status;
                pane.last_event = None;
                #[cfg(feature = "activity-feed")]
                {
                    let sitrep_name = pane.name.clone();
                    let sitrep_status = pane.status.clone();
                    let _ = pane;
                    self.upsert_sitrep(agent_id.clone(), sitrep_name, sitrep_status);
                }
            } else {
                let mut pane = AgentPaneState::new(
                    agent_id.clone(),
                    name,
                    process_id,
                    self.default_terminal_size,
                );
                pane.role = role;
                pane.status = status;
                #[cfg(feature = "activity-feed")]
                self.upsert_sitrep(agent_id.clone(), pane.name.clone(), pane.status.clone());
                pane.last_event = None;
                self.layout.add_pane(agent_id.clone());
                self.panes.insert(agent_id.clone(), pane);
            }
            applied += 1;
        }

        self.clamp_session_list_selection();
        #[cfg(feature = "arena")]
        self.apply_arena_candidates_payload(payload);
        applied
    }

    pub fn apply_message_list_payload(&mut self, payload: &Value) -> usize {
        let Some(messages) = payload.get("messages").and_then(Value::as_array) else {
            self.messages.clear();
            return 0;
        };

        self.messages = messages
            .iter()
            .filter_map(MessageListItem::from_payload)
            .collect();
        self.messages.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.message_id.cmp(&left.message_id))
        });
        self.messages.len()
    }

    /// Restore a full pane snapshot returned by `agent.snapshot`.
    pub fn apply_snapshot(&mut self, payload: &serde_json::Value) -> StateChange {
        let Some(agent_id) =
            string_field(payload, "agent_id").or_else(|| string_field(payload, "pane_id"))
        else {
            return StateChange::Ignored;
        };
        let rows = payload
            .get("rows")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(self.default_terminal_size.rows);
        let cols = payload
            .get("cols")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(self.default_terminal_size.cols);
        let name = string_field(payload, "name").unwrap_or_else(|| agent_id.clone());
        let role = string_field(payload, "role");
        let process_id = payload
            .get("process_id")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());

        if !self.panes.contains_key(&agent_id) {
            self.layout.add_pane(agent_id.clone());
            self.panes.insert(
                agent_id.clone(),
                AgentPaneState::new(
                    agent_id.clone(),
                    name.clone(),
                    process_id,
                    TerminalSize { rows, cols },
                ),
            );
        }

        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.name = name;
        pane.role = role;
        pane.process_id = process_id;
        pane.terminal = TerminalParser::new(rows, cols);
        pane.scroll_offset = 0;
        if let Some(lines) = payload.get("lines").and_then(|value| value.as_array()) {
            for (row, line) in lines.iter().enumerate().take(usize::from(rows)) {
                let Some(text) = line.as_str() else {
                    continue;
                };
                let Ok(row) = u16::try_from(row) else {
                    continue;
                };
                let grid = pane.terminal.grid_mut();
                grid.set_cursor(row, 0);
                let mut display_cols = 0_u16;
                for ch in text.chars() {
                    let width = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if width == 0 {
                        continue;
                    }
                    let width = if width > 1 { 2 } else { 1 };
                    if display_cols.saturating_add(width) > cols {
                        break;
                    }
                    grid.write_char(ch, CellStyle::default());
                    display_cols += width;
                }
            }
        }
        pane.last_event = Some(IpcEventKind::TerminalSnapshotSaved);
        StateChange::UpdatedPane(agent_id)
    }

    /// Apply one daemon event. Malformed or unrelated event payloads are ignored.
    pub fn apply_event(&mut self, event: &DaemonEvent) -> StateChange {
        self.last_event = Some(event.kind.clone());
        #[cfg(feature = "activity-feed")]
        self.record_feed_event(event);

        match event.kind {
            IpcEventKind::AgentSpawned => self.apply_agent_spawned(event),
            IpcEventKind::ClientAttached => self.apply_client_attached(event),
            IpcEventKind::AgentStatusChanged | IpcEventKind::AgentStatusSignal => {
                self.apply_agent_status(event)
            }
            IpcEventKind::PtyOutputChunk | IpcEventKind::ScreenDiff => self.apply_output(event),
            IpcEventKind::TerminalSnapshotSaved => self.apply_snapshot(&event.payload),
            IpcEventKind::AgentExited => self.apply_agent_exited(event),
            IpcEventKind::MessageCreated | IpcEventKind::MessageDelivered => {
                self.apply_message_event(event)
            }
            #[cfg(feature = "arena")]
            IpcEventKind::WorktreeCreated
            | IpcEventKind::WorktreeDiffCaptured
            | IpcEventKind::WorktreeTestCompleted
            | IpcEventKind::WorktreeAdoptRequested => self.apply_arena_event(event),
            _ => StateChange::Ignored,
        }
    }

    #[cfg(feature = "arena")]
    fn apply_arena_candidates_payload(&mut self, payload: &Value) {
        let Some(candidates) = payload.get("arena_candidates").and_then(Value::as_array) else {
            return;
        };
        self.arena_candidates = candidates
            .iter()
            .filter_map(ArenaCandidateState::from_payload)
            .collect();
        self.clamp_arena_selection();
    }

    #[cfg(feature = "arena")]
    fn apply_arena_event(&mut self, event: &DaemonEvent) -> StateChange {
        match event.kind {
            IpcEventKind::WorktreeCreated => {
                let Some(candidate) = ArenaCandidateState::from_worktree_created(&event.payload)
                else {
                    return StateChange::Ignored;
                };
                self.upsert_arena_candidate(candidate);
                StateChange::UpdatedMessages
            }
            IpcEventKind::WorktreeDiffCaptured => {
                let Some(worktree_id) = string_field(&event.payload, "worktree_id") else {
                    return StateChange::Ignored;
                };
                let stat = string_field(&event.payload, "stat").unwrap_or_else(|| "-".to_string());
                self.update_arena_candidate(&worktree_id, |candidate| candidate.diff_stat = stat);
                StateChange::UpdatedMessages
            }
            IpcEventKind::WorktreeTestCompleted => {
                let Some(worktree_id) = string_field(&event.payload, "worktree_id") else {
                    return StateChange::Ignored;
                };
                let status =
                    string_field(&event.payload, "status").unwrap_or_else(|| "-".to_string());
                self.update_arena_candidate(&worktree_id, |candidate| {
                    candidate.test_status = status
                });
                StateChange::UpdatedMessages
            }
            IpcEventKind::WorktreeAdoptRequested => {
                let Some(worktree_id) = string_field(&event.payload, "worktree_id") else {
                    return StateChange::Ignored;
                };
                let approval_id =
                    string_field(&event.payload, "approval_id").unwrap_or_else(|| "-".to_string());
                self.update_arena_candidate(&worktree_id, |candidate| {
                    candidate.summary = format!("approval {approval_id}")
                });
                StateChange::UpdatedMessages
            }
            _ => StateChange::Ignored,
        }
    }

    #[cfg(feature = "arena")]
    fn upsert_arena_candidate(&mut self, candidate: ArenaCandidateState) {
        if let Some(existing) = self
            .arena_candidates
            .iter_mut()
            .find(|existing| existing.worktree_id == candidate.worktree_id)
        {
            *existing = candidate;
        } else {
            self.arena_candidates.push(candidate);
        }
        self.clamp_arena_selection();
    }

    #[cfg(feature = "arena")]
    fn update_arena_candidate<F>(&mut self, worktree_id: &str, update: F)
    where
        F: FnOnce(&mut ArenaCandidateState),
    {
        if let Some(candidate) = self
            .arena_candidates
            .iter_mut()
            .find(|candidate| candidate.worktree_id == worktree_id)
        {
            update(candidate);
        }
    }

    fn apply_message_event(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(message) = MessageListItem::from_payload(&event.payload) else {
            return StateChange::Ignored;
        };
        if let Some(existing) = self
            .messages
            .iter_mut()
            .find(|existing| existing.message_id == message.message_id)
        {
            *existing = message;
        } else {
            self.messages.push(message);
        }
        self.messages.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.message_id.cmp(&left.message_id))
        });
        StateChange::UpdatedMessages
    }

    fn apply_agent_spawned(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };
        let name = string_field(&event.payload, "name").unwrap_or_else(|| agent_id.clone());
        let role = string_field(&event.payload, "role");
        let process_id = event
            .payload
            .get("process_id")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok());

        if let Some(pane) = self.panes.get_mut(&agent_id) {
            pane.name = name;
            pane.role = role;
            pane.process_id = process_id;
            pane.last_event = Some(IpcEventKind::AgentSpawned);
            return StateChange::UpdatedPane(agent_id);
        }

        let mut pane = AgentPaneState::new(
            agent_id.clone(),
            name,
            process_id,
            self.default_terminal_size,
        );
        pane.role = role;
        self.layout.add_pane(agent_id.clone());
        self.layout.focus(&agent_id);
        self.panes.insert(agent_id.clone(), pane);
        StateChange::AddedPane(agent_id)
    }

    fn apply_client_attached(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };

        if self.layout.focus(&agent_id) {
            StateChange::FocusedPane(agent_id)
        } else {
            StateChange::Ignored
        }
    }

    fn apply_agent_status(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };
        let Some(status) = string_field(&event.payload, "status")
            .or_else(|| string_field(&event.payload, "new_status"))
            .or_else(|| string_field(&event.payload, "signal"))
        else {
            return StateChange::Ignored;
        };

        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.status = Some(status);
        #[cfg(feature = "activity-feed")]
        {
            let sitrep_name = pane.name.clone();
            let sitrep_status = pane.status.clone();
            let _ = pane;
            self.upsert_sitrep(agent_id.clone(), sitrep_name, sitrep_status);
        }
        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.last_event = Some(event.kind.clone());
        StateChange::UpdatedPane(agent_id)
    }

    fn apply_output(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id")
            .or_else(|| string_field(&event.payload, "pane_id"))
        else {
            return StateChange::Ignored;
        };
        let Some(bytes) = output_bytes(&event.payload) else {
            return StateChange::Ignored;
        };

        let Some(pane) = self.panes.get_mut(&agent_id) else {
            return StateChange::Ignored;
        };
        pane.terminal.advance(&bytes);
        if pane.scroll_offset > 0 {
            let max_offset = pane.terminal.grid().scrollback().len();
            pane.scroll_offset = pane.scroll_offset.min(max_offset);
        }
        pane.last_event = Some(event.kind.clone());
        StateChange::UpdatedPane(agent_id)
    }

    fn apply_agent_exited(&mut self, event: &DaemonEvent) -> StateChange {
        let Some(agent_id) = string_field(&event.payload, "agent_id") else {
            return StateChange::Ignored;
        };

        if self.panes.remove(&agent_id).is_none() {
            return StateChange::Ignored;
        }
        self.layout.remove_pane(&agent_id);
        #[cfg(feature = "activity-feed")]
        self.remove_sitrep(&agent_id);
        self.clamp_session_list_selection();
        StateChange::RemovedPane(agent_id)
    }

    #[cfg(feature = "activity-feed")]
    fn record_feed_event(&mut self, event: &DaemonEvent) {
        let Some(entry) = FeedEntry::from_event(event) else {
            return;
        };
        let was_following_tail = self
            .feed_entries
            .len()
            .checked_sub(1)
            .is_none_or(|tail| self.activity_feed_selected == tail && self.feed_scroll == 0);
        if self.feed_entries.len() == MAX_FEED_ENTRIES {
            self.feed_entries.pop_front();
            self.activity_feed_selected = self.activity_feed_selected.saturating_sub(1);
        }
        self.feed_entries.push_back(entry);
        if was_following_tail {
            self.activity_feed_selected = self.feed_entries.len().saturating_sub(1);
            self.feed_scroll = 0;
        } else {
            self.sync_activity_feed_scroll_to_selection();
        }
    }

    #[cfg(feature = "activity-feed")]
    fn upsert_sitrep(&mut self, agent_id: String, name: String, status: Option<String>) {
        let status = status.unwrap_or_else(|| "-".to_string());
        let needs_attention = needs_attention_status(&status);
        if let Some(entry) = self
            .sitrep
            .iter_mut()
            .find(|entry| entry.agent_id == agent_id)
        {
            entry.name = name;
            entry.status = status;
            entry.needs_attention = needs_attention;
        } else {
            self.sitrep.push(SitrepEntry {
                agent_id,
                name,
                status,
                needs_attention,
            });
        }
        self.sitrep.sort_by(|left, right| {
            right
                .needs_attention
                .cmp(&left.needs_attention)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.agent_id.cmp(&right.agent_id))
        });
    }

    #[cfg(feature = "activity-feed")]
    fn remove_sitrep(&mut self, agent_id: &str) {
        self.sitrep.retain(|entry| entry.agent_id != agent_id);
    }
}
