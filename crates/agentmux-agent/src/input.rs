//! InputScript encoding and PTY delivery helpers.

use std::time::Duration;

use agentmux_core::{AgentStatus, AgentmuxError, DateTimeUtc, error::Result};
use agentmux_pty::{PtyHandle, bracketed_paste_bytes};

use crate::adapter::{InputAction, InputPrecondition, InputScript};
use crate::session::{InputActivity, InputLock, InputLockError, InputOwner};

/// A delivery step produced from one `InputAction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodedInputStep {
    Bytes(Vec<u8>),
    Wait(Duration),
}

/// Snapshot used to check whether automated input may be injected now.
#[derive(Debug, Clone)]
pub struct InputPreconditionState<'a> {
    pub status: &'a AgentStatus,
    pub lock: &'a InputLock,
    pub activity: &'a InputActivity,
    pub now: DateTimeUtc,
}

/// Check all preconditions declared on an `InputScript`.
pub fn check_input_preconditions(
    script: &InputScript,
    state: &InputPreconditionState<'_>,
) -> Result<()> {
    for precondition in &script.preconditions {
        match precondition {
            InputPrecondition::AgentIdle => {
                if !matches!(
                    state.status,
                    AgentStatus::AwaitingInput
                        | AgentStatus::InteractiveReady
                        | AgentStatus::CompletedTurn
                ) {
                    return Err(AgentmuxError::UserError(format!(
                        "input precondition failed for {}: agent is not idle",
                        script.id
                    )));
                }
            }
            InputPrecondition::InputLockAvailable => {
                if !state.lock.is_available_at(state.now) {
                    return Err(AgentmuxError::UserError(format!(
                        "input precondition failed for {}: input lock is held",
                        script.id
                    )));
                }
            }
            InputPrecondition::QuietFor(duration) => {
                if !state.activity.quiet_for_at(state.now, *duration) {
                    return Err(AgentmuxError::UserError(format!(
                        "input precondition failed for {}: recent human activity",
                        script.id
                    )));
                }
            }
        }
    }

    Ok(())
}

/// Convert one input action into terminal bytes or a wait step.
pub fn encode_input_action(action: &InputAction) -> Result<EncodedInputStep> {
    let step = match action {
        InputAction::TypeText(text) => {
            EncodedInputStep::Bytes(sanitize_text_input(text).into_bytes())
        }
        InputAction::PasteText(text) => {
            EncodedInputStep::Bytes(bracketed_paste_bytes(&sanitize_text_input(text)))
        }
        InputAction::PressEnter => EncodedInputStep::Bytes(b"\r".to_vec()),
        InputAction::PressEsc => EncodedInputStep::Bytes(b"\x1b".to_vec()),
        InputAction::PressTab => EncodedInputStep::Bytes(b"\t".to_vec()),
        InputAction::PressBackspace => EncodedInputStep::Bytes(b"\x7f".to_vec()),
        InputAction::PressCtrl(ch) => EncodedInputStep::Bytes(vec![ctrl_byte(*ch)?]),
        InputAction::PressAlt(ch) => EncodedInputStep::Bytes(alt_bytes(*ch)?),
        InputAction::SendRaw(bytes) => EncodedInputStep::Bytes(bytes.clone()),
        InputAction::Wait(duration) => EncodedInputStep::Wait(*duration),
    };

    Ok(step)
}

/// Send an input script to a PTY handle, preserving action order.
pub async fn write_input_script_to_pty(handle: &mut PtyHandle, script: &InputScript) -> Result<()> {
    for action in &script.actions {
        match encode_input_action(action)? {
            EncodedInputStep::Bytes(bytes) => handle.write_bytes(&bytes)?,
            EncodedInputStep::Wait(duration) => tokio::time::sleep(duration).await,
        }
    }

    Ok(())
}

/// Runtime state required before automated input may be written.
pub struct ReadyInputWrite<'a> {
    pub lock: &'a mut InputLock,
    pub status: &'a AgentStatus,
    pub activity: &'a InputActivity,
    pub now: DateTimeUtc,
    pub owner: InputOwner,
    pub lock_ttl: Duration,
}

/// Check preconditions, acquire the input lock, send the script, then release it.
pub async fn write_input_script_to_pty_when_ready(
    handle: &mut PtyHandle,
    script: &InputScript,
    ready: ReadyInputWrite<'_>,
) -> Result<()> {
    check_input_preconditions(
        script,
        &InputPreconditionState {
            status: ready.status,
            lock: ready.lock,
            activity: ready.activity,
            now: ready.now,
        },
    )?;
    ready
        .lock
        .acquire(ready.owner.clone(), ready.now, ready.lock_ttl)
        .map_err(input_lock_error)?;

    let write_result = write_input_script_to_pty(handle, script).await;
    let release_result = ready.lock.release(&ready.owner).map_err(input_lock_error);

    write_result.and(release_result)
}

fn input_lock_error(error: InputLockError) -> AgentmuxError {
    match error {
        InputLockError::AlreadyHeld => AgentmuxError::UserError("input lock is held".to_string()),
        InputLockError::OwnedByAnother => {
            AgentmuxError::Internal("input lock owner changed during injection".to_string())
        }
    }
}

fn sanitize_text_input(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(*ch, '\n' | '\r' | '\t'))
        .collect()
}

fn ctrl_byte(ch: char) -> Result<u8> {
    let byte = match ch {
        'a'..='z' => ch as u8 - b'a' + 1,
        'A'..='Z' => ch as u8 - b'A' + 1,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => {
            return Err(AgentmuxError::UserError(format!(
                "unsupported Ctrl key: {ch:?}"
            )));
        }
    };

    Ok(byte)
}

fn alt_bytes(ch: char) -> Result<Vec<u8>> {
    if ch.is_control() {
        return Err(AgentmuxError::UserError(format!(
            "unsupported Alt key: {ch:?}"
        )));
    }

    let mut bytes = Vec::with_capacity(1 + ch.len_utf8());
    bytes.push(0x1b);
    let mut buffer = [0_u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use agentmux_core::{AgentSessionId, InputScriptId};
    use agentmux_pty::{PtyHandle, PtySpawnSpec, TerminalSize};

    use super::*;
    use crate::adapter::InputSafety;
    use crate::session::{InputActivity, InputLock, InputOwner};

    fn script(actions: Vec<InputAction>) -> InputScript {
        InputScript {
            id: InputScriptId::new(),
            target_agent_id: AgentSessionId::new(),
            reason: "unit test".to_string(),
            preconditions: vec![InputPrecondition::InputLockAvailable],
            actions,
            safety: InputSafety::Safe,
            created_at: agentmux_core::DateTimeUtc::UNIX_EPOCH,
        }
    }

    fn script_with_preconditions(preconditions: Vec<InputPrecondition>) -> InputScript {
        InputScript {
            id: InputScriptId::new(),
            target_agent_id: AgentSessionId::new(),
            reason: "unit test".to_string(),
            preconditions,
            actions: vec![InputAction::TypeText("ready".to_string())],
            safety: InputSafety::Safe,
            created_at: agentmux_core::DateTimeUtc::UNIX_EPOCH,
        }
    }

    fn precondition_state<'a>(
        status: &'a AgentStatus,
        lock: &'a InputLock,
        activity: &'a InputActivity,
        now: DateTimeUtc,
    ) -> InputPreconditionState<'a> {
        InputPreconditionState {
            status,
            lock,
            activity,
            now,
        }
    }

    fn shell_spec(script: &str) -> PtySpawnSpec {
        let mut env = BTreeMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());

        PtySpawnSpec {
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            cwd: std::env::current_dir().expect("current dir should be available"),
            env,
            size: TerminalSize::default(),
        }
    }

    #[test]
    fn paste_text_uses_bracketed_paste_without_implicit_enter() {
        let step = encode_input_action(&InputAction::PasteText("hello\nworld".to_string()))
            .expect("encode paste");

        assert_eq!(
            step,
            EncodedInputStep::Bytes(b"\x1b[200~hello\nworld\x1b[201~".to_vec())
        );
    }

    #[test]
    fn enter_is_explicit_script_action() {
        let encoded: Vec<EncodedInputStep> = script(vec![
            InputAction::PasteText("hello".to_string()),
            InputAction::PressEnter,
        ])
        .actions
        .iter()
        .map(encode_input_action)
        .collect::<Result<_>>()
        .expect("encode script");

        assert_eq!(
            encoded,
            vec![
                EncodedInputStep::Bytes(b"\x1b[200~hello\x1b[201~".to_vec()),
                EncodedInputStep::Bytes(b"\r".to_vec())
            ]
        );
    }

    #[test]
    fn key_actions_map_to_terminal_bytes() {
        let actions = [
            InputAction::PressCtrl('c'),
            InputAction::PressCtrl('['),
            InputAction::PressAlt('x'),
            InputAction::PressEsc,
            InputAction::PressTab,
            InputAction::PressBackspace,
        ];
        let encoded: Vec<EncodedInputStep> = actions
            .iter()
            .map(encode_input_action)
            .collect::<Result<_>>()
            .expect("encode keys");

        assert_eq!(
            encoded,
            vec![
                EncodedInputStep::Bytes(b"\x03".to_vec()),
                EncodedInputStep::Bytes(b"\x1b".to_vec()),
                EncodedInputStep::Bytes(b"\x1bx".to_vec()),
                EncodedInputStep::Bytes(b"\x1b".to_vec()),
                EncodedInputStep::Bytes(b"\t".to_vec()),
                EncodedInputStep::Bytes(b"\x7f".to_vec())
            ]
        );
    }

    #[test]
    fn text_actions_strip_non_spacing_control_characters() {
        let step = encode_input_action(&InputAction::TypeText("a\x00b\tc\nd".to_string()))
            .expect("encode text");

        assert_eq!(step, EncodedInputStep::Bytes(b"ab\tc\nd".to_vec()));
    }

    #[test]
    fn unsupported_control_keys_are_rejected() {
        let error = encode_input_action(&InputAction::PressCtrl('1'))
            .expect_err("unsupported ctrl key should fail");

        assert!(matches!(error, AgentmuxError::UserError(_)));
    }

    #[test]
    fn preconditions_accept_idle_unlocked_quiet_agent() {
        let lock = InputLock::default();
        let activity = InputActivity::new();
        let now = DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(10);
        let input = script_with_preconditions(vec![
            InputPrecondition::AgentIdle,
            InputPrecondition::InputLockAvailable,
            InputPrecondition::QuietFor(Duration::from_secs(2)),
        ]);

        check_input_preconditions(
            &input,
            &precondition_state(&AgentStatus::AwaitingInput, &lock, &activity, now),
        )
        .expect("preconditions should pass");
    }

    #[test]
    fn preconditions_reject_non_idle_agent() {
        let lock = InputLock::default();
        let activity = InputActivity::new();
        let now = DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(10);
        let input = script_with_preconditions(vec![InputPrecondition::AgentIdle]);

        let error = check_input_preconditions(
            &input,
            &precondition_state(&AgentStatus::RunningTurn, &lock, &activity, now),
        )
        .expect_err("running agent should fail idle precondition");

        assert!(matches!(error, AgentmuxError::UserError(_)));
    }

    #[test]
    fn preconditions_reject_recent_human_activity() {
        let lock = InputLock::default();
        let mut activity = InputActivity::new();
        let now = DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(10);
        activity.record_human_input(now - time::Duration::seconds(1));
        let input =
            script_with_preconditions(vec![InputPrecondition::QuietFor(Duration::from_secs(2))]);

        let error = check_input_preconditions(
            &input,
            &precondition_state(&AgentStatus::AwaitingInput, &lock, &activity, now),
        )
        .expect_err("recent human input should fail quiet precondition");

        assert!(matches!(error, AgentmuxError::UserError(_)));
    }

    #[test]
    fn preconditions_treat_expired_lock_as_available() {
        let mut lock = InputLock::default();
        let activity = InputActivity::new();
        let now = DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(10);
        lock.acquire(
            InputOwner::MessageBus,
            now - time::Duration::seconds(5),
            Duration::from_secs(2),
        )
        .expect("acquire lock");
        let input = script_with_preconditions(vec![InputPrecondition::InputLockAvailable]);

        check_input_preconditions(
            &input,
            &precondition_state(&AgentStatus::AwaitingInput, &lock, &activity, now),
        )
        .expect("expired lock should be available");
    }

    #[tokio::test]
    async fn writes_script_actions_to_pty_in_order() {
        let mut handle = PtyHandle::spawn(shell_spec("cat")).expect("spawn cat");
        let mut reader = handle.try_clone_reader().expect("clone pty reader");
        let input = script(vec![
            InputAction::TypeText("first ".to_string()),
            InputAction::Wait(Duration::from_millis(1)),
            InputAction::PasteText("second".to_string()),
            InputAction::PressEnter,
        ]);

        write_input_script_to_pty(&mut handle, &input)
            .await
            .expect("write script");
        handle.terminate().expect("terminate cat");

        let mut output = String::new();
        std::io::Read::read_to_string(&mut reader, &mut output).expect("read output");
        let _ = handle.wait();

        assert!(output.contains("first "), "output was {output:?}");
        assert!(output.contains("second"), "output was {output:?}");
    }

    #[tokio::test]
    async fn ready_writer_rejects_recent_human_activity_before_writing() {
        let mut handle = PtyHandle::spawn(shell_spec("cat")).expect("spawn cat");
        let mut reader = handle.try_clone_reader().expect("clone pty reader");
        let mut lock = InputLock::default();
        let mut activity = InputActivity::new();
        let now = DateTimeUtc::UNIX_EPOCH + time::Duration::seconds(10);
        activity.record_human_input(now);
        let input =
            script_with_preconditions(vec![InputPrecondition::QuietFor(Duration::from_secs(2))]);
        let status = AgentStatus::AwaitingInput;

        let error = write_input_script_to_pty_when_ready(
            &mut handle,
            &input,
            ReadyInputWrite {
                lock: &mut lock,
                status: &status,
                activity: &activity,
                now,
                owner: InputOwner::Orchestrator,
                lock_ttl: Duration::from_secs(5),
            },
        )
        .await
        .expect_err("recent human input should reject automation");
        handle.terminate().expect("terminate cat");

        let mut output = String::new();
        std::io::Read::read_to_string(&mut reader, &mut output).expect("read output");
        let _ = handle.wait();

        assert!(matches!(error, AgentmuxError::UserError(_)));
        assert!(!output.contains("ready"), "output was {output:?}");
        assert!(lock.is_available_at(now));
    }
}
