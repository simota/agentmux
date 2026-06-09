    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::*;
    use agentmux_core::{
        AgentProvider, AgentRole, AgentSessionId, AgentStatus, ClientId, ContextItemId, DateTimeUtc,
        DeliveryMode, DeliveryStatus, Priority, TaskId,
    };

    use crate::message::{
        AgentMessage, MessageKind, MessageSource, MessageTarget, NewAgentMessage,
    };
    use crate::thread::NewMessageThread;

    fn message_input(to: MessageTarget, delivery_mode: DeliveryMode) -> NewAgentMessage {
        NewAgentMessage {
            task_id: None,
            thread_id: None,
            from: MessageSource::User(ClientId::new()),
            to,
            kind: MessageKind::Handoff,
            priority: Priority::High,
            body: "Please review this patch.".to_string(),
            context_refs: vec![ContextItemId::new()],
            artifact_refs: Vec::new(),
            delivery_mode,
            requires_response: true,
        }
    }

    #[test]
    fn create_message_resolves_role_target_and_places_message_in_inbox() {
        let mut bus = MessageBus::new();
        let implementer = AgentSessionId::new();
        let reviewer = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(
            implementer.clone(),
            AgentRole::Implementer,
        ));
        bus.register_agent(AgentDescriptor::new(reviewer.clone(), AgentRole::Reviewer));

        let message = bus
            .create_message(message_input(
                MessageTarget::Role(AgentRole::Implementer),
                DeliveryMode::InboxOnly,
            ))
            .expect("message is created");

        assert_eq!(message.delivery_status, DeliveryStatus::Queued);
        assert_eq!(bus.inbox(&implementer).unwrap()[0].id, message.id);
        assert!(bus.inbox(&reviewer).unwrap().is_empty());
        assert_eq!(bus.get_message(&message.id).unwrap().body, message.body);
    }

    #[test]
    fn registering_agent_backfills_messages_stored_before_it_existed() {
        let mut bus = MessageBus::new();

        // A message addressed to role:tester is created before any tester
        // session exists — stored with no inbox entry.
        let message = bus
            .create_message_allow_no_recipients(message_input(
                MessageTarget::Role(AgentRole::Tester),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is stored even with no recipient");
        assert_eq!(
            message.delivery_status,
            DeliveryStatus::Queued,
            "an unroutable message starts Queued"
        );

        // No tester inbox yet, so the message lives only in the store.
        assert_eq!(bus.list_messages().len(), 1);

        // The tester registers later …
        let tester = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester));

        // … and the previously-stored message is backfilled into its inbox.
        let inbox = bus.inbox(&tester).expect("tester has an inbox");
        assert_eq!(inbox.len(), 1, "queued message is backfilled on register");
        assert_eq!(inbox[0].id, message.id);

        // Registering again (or a second matching agent) must not duplicate the
        // backfilled message in the same inbox.
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester));
        assert_eq!(
            bus.inbox(&tester).unwrap().len(),
            1,
            "re-registering must not duplicate the backfilled message"
        );

        // A non-matching role (reviewer) must not claim the tester message.
        let reviewer = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(reviewer.clone(), AgentRole::Reviewer));
        assert!(
            bus.inbox(&reviewer).unwrap().is_empty(),
            "a non-matching agent must not receive the backfilled message"
        );
    }

    #[test]
    fn backfill_skips_already_delivered_messages() {
        let mut bus = MessageBus::new();
        let now = DateTimeUtc::now_utc();

        let message = bus
            .create_message_allow_no_recipients(message_input(
                MessageTarget::Role(AgentRole::Tester),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is stored");

        // Mark it Delivered before the tester ever registers (e.g. it was
        // delivered to a different matching session earlier).
        bus.update_delivery_status(&message.id, DeliveryStatus::Delivered, now)
            .expect("status updates");

        let tester = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester));

        assert!(
            bus.inbox(&tester).unwrap().is_empty(),
            "a Delivered message must not be backfilled"
        );
    }

    #[test]
    fn target_resolution_supports_agent_task_team_and_broadcast() {
        let mut bus = MessageBus::new();
        let task_id = TaskId::new();
        let planner = AgentSessionId::new();
        let tester = AgentSessionId::new();
        bus.register_agent(
            AgentDescriptor::new(planner.clone(), AgentRole::Planner)
                .with_name("planner-a1b2c3")
                .with_task_id(task_id.clone())
                .with_team("alpha"),
        );
        bus.register_agent(AgentDescriptor::new(tester.clone(), AgentRole::Tester).with_team("qa"));

        assert_eq!(
            bus.resolve_target(&MessageTarget::Agent(planner.clone()))
                .unwrap(),
            vec![planner.clone()]
        );
        assert_eq!(
            bus.resolve_target(&MessageTarget::AgentName("planner-a1b2c3".to_string()))
                .unwrap(),
            vec![planner.clone()]
        );
        assert_eq!(
            bus.resolve_target(&MessageTarget::Task(task_id)).unwrap(),
            vec![planner.clone()]
        );
        assert_eq!(
            bus.resolve_target(&MessageTarget::Team("qa".to_string()))
                .unwrap(),
            vec![tester.clone()]
        );
        let broadcast: BTreeSet<_> = bus
            .resolve_target(&MessageTarget::Broadcast)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(broadcast, BTreeSet::from([planner, tester]));
    }

    #[test]
    fn require_human_approval_starts_waiting_for_approval() {
        assert_eq!(
            initial_delivery_status(&DeliveryMode::RequireHumanApproval),
            DeliveryStatus::WaitingForApproval
        );

        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Reviewer));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id),
                DeliveryMode::RequireHumanApproval,
            ))
            .expect("message is created");

        assert_eq!(message.delivery_status, DeliveryStatus::WaitingForApproval);
    }

    #[test]
    fn delivery_status_and_read_time_are_mutable_crud_fields() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        bus.update_delivery_status(&message.id, DeliveryStatus::Delivered, now)
            .expect("status is updated");
        bus.mark_read(&message.id, now)
            .expect("message is marked read");

        let updated = bus.get_message(&message.id).unwrap();
        assert_eq!(updated.delivery_status, DeliveryStatus::Delivered);
        assert_eq!(updated.delivered_at, Some(now));
        assert_eq!(updated.read_at, Some(now));
    }

    #[test]
    fn inject_when_idle_prepares_prompt_and_marks_message_injecting() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(
            agent_id.clone(),
            AgentRole::Implementer,
        ));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        let delivery = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("idle message is prepared");

        match delivery {
            IdleDelivery::Ready(prepared) => {
                assert_eq!(prepared.message_id, message.id);
                assert_eq!(prepared.agent_id, agent_id);
                assert!(prepared.prompt.contains("[agentmux handoff]"));
                assert!(
                    prepared
                        .prompt
                        .contains("message:\nPlease review this patch.")
                );
            }
            IdleDelivery::Waiting(wait) => panic!("expected ready delivery, got {wait:?}"),
        }
        assert_eq!(
            bus.get_message(&message.id).unwrap().delivery_status,
            DeliveryStatus::Injecting
        );
    }

    #[test]
    fn inject_when_idle_waits_when_agent_is_busy_or_needs_human() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(
            agent_id.clone(),
            AgentRole::Implementer,
        ));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        let busy = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::RunningTurn,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("busy agent is a wait decision");

        assert_eq!(
            busy,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id: agent_id.clone(),
                reason: DeliveryWaitReason::AgentBusy,
            })
        );
        assert_eq!(
            bus.get_message(&message.id).unwrap().delivery_status,
            DeliveryStatus::WaitingForAgent
        );

        let needs_human = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::Stalled,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("stalled agent is a wait decision");

        assert_eq!(
            needs_human,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id,
                reason: DeliveryWaitReason::AgentNeedsHuman,
            })
        );
    }

    #[test]
    fn injection_result_helpers_manage_delivered_and_failed_status() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        let delivered = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let failed = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect("message is created");
        let now = DateTimeUtc::UNIX_EPOCH;

        bus.mark_message_injected(&delivered.id, &agent_id, now)
            .expect("delivered status is recorded");
        bus.mark_message_injection_failed(&failed.id, now)
            .expect("failed status is recorded");

        let delivered = bus.get_message(&delivered.id).unwrap();
        let failed = bus.get_message(&failed.id).unwrap();
        assert_eq!(delivered.delivery_status, DeliveryStatus::Delivered);
        assert_eq!(delivered.delivered_at, Some(now));
        assert_eq!(failed.delivery_status, DeliveryStatus::Failed);
        assert_eq!(failed.delivered_at, None);
    }

    #[test]
    fn non_inject_when_idle_messages_are_not_prepared_for_idle_injection() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        bus.create_message(message_input(
            MessageTarget::Agent(agent_id.clone()),
            DeliveryMode::InboxOnly,
        ))
        .expect("message is created");

        let delivery = bus
            .prepare_next_inject_when_idle(
                &agent_id,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                DateTimeUtc::UNIX_EPOCH,
            )
            .expect("inbox only message is ignored");

        assert_eq!(
            delivery,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id,
                reason: DeliveryWaitReason::NoInjectWhenIdleMessage,
            })
        );
    }

    #[test]
    fn delete_message_removes_it_from_inboxes() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Tester));
        let message = bus
            .create_message(message_input(
                MessageTarget::Agent(agent_id.clone()),
                DeliveryMode::InboxOnly,
            ))
            .expect("message is created");

        let deleted = bus.delete_message(&message.id).expect("message is deleted");

        assert_eq!(deleted.id, message.id);
        assert!(bus.get_message(&message.id).is_none());
        assert!(bus.inbox(&agent_id).unwrap().is_empty());
    }

    #[test]
    fn prompt_renderer_includes_message_context_paths_and_provider_note() {
        let message = AgentMessage::new(message_input(
            MessageTarget::Role(AgentRole::Implementer),
            DeliveryMode::InjectWhenIdle,
        ));
        let context = PromptContext {
            inline_items: vec![PromptContextItem {
                title: "Decision".to_string(),
                body: "Keep the public API stable.".to_string(),
            }],
            mailbox_paths: vec![PathBuf::from(".agentmux/inbox/impl-codex/msg-00042.md")],
        };

        let prompt = render_prompt(&message, AgentProvider::Codex, &context, None);

        assert!(prompt.contains("[agentmux handoff]"));
        assert!(prompt.contains("kind: Handoff"));
        assert!(prompt.contains("priority: High"));
        assert!(prompt.contains("message:\nPlease review this patch."));
        assert!(prompt.contains("- Decision: Keep the public API stable."));
        assert!(prompt.contains("- .agentmux/inbox/impl-codex/msg-00042.md"));
        assert!(prompt.contains("AGENTMUX_RESULT JSON"));
        assert!(prompt.contains("送信前に人間確認を求めない"));
        assert!(!prompt.contains("内容を確認してください"));
        assert!(prompt.contains("workspace 内の path"));
    }

    fn three_party_bus() -> (MessageBus, AgentSessionId, AgentSessionId, AgentSessionId) {
        let mut bus = MessageBus::new();
        let claude = AgentSessionId::new();
        let codex = AgentSessionId::new();
        let agy = AgentSessionId::new();
        bus.register_agent(
            AgentDescriptor::new(claude.clone(), AgentRole::Implementer).with_name("claude-a"),
        );
        bus.register_agent(
            AgentDescriptor::new(codex.clone(), AgentRole::Reviewer).with_name("codex-b"),
        );
        bus.register_agent(AgentDescriptor::new(agy.clone(), AgentRole::Tester).with_name("agy-c"));
        (bus, claude, codex, agy)
    }

    fn thread_input(
        participants: Vec<AgentSessionId>,
        max_messages_per_participant: Option<u32>,
    ) -> NewMessageThread {
        NewMessageThread {
            topic: "X の設計方針".to_string(),
            participants,
            opened_by: MessageSource::User(ClientId::new()),
            max_messages_per_participant,
        }
    }

    #[test]
    fn thread_message_fans_out_to_all_participants_except_sender() {
        let (mut bus, claude, codex, agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(
                vec![claude.clone(), codex.clone(), agy.clone()],
                None,
            ))
            .expect("thread opens");

        let mut input = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        let message = bus.create_message(input).expect("thread message accepted");

        assert_eq!(message.thread_id, Some(thread.id.clone()));
        assert!(
            bus.inbox(&claude).unwrap().is_empty(),
            "sender must not receive its own thread message"
        );
        assert_eq!(bus.inbox(&codex).unwrap()[0].id, message.id);
        assert_eq!(bus.inbox(&agy).unwrap()[0].id, message.id);
        assert_eq!(bus.thread_message_count(&thread.id), 1);
    }

    #[test]
    fn broadcast_and_role_fan_out_exclude_the_sending_agent() {
        let (mut bus, claude, codex, agy) = three_party_bus();

        let mut input = message_input(MessageTarget::Broadcast, DeliveryMode::InjectWhenIdle);
        input.from = MessageSource::Agent(claude.clone());
        bus.create_message(input).expect("broadcast accepted");

        assert!(bus.inbox(&claude).unwrap().is_empty());
        assert_eq!(bus.inbox(&codex).unwrap().len(), 1);
        assert_eq!(bus.inbox(&agy).unwrap().len(), 1);

        // A role target that resolves only to the sender is an error instead
        // of a silent self-delivery.
        let mut input = message_input(
            MessageTarget::Role(AgentRole::Implementer),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        let error = bus.create_message(input).expect_err("self-only fan-out");
        assert!(error.to_string().contains("other than the sender"));
    }

    #[test]
    fn thread_enforces_participants_limit_and_close() {
        let (mut bus, claude, codex, agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(vec![claude.clone(), codex.clone()], Some(1)))
            .expect("thread opens");

        // Non-participant agents cannot post.
        let mut outsider = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        outsider.from = MessageSource::Agent(agy.clone());
        assert!(
            bus.create_message(outsider)
                .expect_err("outsider rejected")
                .to_string()
                .contains("not a participant")
        );

        // First message is fine; the second hits the per-participant limit.
        let mut first = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        first.from = MessageSource::Agent(claude.clone());
        bus.create_message(first).expect("first message accepted");

        let mut second = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        second.from = MessageSource::Agent(claude.clone());
        let error = bus.create_message(second).expect_err("limit reached");
        assert!(error.to_string().contains("message limit reached"));

        // The user is not turn-limited (moderator can keep steering) …
        bus.create_message(message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        ))
        .expect("user message accepted");

        // … and a closed thread rejects everything.
        bus.close_thread(&thread.id, DateTimeUtc::UNIX_EPOCH)
            .expect("thread closes");
        let error = bus
            .create_message(message_input(
                MessageTarget::Thread(thread.id.clone()),
                DeliveryMode::InjectWhenIdle,
            ))
            .expect_err("closed thread rejects");
        assert!(error.to_string().contains("is closed"));
    }

    #[test]
    fn open_thread_validates_topic_and_participants() {
        let (mut bus, claude, _codex, _agy) = three_party_bus();

        let mut empty_topic = thread_input(vec![claude.clone(), AgentSessionId::new()], None);
        empty_topic.topic = "  ".to_string();
        assert!(bus.open_thread(empty_topic).is_err());

        assert!(
            bus.open_thread(thread_input(vec![claude.clone()], None))
                .expect_err("single participant rejected")
                .to_string()
                .contains("at least 2 participants")
        );

        assert!(
            bus.open_thread(thread_input(vec![claude, AgentSessionId::new()], None))
                .expect_err("unknown participant rejected")
                .to_string()
                .contains("unknown agent session")
        );
    }

    #[test]
    fn fan_out_message_is_injected_once_per_recipient() {
        let (mut bus, claude, codex, agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(
                vec![claude.clone(), codex.clone(), agy.clone()],
                None,
            ))
            .expect("thread opens");

        let mut input = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        let message = bus.create_message(input).expect("thread message accepted");
        let now = DateTimeUtc::UNIX_EPOCH;

        // First recipient injects and the message becomes Delivered …
        let first = bus
            .prepare_next_inject_when_idle(
                &codex,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("first delivery prepared");
        assert!(matches!(first, IdleDelivery::Ready(_)));
        bus.mark_message_injected(&message.id, &codex, now)
            .expect("first injection recorded");

        // … but the second recipient must still receive its own injection.
        let second = bus
            .prepare_next_inject_when_idle(
                &agy,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("second delivery prepared");
        match second {
            IdleDelivery::Ready(prepared) => assert_eq!(prepared.message_id, message.id),
            IdleDelivery::Waiting(wait) => {
                panic!("second recipient must get the fan-out message, got {wait:?}")
            }
        }
        bus.mark_message_injected(&message.id, &agy, now)
            .expect("second injection recorded");

        // Already-served recipients are not re-injected.
        let again = bus
            .prepare_next_inject_when_idle(
                &codex,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                now,
            )
            .expect("no duplicate delivery");
        assert_eq!(
            again,
            IdleDelivery::Waiting(DeliveryWait {
                agent_id: codex.clone(),
                reason: DeliveryWaitReason::NoInjectWhenIdleMessage,
            })
        );
    }

    #[test]
    fn thread_prompt_includes_topic_and_reply_instruction() {
        let (mut bus, claude, codex, _agy) = three_party_bus();
        let thread = bus
            .open_thread(thread_input(vec![claude.clone(), codex.clone()], None))
            .expect("thread opens");

        let mut input = message_input(
            MessageTarget::Thread(thread.id.clone()),
            DeliveryMode::InjectWhenIdle,
        );
        input.from = MessageSource::Agent(claude.clone());
        bus.create_message(input).expect("thread message accepted");

        let delivery = bus
            .prepare_next_inject_when_idle(
                &codex,
                &AgentStatus::AwaitingInput,
                AgentProvider::Codex,
                &PromptContext::empty(),
                DateTimeUtc::UNIX_EPOCH,
            )
            .expect("delivery prepared");

        match delivery {
            IdleDelivery::Ready(prepared) => {
                assert!(prepared.prompt.contains(&format!("thread: {}", thread.id)));
                assert!(prepared.prompt.contains("topic: X の設計方針"));
                assert!(prepared.prompt.contains(&format!("--thread {}", thread.id)));
                assert!(prepared.prompt.contains("発言上限"));
            }
            IdleDelivery::Waiting(wait) => panic!("expected ready delivery, got {wait:?}"),
        }
    }

    #[test]
    fn empty_body_and_unresolved_target_are_rejected() {
        let mut bus = MessageBus::new();
        let mut empty = message_input(MessageTarget::Broadcast, DeliveryMode::InboxOnly);
        empty.body = "  ".to_string();
        assert!(bus.create_message(empty).is_err());

        let unresolved = message_input(
            MessageTarget::Role(AgentRole::Implementer),
            DeliveryMode::InboxOnly,
        );
        assert!(bus.create_message(unresolved).is_err());
    }

    #[test]
    fn set_agent_role_reroutes_role_target_to_new_role() {
        let mut bus = MessageBus::new();
        let agent_id = AgentSessionId::new();
        bus.register_agent(AgentDescriptor::new(agent_id.clone(), AgentRole::Implementer));

        // Before the change, only the implementer role resolves to this session.
        assert_eq!(
            bus.resolve_target(&MessageTarget::Role(AgentRole::Implementer))
                .unwrap(),
            vec![agent_id.clone()]
        );
        assert!(
            bus.resolve_target(&MessageTarget::Role(AgentRole::Reviewer))
                .is_err()
        );

        assert!(bus.set_agent_role(&agent_id, AgentRole::Reviewer));

        // After the change the reviewer role resolves and the old role no longer
        // does.
        assert_eq!(
            bus.resolve_target(&MessageTarget::Role(AgentRole::Reviewer))
                .unwrap(),
            vec![agent_id.clone()]
        );
        assert!(
            bus.resolve_target(&MessageTarget::Role(AgentRole::Implementer))
                .is_err()
        );
    }

    #[test]
    fn set_agent_role_returns_false_for_unknown_agent() {
        let mut bus = MessageBus::new();
        let unknown = AgentSessionId::new();
        assert!(!bus.set_agent_role(&unknown, AgentRole::Reviewer));
    }
