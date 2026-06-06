use super::*;

    #[test]
    fn message_list_payload_updates_message_bus_state_newest_first() {
        let mut state = TuiSessionState::default();

        let applied = state.apply_message_list_payload(&json!({
            "messages": [
                {
                    "message_id": "msg_old",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "queued",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "old"
                },
                {
                    "message_id": "msg_new",
                    "created_at": "2026-06-04T02:00:00+00:00",
                    "delivery_status": "delivered",
                    "kind": "test_result",
                    "from": { "kind": "orchestrator" },
                    "to": { "kind": "role", "id": "tester" },
                    "body": "new"
                }
            ]
        }));

        assert_eq!(applied, 2);
        assert_eq!(state.messages()[0].message_id, "msg_new");
        assert_eq!(state.messages()[0].from, "orchestrator");
        assert_eq!(state.messages()[0].to, "role:tester");
    }

    #[test]
    fn message_events_upsert_message_bus_state() {
        let mut state = TuiSessionState::default();

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageCreated,
                json!({
                    "message_id": "msg_1",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "queued",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "review this"
                })
            )),
            StateChange::UpdatedMessages
        );
        assert_eq!(state.messages()[0].delivery_status, "queued");

        assert_eq!(
            state.apply_event(&event(
                IpcEventKind::MessageDelivered,
                json!({
                    "message_id": "msg_1",
                    "created_at": "2026-06-04T01:00:00+00:00",
                    "delivery_status": "delivered",
                    "kind": "handoff",
                    "from": { "kind": "agent", "id": "planner" },
                    "to": { "kind": "agent", "id": "impl" },
                    "body": "review this"
                })
            )),
            StateChange::UpdatedMessages
        );
        assert_eq!(state.messages().len(), 1);
        assert_eq!(state.messages()[0].delivery_status, "delivered");
    }
