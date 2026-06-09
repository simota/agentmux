//! Interactive TUI session runtime, stream-frame handling, pane sizing, and copy-mode helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::time::Duration;

use agentmux_core::{AgentmuxError, error::Result};
use agentmux_ipc::{
    ClientHello, DaemonStreamFrame, JsonlReader, JsonlWriter,
};
use agentmux_ipc::ClientRequest;
use agentmux_tui::{
    input::{InputForwardError, dispatch_to_daemon_request},
    keymap::KeymapDispatcher,
    layout::{PaneLayout, Rect, SplitDirection},
    render::TuiSessionRenderer,
    state::{
        CommandEffect, CopyPoint, CopySelection, StateChange,
        TerminalSize as TuiTerminalSize, TuiSessionState,
    },
    terminal::{CrosstermTerminalIo, TerminalIo, TerminalSession},
};
use crossterm::{
    event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    terminal as crossterm_terminal,
};
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::daemon::ensure_daemon;
use crate::output::response_error;
use crate::requests::{
    agent_broadcast_input_request, agent_resize_request,
    agent_spawn_for_provider_request_with_id, agent_spawn_for_provider_request_with_size,
    agent_stop_request, attach_request, detach_request, message_list_request, snapshot_request,
    tui_daemon_status_request,
};
#[cfg(feature = "activity-feed")]
use crate::requests::event_subscribe_request;
#[cfg(feature = "arena")]
use crate::requests::worktree_adopt_request;
#[cfg(feature = "activity-feed")]
use crate::daemon::daemon_supports_event_subscribe;
#[cfg(feature = "arena")]
use crate::daemon::daemon_supports_arena_state;
use crate::cli::StartupPaneChoice;
use crate::parse::{StartupLayout, StartupLayoutNode};
use agentmux_tui::layout::LayoutNode;
use agentmux_tui::state::{COMMANDS_PANE_ID, CONVERSATION_LIST_PANE_ID};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TuiSignal {
    Sigint,
}

pub(crate) fn tui_signal_effect(signal: TuiSignal) -> CommandEffect {
    match signal {
        TuiSignal::Sigint => CommandEffect::Detach,
    }
}

pub(crate) fn tui_close_request(effect: CommandEffect) -> Option<ClientRequest> {
    match effect {
        CommandEffect::Detach | CommandEffect::Quit => Some(detach_request()),
        _ => None,
    }
}

pub(crate) fn spawn_tui_signal_forwarder(signal_tx: mpsc::UnboundedSender<TuiSignal>) -> JoinHandle<()> {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = signal_tx.send(TuiSignal::Sigint);
        }
    })
}

pub(crate) async fn run_bare_tui_session(socket_path: &Path) -> Result<()> {
    run_tui_session(socket_path, None).await
}

pub(crate) async fn run_tui_session(socket_path: &Path, target: Option<String>) -> Result<()> {
    // Attach path has no startup layout; panes arrive from the daemon snapshot.
    run_tui_session_inner(socket_path, target, None).await
}

pub(crate) async fn run_tui_session_with_startup_panes(
    socket_path: &Path,
    layout: StartupLayout,
) -> Result<()> {
    run_tui_session_inner(socket_path, None, Some(layout)).await
}

pub(crate) async fn run_tui_session_inner(
    socket_path: &Path,
    target: Option<String>,
    startup_layout: Option<StartupLayout>,
) -> Result<()> {
    ensure_daemon(socket_path).await?;
    let stream = UnixStream::connect(socket_path).await.map_err(|error| {
        AgentmuxError::IpcError(format!(
            "failed to connect daemon socket '{}': {error}",
            socket_path.display()
        ))
    })?;
    let (reader, writer) = stream.into_split();
    let mut reader = JsonlReader::new(BufReader::new(reader));
    let mut writer = JsonlWriter::new(writer);

    writer
        .write(&ClientHello::new(env!("CARGO_PKG_VERSION")))
        .await?;

    let status_request = tui_daemon_status_request();
    let status_request_id = status_request.id.clone();
    writer.write(&status_request).await?;

    // Depth-first leaf choices drive deterministic spawn ordering. `split_direction`
    // is the root split's direction (already in engine terms: spec `|` -> `Vertical`,
    // spec `―` -> `Horizontal`; see `parse_start_layout`).
    let startup_panes: Vec<StartupPaneChoice> = startup_layout
        .as_ref()
        .map(|layout| layout.panes.clone())
        .unwrap_or_default();
    let split_direction = startup_layout
        .as_ref()
        .map(startup_root_direction)
        .unwrap_or(SplitDirection::Vertical);
    let open_startup_messages = startup_panes
        .iter()
        .any(|pane| matches!(pane, StartupPaneChoice::Messages));
    let open_startup_commands = startup_panes
        .iter()
        .any(|pane| matches!(pane, StartupPaneChoice::Commands));
    let startup_spawn_requests = startup_panes
        .iter()
        .copied()
        .filter_map(|pane| match pane {
            StartupPaneChoice::Agent(provider) => Some(provider),
            StartupPaneChoice::Messages | StartupPaneChoice::Commands => None,
        })
        .enumerate()
        .map(|(index, provider)| {
            agent_spawn_for_provider_request_with_id(
                format!("req_start_agent_spawn_{index}"),
                provider,
                None,
            )
        })
        .collect::<Vec<_>>();
    for request in &startup_spawn_requests {
        writer.write(request).await?;
    }
    let startup_message_list_request = open_startup_messages.then(message_list_request);
    if let Some(request) = &startup_message_list_request {
        writer.write(request).await?;
    }
    let startup_spawn_request_ids = startup_spawn_requests
        .iter()
        .map(|request| request.id.clone())
        .collect::<Vec<_>>();
    let attach_and_snapshot = target.map(|target| {
        let snapshot_request = snapshot_request(target.clone());
        let attach_request = attach_request(target);
        (attach_request, snapshot_request)
    });
    if let Some((attach_request, snapshot_request)) = &attach_and_snapshot {
        writer.write(attach_request).await?;
        writer.write(snapshot_request).await?;
    }

    let mut state = TuiSessionState::new(split_direction);
    let mut startup_agent_ids = Vec::new();
    if let Some((attach_request, snapshot_request)) = &attach_and_snapshot {
        let _bootstrap = wait_for_tui_bootstrap(
            &mut reader,
            &mut state,
            &status_request_id,
            Some(&attach_request.id),
            Some(&snapshot_request.id),
            &startup_spawn_request_ids,
            startup_message_list_request
                .as_ref()
                .map(|request| request.id.as_str()),
        )
        .await?;
    } else {
        let bootstrap = wait_for_tui_bootstrap(
            &mut reader,
            &mut state,
            &status_request_id,
            None,
            None,
            &startup_spawn_request_ids,
            startup_message_list_request
                .as_ref()
                .map(|request| request.id.as_str()),
        )
        .await?;
        startup_agent_ids = bootstrap.agent_ids;

        // Apply the parsed startup tree once every leaf can be resolved to a
        // concrete pane id. Provider leaves map (in spec/DFS order) to spawned
        // agent ids via their `req_start_agent_spawn_{n}` request id; the
        // conversation-list leaf maps to the local conversation-list pane id.
        // Falling back to the daemon-driven flat order keeps behavior identical
        // when resolution is impossible (e.g. a spawn failed to report an id).
        if let Some(root) =
            resolve_startup_layout(startup_layout.as_ref(), &bootstrap.spawned_by_request)
        {
            state.apply_startup_layout(root);
        } else if open_startup_messages {
            state.open_conversation_list_pane();
        } else if open_startup_commands {
            state.open_commands_pane();
        }
        if state.layout().panes().is_empty() {
            state.open_provider_picker();
        }
    }

    let terminal_io = CrosstermTerminalIo::new(io::stdout()).map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to initialise terminal UI: {error}"))
    })?;
    let mut terminal = TerminalSession::enter(terminal_io).map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to enter terminal UI: {error}"))
    })?;

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match reader.read::<DaemonStreamFrame>().await {
                Ok(Some(frame)) => {
                    if frame_tx.send(Ok(frame)).is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = frame_tx.send(Err(AgentmuxError::IpcError(
                        "daemon closed the attached event stream".to_string(),
                    )));
                    break;
                }
                Err(error) => {
                    let _ = frame_tx.send(Err(error));
                    break;
                }
            }
        }
    });

    let renderer = TuiSessionRenderer::default();
    let mut keymap = KeymapDispatcher::default();
    let mut input_sequence = 0_u64;
    let mut resize_sequence = 0_u64;
    let mut copy_mode = false;
    let mut copy_drag_start: Option<CopyPoint> = None;
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let signal_task = spawn_tui_signal_forwarder(signal_tx);

    sync_current_terminal_pane_sizes(&mut writer, &mut state, &mut resize_sequence).await?;
    if let Some(first_agent_id) = startup_agent_ids.first() {
        writer
            .write(&attach_request(first_agent_id.clone()))
            .await?;
    }
    for agent_id in &startup_agent_ids {
        writer.write(&snapshot_request(agent_id.clone())).await?;
    }
    draw_tui_frame(&mut terminal, &renderer, &state)?;

    // Cap how many PTY output frames are drained before the loop yields to the
    // keyboard poll below. Without this bound, an agent that streams output
    // continuously (e.g. agy while it is running) keeps `frame_rx` non-empty, so
    // an unbounded drain loop never falls through to `poll_event` and human keys
    // (Esc and every other key) are starved for as long as the agent keeps
    // producing output. Draining a bounded batch and coalescing the redraw into a
    // single call per iteration keeps input responsive under heavy output.
    const MAX_FRAMES_PER_TICK: usize = 64;
    loop {
        let mut pending_redraw = false;
        for _ in 0..MAX_FRAMES_PER_TICK {
            // Stream-level errors (daemon closed / read failure) stay fatal via `?`.
            // A per-request response error during the session — e.g. a keystroke
            // forwarded to an agent with no live PTY — must NOT tear down the
            // cockpit; surface it as a notice and keep running.
            let frame = match frame_rx.try_recv() {
                Ok(frame) => frame?,
                Err(_) => break,
            };
            let spawned_agent_id = spawned_agent_id_from_frame(&frame);
            let _notice = apply_runtime_stream_frame(&mut state, frame);
            if let Some(agent_id) = spawned_agent_id {
                sync_current_terminal_pane_sizes(&mut writer, &mut state, &mut resize_sequence)
                    .await?;
                writer.write(&attach_request(agent_id.clone())).await?;
                writer.write(&snapshot_request(agent_id)).await?;
            }
            pending_redraw = true;
        }
        if pending_redraw {
            draw_tui_frame(&mut terminal, &renderer, &state)?;
        }

        if let Ok(signal) = signal_rx.try_recv() {
            if let Some(request) = tui_close_request(tui_signal_effect(signal)) {
                writer.write(&request).await?;
                break;
            }
        }

        if let Some(event) = terminal
            .io_mut()
            .poll_event(Duration::from_millis(16))
            .map_err(|error| AgentmuxError::TerminalError(format!("failed to read key: {error}")))?
        {
            let Event::Key(key) = event else {
                if let Event::Resize(cols, rows) = event {
                    for request in
                        resize_panes_for_terminal(&mut state, cols, rows, &mut resize_sequence)
                    {
                        writer.write(&request).await?;
                    }
                    draw_tui_frame(&mut terminal, &renderer, &state)?;
                } else if copy_mode && let Event::Mouse(mouse) = event {
                    let (cols, rows) = current_terminal_size()?;
                    if let Some(action) = copy_mode_mouse_action(
                        &mut state,
                        cols,
                        rows,
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                        &mut copy_drag_start,
                    ) {
                        match action {
                            CopyModeAction::Redraw => {
                                draw_tui_frame(&mut terminal, &renderer, &state)?;
                            }
                            CopyModeAction::CopyAndExit(text) => {
                                terminal
                                    .io_mut()
                                    .copy_to_clipboard(&text)
                                    .map_err(|error| {
                                        AgentmuxError::TerminalError(format!(
                                            "failed to copy selection to clipboard: {error}"
                                        ))
                                    })?;
                                terminal
                                    .io_mut()
                                    .set_mouse_capture(false)
                                    .map_err(|error| {
                                        AgentmuxError::TerminalError(format!(
                                            "failed to disable mouse capture: {error}"
                                        ))
                                    })?;
                                copy_mode = false;
                                copy_drag_start = None;
                                state.reset_focused_pane_scroll();
                                state.clear_copy_selection();
                                draw_tui_frame(&mut terminal, &renderer, &state)?;
                            }
                        }
                    }
                } else if let Event::Mouse(mouse) = event
                    && let Some(delta) = mouse_scroll_delta(mouse.kind)
                {
                    let (cols, rows) = current_terminal_size()?;
                    if scroll_pane_at(&mut state, cols, rows, mouse.column, mouse.row, delta) {
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                }
                continue;
            };
            if copy_mode && copy_mode_key_exits(key.code, key.modifiers) {
                terminal
                    .io_mut()
                    .set_mouse_capture(false)
                    .map_err(|error| {
                        AgentmuxError::TerminalError(format!(
                            "failed to disable mouse capture: {error}"
                        ))
                    })?;
                copy_mode = false;
                copy_drag_start = None;
                state.reset_focused_pane_scroll();
                state.clear_copy_selection();
                draw_tui_frame(&mut terminal, &renderer, &state)?;
                continue;
            }
            let conversation_list_focused = state
                .layout()
                .focused()
                .is_some_and(|pane_id| state.is_conversation_list_pane(pane_id));
            let commands_pane_focused = state
                .layout()
                .focused()
                .is_some_and(|pane_id| state.is_commands_pane(pane_id));
            #[cfg(feature = "activity-feed")]
            let activity_feed_focused = state
                .layout()
                .focused()
                .is_some_and(|pane_id| state.is_activity_feed_pane(pane_id));
            #[cfg(all(feature = "arena", feature = "activity-feed"))]
            let dispatch = keymap.dispatch_with_arena_context(
                key,
                state.session_list_visible(),
                state.message_bus_visible(),
                state.provider_picker_visible(),
                conversation_list_focused,
                state.arena_overlay_visible(),
                activity_feed_focused,
            );
            #[cfg(all(feature = "arena", not(feature = "activity-feed")))]
            let dispatch = keymap.dispatch_with_arena_context(
                key,
                state.session_list_visible(),
                state.message_bus_visible(),
                state.provider_picker_visible(),
                conversation_list_focused,
                state.arena_overlay_visible(),
                false,
            );
            #[cfg(all(not(feature = "arena"), feature = "activity-feed"))]
            #[cfg(feature = "activity-feed")]
            let dispatch = keymap.dispatch_with_activity_feed_context(
                key,
                state.session_list_visible(),
                state.message_bus_visible(),
                state.provider_picker_visible(),
                conversation_list_focused,
                activity_feed_focused,
            );
            #[cfg(all(not(feature = "arena"), not(feature = "activity-feed")))]
            let dispatch = keymap.dispatch_with_context(
                key,
                state.session_list_visible(),
                state.message_bus_visible(),
                state.provider_picker_visible(),
                conversation_list_focused,
            );
            // Mirror the dispatcher's prefix state into render state so the
            // status bar can show the PREFIX indicator (single source of truth
            // stays in the dispatcher; this just reflects it each key event).
            state.set_prefix_active(keymap.is_awaiting_prefix_command());

            // Commands pane owns plain keystrokes: route them to the local input
            // editor instead of an agent PTY. Prefix-mode commands (pane switch,
            // close, etc.) still flow through `apply_command` below, because the
            // dispatcher classifies them as `Command`/`PrefixStarted` rather than
            // forwarded/consumed input.
            if commands_pane_focused
                && matches!(
                    dispatch,
                    agentmux_tui::keymap::KeyDispatch::ForwardToFocusedPane(_)
                        | agentmux_tui::keymap::KeyDispatch::Consumed
                )
            {
                match commands_input_key(key.code) {
                    Some(CommandsInputAction::Send) => {
                        if let Some(CommandEffect::BroadcastInput { target, text }) =
                            state.commands_broadcast_effect()
                        {
                            match agent_broadcast_input_request(target.clone(), text.clone(), true) {
                                Ok(request) => {
                                    state.begin_commands_broadcast(target, text);
                                    writer.write(&request).await?;
                                }
                                Err(error) => state.set_runtime_notice(error.to_string()),
                            }
                        }
                    }
                    Some(CommandsInputAction::CycleTarget) => state.cycle_commands_target(),
                    Some(CommandsInputAction::Clear) => state.commands_input_clear(),
                    Some(CommandsInputAction::Backspace) => state.commands_input_backspace(),
                    Some(CommandsInputAction::Insert(ch)) => state.commands_input_push(ch),
                    None => {}
                }
                draw_tui_frame(&mut terminal, &renderer, &state)?;
                continue;
            }

            if let Some(command) = match &dispatch {
                agentmux_tui::keymap::KeyDispatch::Command(command) => Some(*command),
                _ => None,
            } {
                match state.apply_command(command) {
                    CommandEffect::Continue => {
                        sync_current_terminal_pane_sizes(
                            &mut writer,
                            &mut state,
                            &mut resize_sequence,
                        )
                        .await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    CommandEffect::SpawnAgentPane(provider) => {
                        let spawn_size = current_terminal_size()
                            .ok()
                            .and_then(|(cols, rows)| pending_spawn_pane_size(&state, cols, rows));
                        writer
                            .write(&agent_spawn_for_provider_request_with_size(
                                provider, spawn_size,
                            ))
                            .await?;
                    }
                    CommandEffect::OpenConversationListPane => {
                        writer.write(&message_list_request()).await?;
                        sync_current_terminal_pane_sizes(
                            &mut writer,
                            &mut state,
                            &mut resize_sequence,
                        )
                        .await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    CommandEffect::OpenCommandsPane => {
                        sync_current_terminal_pane_sizes(
                            &mut writer,
                            &mut state,
                            &mut resize_sequence,
                        )
                        .await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    CommandEffect::BroadcastInput { target, text } => {
                        // Plain-key path emits this directly; this arm covers the
                        // (currently unused) command path and keeps the match total.
                        match agent_broadcast_input_request(target.clone(), text.clone(), true) {
                            Ok(request) => {
                                state.begin_commands_broadcast(target, text);
                                writer.write(&request).await?;
                            }
                            Err(error) => {
                                state.set_runtime_notice(error.to_string());
                                draw_tui_frame(&mut terminal, &renderer, &state)?;
                            }
                        }
                    }
                    #[cfg(feature = "activity-feed")]
                    CommandEffect::ToggleActivityFeedPane { visible } => {
                        if visible {
                            if daemon_supports_event_subscribe(&state) {
                                writer
                                    .write(&event_subscribe_request(state.feed_filter()))
                                    .await?;
                            } else {
                                state
                                    .set_runtime_notice("Activity Feed unsupported by this daemon");
                            }
                        }
                        sync_current_terminal_pane_sizes(
                            &mut writer,
                            &mut state,
                            &mut resize_sequence,
                        )
                        .await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    #[cfg(feature = "activity-feed")]
                    CommandEffect::FocusPaneById(agent_id) => {
                        state.layout_mut().focus(&agent_id);
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    #[cfg(feature = "arena")]
                    CommandEffect::ArenaAdopt(worktree_id) => {
                        if daemon_supports_arena_state(&state) {
                            writer.write(&worktree_adopt_request(worktree_id)).await?;
                        } else {
                            state.set_runtime_notice("Arena unsupported by this daemon");
                            draw_tui_frame(&mut terminal, &renderer, &state)?;
                        }
                    }
                    CommandEffect::StopPane(agent_id) => {
                        writer.write(&agent_stop_request(agent_id)).await?;
                    }
                    CommandEffect::RefreshMessages => {
                        writer.write(&message_list_request()).await?;
                        draw_tui_frame(&mut terminal, &renderer, &state)?;
                    }
                    CommandEffect::Unhandled(agentmux_tui::keymap::TuiCommand::EnterCopyMode) => {
                        if state.focused_pane().is_some() {
                            terminal.io_mut().set_mouse_capture(true).map_err(|error| {
                                AgentmuxError::TerminalError(format!(
                                    "failed to enable mouse capture: {error}"
                                ))
                            })?;
                            copy_mode = true;
                            copy_drag_start = None;
                            state.clear_copy_selection();
                            draw_tui_frame(&mut terminal, &renderer, &state)?;
                        }
                    }
                    CommandEffect::Detach => {
                        writer.write(&detach_request()).await?;
                        break;
                    }
                    CommandEffect::Quit => {
                        writer.write(&detach_request()).await?;
                        break;
                    }
                    CommandEffect::Unhandled(_) => {}
                }
                continue;
            }

            input_sequence = input_sequence.saturating_add(1);
            let request_id = format!("req_input_{input_sequence}");
            match dispatch_to_daemon_request(&state, request_id, dispatch) {
                Ok(Some(request)) => writer.write(&request).await?,
                Ok(None) | Err(InputForwardError::NoFocusedPane) => {}
                Err(error) => {
                    return Err(AgentmuxError::UserError(format!(
                        "failed to forward input to focused agent: {error:?}"
                    )));
                }
            }
        }
    }

    signal_task.abort();

    terminal.shutdown().map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to restore terminal UI: {error}"))
    })
}

/// Outcome of the TUI bootstrap handshake.
pub(crate) struct TuiBootstrap {
    /// Spawned agent ids in arrival order (used for attach/snapshot fan-out).
    pub agent_ids: Vec<String>,
    /// Map from each startup spawn request id to the spawned agent id, used to
    /// resolve startup-layout leaves to concrete pane ids in spec order.
    pub spawned_by_request: BTreeMap<String, String>,
}

pub(crate) async fn wait_for_tui_bootstrap<R>(
    reader: &mut JsonlReader<R>,
    state: &mut TuiSessionState,
    status_request_id: &str,
    attach_request_id: Option<&str>,
    snapshot_request_id: Option<&str>,
    startup_spawn_request_ids: &[String],
    startup_message_list_request_id: Option<&str>,
) -> Result<TuiBootstrap>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut status_received = false;
    let mut attach_received = attach_request_id.is_none();
    let mut snapshot_received = snapshot_request_id.is_none();
    let mut startup_messages_received = startup_message_list_request_id.is_none();
    let mut startup_spawn_received = BTreeSet::new();
    let mut startup_agent_ids = Vec::new();
    let mut spawned_by_request = BTreeMap::new();

    while !(status_received
        && attach_received
        && snapshot_received
        && startup_messages_received
        && startup_spawn_received.len() == startup_spawn_request_ids.len())
    {
        let frame = reader.read::<DaemonStreamFrame>().await?.ok_or_else(|| {
            AgentmuxError::IpcError("daemon closed before TUI attach completed".to_string())
        })?;
        let spawned_agent_id = spawned_agent_id_from_frame(&frame);
        if let Some(response_id) = apply_tui_stream_frame(state, frame)? {
            if response_id == status_request_id {
                status_received = true;
            }
            if Some(response_id.as_str()) == attach_request_id {
                attach_received = true;
            }
            if Some(response_id.as_str()) == snapshot_request_id {
                snapshot_received = true;
            }
            if Some(response_id.as_str()) == startup_message_list_request_id {
                startup_messages_received = true;
            }
            if startup_spawn_request_ids.contains(&response_id) {
                startup_spawn_received.insert(response_id.clone());
                if let Some(agent_id) = spawned_agent_id {
                    spawned_by_request.insert(response_id, agent_id.clone());
                    startup_agent_ids.push(agent_id);
                }
            }
        }
    }

    Ok(TuiBootstrap {
        agent_ids: startup_agent_ids,
        spawned_by_request,
    })
}

pub(crate) fn apply_tui_stream_frame(
    state: &mut TuiSessionState,
    frame: DaemonStreamFrame,
) -> Result<Option<String>> {
    match frame {
        DaemonStreamFrame::Response(response) => {
            if !response.ok {
                return Err(response_error("tui", response));
            }
            if response.id == "req_tui_status" {
                state.apply_daemon_status(&response.payload.clone().unwrap_or_else(|| json!({})));
            }
            if response.id == "req_snapshot" {
                state.apply_snapshot(&response.payload.clone().unwrap_or_else(|| json!({})));
            }
            if response.id == "req_message_list" {
                state.apply_message_list_payload(
                    &response.payload.clone().unwrap_or_else(|| json!({})),
                );
            }
            if is_agent_spawn_response_id(&response.id) {
                if let Some(payload) = response.payload.as_ref() {
                    state.apply_daemon_status(&json!({ "agents": [payload] }));
                }
            }
            if response.id == "req_agent_broadcast_input" {
                state.apply_commands_broadcast_response(
                    &response.payload.clone().unwrap_or_else(|| json!({})),
                );
            }
            Ok(Some(response.id))
        }
        DaemonStreamFrame::Event(event) => {
            state.apply_event(&event);
            Ok(None)
        }
    }
}

pub(crate) fn spawned_agent_id_from_frame(frame: &DaemonStreamFrame) -> Option<String> {
    let DaemonStreamFrame::Response(response) = frame else {
        return None;
    };
    if !response.ok || !is_agent_spawn_response_id(&response.id) {
        return None;
    }
    response
        .payload
        .as_ref()
        .and_then(|payload| payload.get("agent_id"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

pub(crate) fn is_agent_spawn_response_id(response_id: &str) -> bool {
    response_id == "req_agent_spawn_provider"
        || response_id == "req_bare_agent_spawn"
        || response_id.starts_with("req_start_agent_spawn_")
}

/// Apply a stream frame during the interactive loop. Unlike bootstrap, a failed
/// per-request response (e.g. input to an agent with no live PTY) is non-fatal:
/// the error text is returned as a notice so the session can keep running.
pub(crate) fn apply_runtime_stream_frame(
    state: &mut TuiSessionState,
    frame: DaemonStreamFrame,
) -> Option<String> {
    match apply_tui_stream_frame(state, frame) {
        Ok(_) => None,
        Err(error) => {
            let notice = error.to_string();
            state.set_runtime_notice(notice.clone());
            Some(notice)
        }
    }
}

pub(crate) fn draw_tui_frame<T: TerminalIo>(
    terminal: &mut TerminalSession<T>,
    renderer: &TuiSessionRenderer,
    state: &TuiSessionState,
) -> Result<()> {
    terminal
        .io_mut()
        .draw(|frame| renderer.render(frame.area(), state, frame.buffer_mut()))
        .map_err(|error| AgentmuxError::TerminalError(format!("failed to draw TUI: {error}")))
}

pub(crate) async fn sync_current_terminal_pane_sizes<W>(
    writer: &mut JsonlWriter<W>,
    state: &mut TuiSessionState,
    resize_sequence: &mut u64,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let (cols, rows) = current_terminal_size()?;
    for request in resize_panes_for_terminal(state, cols, rows, resize_sequence) {
        writer.write(&request).await?;
    }
    Ok(())
}

pub(crate) fn current_terminal_size() -> Result<(u16, u16)> {
    crossterm_terminal::size().map_err(|error| {
        AgentmuxError::TerminalError(format!("failed to read terminal size: {error}"))
    })
}

/// The engine split direction of the parsed startup layout's root.
fn startup_root_direction(layout: &StartupLayout) -> SplitDirection {
    match &layout.root {
        StartupLayoutNode::Split { direction, .. } => *direction,
        StartupLayoutNode::Leaf(_) => SplitDirection::Vertical,
    }
}

/// Resolve a parsed startup layout into a concrete engine [`LayoutNode`] tree.
///
/// Returns `None` when there is no startup layout, when the layout has no panes
/// (picker case), or when any provider leaf cannot be matched to a spawned agent
/// id (so callers can fall back to the daemon-driven flat ordering).
fn resolve_startup_layout(
    layout: Option<&StartupLayout>,
    spawned_by_request: &BTreeMap<String, String>,
) -> Option<LayoutNode> {
    let layout = layout?;
    if layout.panes.is_empty() {
        return None;
    }

    // Walk leaves in DFS order; provider leaves consume spawn request ids in the
    // same order they were issued (`req_start_agent_spawn_0`, `_1`, ...).
    let mut agent_index = 0usize;
    let mut resolution_failed = false;
    let root = layout.resolve_root(|choice| match choice {
        StartupPaneChoice::Agent(_) => {
            let request_id = format!("req_start_agent_spawn_{agent_index}");
            agent_index += 1;
            match spawned_by_request.get(&request_id) {
                Some(agent_id) => agent_id.clone(),
                None => {
                    resolution_failed = true;
                    request_id
                }
            }
        }
        StartupPaneChoice::Messages => CONVERSATION_LIST_PANE_ID.to_string(),
        StartupPaneChoice::Commands => COMMANDS_PANE_ID.to_string(),
    });

    if resolution_failed {
        return None;
    }
    Some(root)
}

pub(crate) fn pending_spawn_pane_size(
    state: &TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
) -> Option<TuiTerminalSize> {
    let pending_pane = "__agentmux_pending_spawn__".to_string();
    // Append the pending leaf at the root level — the same slot dynamic
    // `add_pane` would use — so its computed rect matches the real spawn.
    let pending_layout = state.layout().with_pending_pane(pending_pane.clone());

    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    pending_layout
        .pane_rects(area)
        .into_iter()
        .find_map(|(agent_id, rect)| {
            if agent_id != pending_pane {
                return None;
            }
            let (rows, cols) = PaneLayout::pane_inner_size(rect);
            (rows > 0 && cols > 0).then_some(TuiTerminalSize { rows, cols })
        })
}

pub(crate) fn resize_panes_for_terminal(
    state: &mut TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
    resize_sequence: &mut u64,
) -> Vec<ClientRequest> {
    resize_pane_sizes(state, terminal_cols, terminal_rows)
        .into_iter()
        .map(|(agent_id, size)| {
            *resize_sequence = resize_sequence.saturating_add(1);
            state.resize_pane(&agent_id, size);
            agent_resize_request(format!("req_resize_{resize_sequence}"), agent_id, size)
        })
        .collect()
}

pub(crate) fn mouse_scroll_delta(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(3),
        MouseEventKind::ScrollDown => Some(-3),
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CopyModeAction {
    Redraw,
    CopyAndExit(String),
}

/// A Commands-panel input editor action derived from a single key press.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandsInputAction {
    Send,
    CycleTarget,
    Clear,
    Backspace,
    Insert(char),
}

/// Map a key code to a Commands-panel input action.
///
/// `Enter` submits, `Tab` cycles the broadcast target, `Esc` clears, `Backspace`
/// deletes, and printable characters are inserted. Other keys are ignored.
pub(crate) fn commands_input_key(code: KeyCode) -> Option<CommandsInputAction> {
    match code {
        KeyCode::Enter => Some(CommandsInputAction::Send),
        KeyCode::Tab => Some(CommandsInputAction::CycleTarget),
        KeyCode::Esc => Some(CommandsInputAction::Clear),
        KeyCode::Backspace => Some(CommandsInputAction::Backspace),
        KeyCode::Char(ch) => Some(CommandsInputAction::Insert(ch)),
        _ => None,
    }
}

pub(crate) fn copy_mode_key_exits(code: KeyCode, modifiers: KeyModifiers) -> bool {
    if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return false;
    }
    matches!(code, KeyCode::Esc | KeyCode::Char('q'))
}

pub(crate) fn copy_mode_mouse_action(
    state: &mut TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
    kind: MouseEventKind,
    mouse_col: u16,
    mouse_row: u16,
    drag_start: &mut Option<CopyPoint>,
) -> Option<CopyModeAction> {
    if let Some(delta) = mouse_scroll_delta(kind) {
        return (!matches!(state.scroll_focused_pane(delta), StateChange::Ignored))
            .then_some(CopyModeAction::Redraw);
    }

    let agent_id = state.layout().focused()?.to_string();
    let inner = focused_pane_inner_rect(state, terminal_cols, terminal_rows)?;

    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if !rect_contains(inner, mouse_col, mouse_row) {
                return None;
            }
            let point = copy_point_from_mouse(inner, mouse_col, mouse_row);
            *drag_start = Some(point);
            state.set_copy_selection(CopySelection::new(agent_id, point, point));
            Some(CopyModeAction::Redraw)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let start = (*drag_start)?;
            let point = copy_point_from_mouse(inner, mouse_col, mouse_row);
            state.set_copy_selection(CopySelection::new(agent_id, start, point));
            Some(CopyModeAction::Redraw)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let start = (*drag_start)?;
            let point = copy_point_from_mouse(inner, mouse_col, mouse_row);
            let selection = CopySelection::new(agent_id, start, point);
            let text = selected_text(state, &selection, inner.height);
            state.set_copy_selection(selection);
            *drag_start = None;
            Some(CopyModeAction::CopyAndExit(text))
        }
        _ => None,
    }
}

pub(crate) fn focused_pane_inner_rect(
    state: &TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
) -> Option<Rect> {
    let focused = state.layout().focused()?;
    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    state
        .layout()
        .pane_rects(area)
        .into_iter()
        .find_map(|(agent_id, rect)| (agent_id == focused).then_some(inner_rect(rect)))
        .filter(|rect| rect.width > 0 && rect.height > 0)
}

pub(crate) fn inner_rect(rect: Rect) -> Rect {
    Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
}

pub(crate) fn copy_point_from_mouse(inner: Rect, mouse_col: u16, mouse_row: u16) -> CopyPoint {
    CopyPoint {
        row: mouse_row
            .saturating_sub(inner.y)
            .min(inner.height.saturating_sub(1)),
        col: mouse_col
            .saturating_sub(inner.x)
            .min(inner.width.saturating_sub(1)),
    }
}

pub(crate) fn selected_text(
    state: &TuiSessionState,
    selection: &CopySelection,
    viewport_height: u16,
) -> String {
    let Some(pane) = state.pane(&selection.agent_id) else {
        return String::new();
    };
    let grid = pane.grid();
    let total_rows = grid.scrollback().len() + usize::from(grid.rows());
    let visible_rows = usize::from(viewport_height).min(total_rows);
    if visible_rows == 0 {
        return String::new();
    }
    let start_history_row = total_rows.saturating_sub(visible_rows).saturating_sub(
        pane.scroll_offset()
            .min(total_rows.saturating_sub(visible_rows)),
    );

    let (start, end) = selection.normalized();
    let mut lines = Vec::new();
    for row in start.row..=end.row {
        let mut line = String::new();
        let first_col = if row == start.row { start.col } else { 0 };
        let last_col = if row == end.row {
            end.col
        } else {
            grid.cols().saturating_sub(1)
        };
        let history_row = start_history_row + usize::from(row);
        for col in first_col..=last_col.min(grid.cols().saturating_sub(1)) {
            if let Some(ch) = visible_cell_char(grid, history_row, col) {
                line.push(ch);
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

pub(crate) fn visible_cell_char(
    grid: &agentmux_terminal::ScreenGrid,
    history_row: usize,
    col: u16,
) -> Option<char> {
    let scrollback_rows = grid.scrollback().len();
    if history_row < scrollback_rows {
        return grid
            .scrollback()
            .get(history_row)
            .and_then(|line| line.cells().get(usize::from(col)))
            .map(|cell| cell.ch);
    }
    let grid_row = history_row.checked_sub(scrollback_rows)?;
    let grid_row = u16::try_from(grid_row).ok()?;
    grid.cell(grid_row, col).map(|cell| cell.ch)
}

pub(crate) fn scroll_pane_at(
    state: &mut TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
    mouse_col: u16,
    mouse_row: u16,
    delta: isize,
) -> bool {
    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    let target = state
        .layout()
        .pane_rects(area)
        .into_iter()
        .find_map(|(agent_id, rect)| rect_contains(rect, mouse_col, mouse_row).then_some(agent_id))
        .or_else(|| state.layout().focused().map(ToOwned::to_owned));
    let Some(agent_id) = target else {
        return false;
    };
    !matches!(state.scroll_pane(&agent_id, delta), StateChange::Ignored)
}

pub(crate) fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && row >= rect.y
        && col < rect.x.saturating_add(rect.width)
        && row < rect.y.saturating_add(rect.height)
}

pub(crate) fn resize_pane_sizes(
    state: &TuiSessionState,
    terminal_cols: u16,
    terminal_rows: u16,
) -> Vec<(String, TuiTerminalSize)> {
    let area = Rect::new(0, 0, terminal_cols, terminal_rows);
    state
        .layout()
        .pane_rects(area)
        .into_iter()
        .filter_map(|(agent_id, rect)| {
            #[cfg(feature = "activity-feed")]
            if state.is_activity_feed_pane(&agent_id) {
                return None;
            }
            state.pane(&agent_id)?;
            let (rows, cols) = PaneLayout::pane_inner_size(rect);
            (rows > 0 && cols > 0).then_some((agent_id, TuiTerminalSize { rows, cols }))
        })
        .collect()
}

