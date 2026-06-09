use super::*;
use std::collections::BTreeMap;
use std::time::Duration;

use agentmux_agent::adapter::{InputPrecondition, InputSafety};
use agentmux_agent::{
    AgentResultStatus, InputAction, OutgoingMessage, OutgoingMessageKind, OutgoingPriority,
    ResultRecommendation, ResultRisk,
};
use agentmux_core::InputScriptId;
use agentmux_ipc::{IpcCommand, JsonlReader, JsonlWriter};
use agentmux_store::EventLog;

impl DaemonRuntime {
    /// Test helper: persist a live result with a fresh dedup ring and map the
    /// outcome to `bool` (true == a new result was persisted). Mirrors the
    /// pre-dedup `Ok(bool)` contract for the single-call test cases.
    async fn persist_live_agent_result_once(
        &self,
        agent_id: Option<&AgentSessionId>,
        agent_name: &str,
        output_tail: &str,
    ) -> Result<bool> {
        let mut seen = SeenResultHashes::new(8);
        let outcome = self
            .persist_live_agent_result(agent_id, agent_name, output_tail, &mut seen)
            .await?;
        Ok(matches!(outcome, LiveResultOutcome::Persisted))
    }
}

mod ipc;
mod messages;
mod results;
mod spawn;
mod worktree;

async fn read_response_and_event<R>(reader: &mut JsonlReader<R>) -> (DaemonResponse, DaemonEvent)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut response = None;
    let mut event = None;
    for _ in 0..2 {
        let frame: serde_json::Value = reader.read().await.unwrap().unwrap();
        if frame.get("ok").is_some() {
            response = Some(serde_json::from_value(frame).unwrap());
        } else {
            event = Some(serde_json::from_value(frame).unwrap());
        }
    }
    (response.unwrap(), event.unwrap())
}

async fn read_response<R>(reader: &mut JsonlReader<R>) -> DaemonResponse
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let frame: serde_json::Value = tokio::time::timeout(Duration::from_secs(2), reader.read())
        .await
        .expect("response frame is not timed out")
        .expect("response frame is readable")
        .expect("response frame exists");
    assert!(
        frame.get("ok").is_some(),
        "expected response frame, got {frame:?}"
    );
    serde_json::from_value(frame).expect("response frame is valid")
}

fn test_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git command failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git command failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

async fn mark_arena_candidate(
    runtime: &DaemonRuntime,
    worktree_id: WorktreeId,
    diff_stat: Option<String>,
    test_status: Option<TestRunStatus>,
) {
    let mut state = runtime.state.write().await;
    state.arena_candidates.insert(
        worktree_id.clone(),
        ArenaCandidate {
            worktree_id,
            agent_id: AgentSessionId::new(),
            provider: "test".to_string(),
            diff_stat,
            test_status,
        },
    );
}

async fn wait_for_worktree_status(
    runtime: &DaemonRuntime,
    worktree_id: &WorktreeId,
    expected: WorktreeStatus,
) {
    for _ in 0..20 {
        let worktree = runtime.worktree_by_id(worktree_id).await.unwrap();
        if worktree.status == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let worktree = runtime.worktree_by_id(worktree_id).await.unwrap();
    assert_eq!(worktree.status, expected);
}

async fn assert_no_frame<R>(reader: &mut JsonlReader<R>)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let frame = tokio::time::timeout(
        Duration::from_millis(50),
        reader.read::<serde_json::Value>(),
    )
    .await;
    assert!(frame.is_err(), "unexpected daemon frame: {frame:?}");
}

fn pty_capture_script() -> &'static str {
    r#"my $out = shift; open my $fh, ">", $out or die $!; select((select($fh), $| = 1)[0]); while (defined(my $line = <STDIN>)) { print {$fh} $line; last if $line =~ /AGENTMUX_RESULT JSON/; }"#
}

async fn wait_for_file_contains(path: &Path, needle: &str) -> Option<String> {
    for _ in 0..100 {
        if let Ok(output) = std::fs::read_to_string(path)
            && output.contains(needle)
        {
            return Some(output);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

async fn terminate_agent_process(runtime: &DaemonRuntime, agent_id: &str) {
    let agent_id = parse_agent_session_id(agent_id).unwrap();
    let state = runtime.state.read().await;
    let live_agent = state.agents.get(&agent_id).unwrap();
    if let Some(pty) = &live_agent.pty {
        let mut pty = pty.lock().unwrap();
        let _ = pty.terminate();
        // Bounded reap (<=2s): never block the current-thread test runtime
        // forever if the child does not exit promptly (e.g. a shell that
        // keeps the PTY open via a child process). The assertions that
        // matter run before termination, so the exit status is irrelevant.
        for _ in 0..200 {
            match pty.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}
