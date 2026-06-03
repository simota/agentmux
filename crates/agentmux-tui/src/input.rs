//! Pure input forwarding helpers for focused agent panes.
//!
//! The run loop owns the actual socket write. This module only turns keymap
//! output plus current focus state into an IPC request that the daemon already
//! knows how to inject into the target PTY.

use agentmux_agent::adapter::{InputPrecondition, InputSafety};
use agentmux_agent::{InputAction, InputScript};
use agentmux_core::{AgentSessionId, DateTimeUtc, InputScriptId};
use agentmux_ipc::protocol::{ClientRequest, IpcCommand};

use crate::keymap::KeyDispatch;
use crate::state::TuiSessionState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputForwardError {
    NoFocusedPane,
    InvalidFocusedAgentId(String),
    EmptyInput,
}

pub fn dispatch_to_daemon_request(
    state: &TuiSessionState,
    request_id: impl Into<String>,
    dispatch: KeyDispatch,
) -> Result<Option<ClientRequest>, InputForwardError> {
    match dispatch {
        KeyDispatch::ForwardToFocusedPane(bytes) => {
            focused_input_request(state, request_id, bytes).map(Some)
        }
        KeyDispatch::Command(_) | KeyDispatch::PrefixStarted | KeyDispatch::Consumed => Ok(None),
    }
}

pub fn focused_input_request(
    state: &TuiSessionState,
    request_id: impl Into<String>,
    bytes: Vec<u8>,
) -> Result<ClientRequest, InputForwardError> {
    if bytes.is_empty() {
        return Err(InputForwardError::EmptyInput);
    }

    let agent_id = state
        .focused_pane()
        .ok_or(InputForwardError::NoFocusedPane)?
        .agent_id()
        .to_owned();
    let target_agent_id = agent_id
        .parse::<AgentSessionId>()
        .map_err(|_| InputForwardError::InvalidFocusedAgentId(agent_id))?;
    let script = InputScript {
        id: InputScriptId::new(),
        target_agent_id,
        reason: "human key input".to_string(),
        preconditions: vec![InputPrecondition::InputLockAvailable],
        actions: vec![InputAction::SendRaw(bytes)],
        safety: InputSafety::Safe,
        created_at: DateTimeUtc::now_utc(),
    };

    Ok(ClientRequest::new(
        request_id,
        IpcCommand::AgentSendInputScript,
        serde_json::to_value(script).expect("InputScript serialization is infallible"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_agent::InputScript;
    use agentmux_ipc::protocol::IpcCommand;
    use serde_json::json;

    use crate::keymap::{KeymapDispatcher, TuiCommand};

    fn state_with_focus(agent_id: &str) -> TuiSessionState {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [{ "id": agent_id, "name": "impl" }]
        }));
        state
    }

    #[test]
    fn regular_key_dispatch_builds_focused_input_script_request() {
        let state = state_with_focus("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B");
        let mut dispatcher = KeymapDispatcher::default();
        let dispatch = dispatcher.dispatch(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));

        let request = dispatch_to_daemon_request(&state, "req_input_1", dispatch)
            .expect("input forwarding succeeds")
            .expect("forward request");

        assert_eq!(request.id, "req_input_1");
        assert_eq!(request.command, IpcCommand::AgentSendInputScript);
        let script: InputScript =
            serde_json::from_value(request.payload).expect("payload matches daemon input script");
        assert_eq!(
            script.target_agent_id.to_string(),
            state.layout().focused().unwrap()
        );
        assert_eq!(script.reason, "human key input");
        assert_eq!(script.actions.len(), 1);
        assert_eq!(
            serde_json::to_value(&script.actions[0]).unwrap(),
            json!({"send_raw": [97]})
        );
    }

    #[test]
    fn ctrl_c_dispatch_targets_only_focused_agent() {
        let mut state = TuiSessionState::default();
        state.apply_daemon_status(&json!({
            "agents": [
                { "id": "agent_01KBKX3F4SPGZ1A0JMQJEFAV7B", "name": "first" },
                { "id": "agent_01KBKX3F4TSS3HYK1A7S98D670", "name": "second" }
            ]
        }));
        state.focus_next();

        let request = focused_input_request(&state, "req_interrupt", b"\x03".to_vec())
            .expect("ctrl-c request");
        let script: InputScript =
            serde_json::from_value(request.payload).expect("payload matches daemon input script");

        assert_eq!(
            script.target_agent_id.to_string(),
            "agent_01KBKX3F4TSS3HYK1A7S98D670"
        );
        assert_eq!(
            serde_json::to_value(&script.actions[0]).unwrap(),
            json!({"send_raw": [3]})
        );
    }

    #[test]
    fn prefix_command_is_not_forwarded_to_daemon() {
        let state = state_with_focus("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B");

        let request = dispatch_to_daemon_request(
            &state,
            "req_ignored",
            KeyDispatch::Command(TuiCommand::Detach),
        )
        .expect("command dispatch is handled");

        assert!(request.is_none());
    }

    #[test]
    fn missing_focus_and_empty_bytes_are_rejected() {
        let state = TuiSessionState::default();

        assert_eq!(
            focused_input_request(&state, "req_input", b"a".to_vec()),
            Err(InputForwardError::NoFocusedPane)
        );

        let state = state_with_focus("agent_01KBKX3F4SPGZ1A0JMQJEFAV7B");
        assert_eq!(
            focused_input_request(&state, "req_input", Vec::new()),
            Err(InputForwardError::EmptyInput)
        );
    }
}
