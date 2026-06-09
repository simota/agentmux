//! `agentmux` — CLI entry point.
//!
//! Top-level subcommands mirror `docs/spec/11_cli_tui_user_spec.md §2`.
//! The CLI is a thin JSONL/Unix-socket client for the daemon. Interactive
//! control remains in the TUI.

use std::path::Path;

use agentmux_core::{AgentmuxConfig, AgentmuxError, error::Result};
#[cfg(feature = "arena")]
use agentmux_ipc::ARENA_PROTOCOL_VERSION;
use clap::Parser;
use serde_json::json;
use tokio::net::UnixStream;

mod cli;
mod daemon;
mod doctor;
mod output;
mod parse;
mod protocol;
mod requests;
mod tui;

use cli::*;
use daemon::*;
use doctor::*;
use output::*;
use parse::*;
use protocol::*;
use requests::*;
use tui::*;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    let Some(command) = cli.command else {
        run_bare_tui_session(&socket_path).await?;
        return Ok(());
    };

    match command {
        Commands::Start(args) => {
            let layout = parse_start_layout(args.providers.as_deref())?;
            run_tui_session_with_startup_panes(&socket_path, layout).await?;
        }
        Commands::Doctor(_) => {
            let report = doctor_report(
                &socket_path,
                &std::env::current_dir().map_err(|error| {
                    AgentmuxError::Internal(format!("failed to resolve cwd: {error}"))
                })?,
            );
            print_doctor_report(&report);
        }
        Commands::Daemon(args) => match args.action {
            DaemonAction::Start => {
                ensure_daemon(&socket_path).await?;
                println!("daemon running ({})", socket_path.display());
            }
            DaemonAction::Stop => stop_daemon(&socket_path)?,
            DaemonAction::Status => {
                // Passive: report not-running rather than auto-starting.
                if daemon_running(&socket_path).await {
                    let response =
                        send_daemon_request(&socket_path, daemon_status_request()).await?;
                    print_response("daemon", response)?;
                } else {
                    println!("daemon: not running ({})", socket_path.display());
                }
            }
        },
        Commands::Project(args) => match args.action {
            ProjectAction::Init { path } => {
                // `project init` is a purely local operation — it creates the
                // `.agentmux/` directory and does not require a running daemon.
                let project_dir = init_project(Path::new(&path))?;
                println!("project initialised at {}", project_dir.display());
                println!("  created: .agentmux/config.toml");
                println!("  hint:    add '.agentmux/' to your .gitignore");
            }
            ProjectAction::Open { path } => {
                println!("project open {path} — not yet implemented");
            }
            ProjectAction::Status => {
                // Local-first: report project state from `.agentmux/` without
                // requiring a running daemon. Daemon connectivity is reported
                // best-effort and never fails the command.
                let cwd = std::env::current_dir().map_err(|error| {
                    AgentmuxError::UserError(format!("cannot resolve current directory: {error}"))
                })?;
                let agentmux_dir = cwd.join(".agentmux");
                if agentmux_dir.is_dir() {
                    println!("project root: {}", cwd.display());
                    let config_path = agentmux_dir.join("config.toml");
                    match AgentmuxConfig::load_from_path(&config_path) {
                        Ok(_) => println!("config:       {} (valid)", config_path.display()),
                        Err(error) => {
                            println!("config:       {} (invalid: {error})", config_path.display())
                        }
                    }
                    match UnixStream::connect(&socket_path).await {
                        Ok(_) => println!("daemon:       running ({})", socket_path.display()),
                        Err(_) => println!("daemon:       not running ({})", socket_path.display()),
                    }
                } else {
                    println!(
                        "project: not initialised (no .agentmux/ in {})",
                        cwd.display()
                    );
                    println!("  run: agentmux project init .");
                }
            }
            ProjectAction::InstallResultProtocol { path, global } => {
                let report = install_result_protocol(Path::new(&path), global)?;
                print_result_protocol_report(&report);
            }
        },
        Commands::Task(args) => match args.action {
            #[cfg(not(feature = "arena"))]
            TaskAction::Run { description, team } => {
                let response =
                    send_daemon_request(&socket_path, task_run_request(description, team)?).await?;
                print_response("task", response)?;
            }
            #[cfg(feature = "arena")]
            TaskAction::Run {
                description,
                team,
                arena,
                base_branch,
            } => {
                if arena.is_some() && !daemon_supports_arena(&socket_path).await? {
                    eprintln!(
                        "Arena unsupported by this daemon; upgrade daemon protocol to {ARENA_PROTOCOL_VERSION} or newer."
                    );
                    return Ok(());
                }
                let response = send_daemon_request(
                    &socket_path,
                    task_run_request(description, team, arena, base_branch)?,
                )
                .await?;
                print_response("task", response)?;
            }
            TaskAction::Status { task_id } => {
                println!("task status {task_id} — not yet implemented")
            }
            TaskAction::Pause { task_id } => println!("task pause {task_id} — not yet implemented"),
            TaskAction::Resume { task_id } => {
                println!("task resume {task_id} — not yet implemented")
            }
            TaskAction::Cancel { task_id } => {
                println!("task cancel {task_id} — not yet implemented")
            }
            TaskAction::Summary { task_id } => {
                println!("task summary {task_id} — not yet implemented")
            }
        },
        Commands::Agent(args) => match args.action {
            AgentAction::Ls => {
                let response = send_daemon_request(&socket_path, agent_ls_request()).await?;
                print_response("agent", response)?;
            }
            AgentAction::Spawn { provider, role } => {
                let response =
                    send_daemon_request(&socket_path, agent_spawn_request(provider, role)?).await?;
                print_response("agent", response)?;
            }
            AgentAction::Stop { agent_id } => {
                let response =
                    send_daemon_request(&socket_path, agent_stop_request(agent_id)).await?;
                print_response("agent", response)?;
            }
            AgentAction::Send {
                inject,
                no_inject,
                agent_id,
                body,
            } => {
                send_message_and_maybe_inject(
                    &socket_path,
                    "agent",
                    agent_send_request(agent_id, body)?,
                    should_inject_message(inject, no_inject),
                )
                .await?;
            }
            AgentAction::Broadcast { to, no_enter, text } => {
                let response = send_daemon_request(
                    &socket_path,
                    agent_broadcast_input_request(to, text, !no_enter)?,
                )
                .await?;
                print_response("agent", response)?;
            }
            AgentAction::Inject {
                message_id,
                agent_id,
            } => {
                let response =
                    send_daemon_request(&socket_path, agent_inject_request(message_id, agent_id))
                        .await?;
                print_response("agent", response)?;
            }
            AgentAction::Focus { agent_id } => {
                let response =
                    send_daemon_request(&socket_path, agent_focus_request(agent_id)).await?;
                print_response("agent", response)?;
            }
            AgentAction::Interrupt { agent_id } => {
                let response =
                    send_daemon_request(&socket_path, agent_interrupt_request(agent_id)).await?;
                print_response("agent", response)?;
            }
            AgentAction::Keys { agent_id, spec } => {
                let response =
                    send_daemon_request(&socket_path, agent_send_keys_request(&agent_id, &spec)?)
                        .await?;
                print_response("agent", response)?;
            }
            AgentAction::SetRole { agent_id, role } => {
                let response =
                    send_daemon_request(&socket_path, agent_set_role_request(agent_id, role)?)
                        .await?;
                print_response("agent", response)?;
            }
        },
        Commands::Sessions(_) => {
            let response = send_daemon_request(&socket_path, sessions_list_request()).await?;
            print_sessions_response(response)?;
        }
        Commands::Message(args) => match args.action {
            MessageAction::List => {
                let response = send_daemon_request(&socket_path, message_list_request()).await?;
                print_response("message", response)?;
            }
            MessageAction::History {
                limit,
                task,
                thread,
                agent,
                kind,
                status,
            } => {
                let response = send_daemon_request(&socket_path, message_list_request()).await?;
                print_message_history_response(
                    response,
                    &MessageHistoryFilter {
                        limit,
                        task,
                        thread,
                        agent,
                        kind,
                        status,
                    },
                )?;
            }
            MessageAction::Show { message_id } => {
                let response =
                    send_daemon_request(&socket_path, message_show_request(message_id)).await?;
                print_response("message", response)?;
            }
            MessageAction::Send {
                inject,
                no_inject,
                to,
                thread,
                kind,
                priority,
                body,
            } => {
                // `--thread <id>` targets the thread itself when no explicit
                // --to is given; fan-out delivery is handled by the daemon's
                // idle-delivery machinery, so skip the single-target manual
                // inject step for thread posts.
                let to = to
                    .unwrap_or_else(|| format!("thread:{}", thread.as_deref().unwrap_or_default()));
                let mut request = message_send_request(to, body, kind, priority)?;
                let is_thread_post = thread.is_some();
                if let Some(thread) = thread {
                    request.payload["thread_id"] = json!(thread);
                }
                send_message_and_maybe_inject(
                    &socket_path,
                    "message",
                    request,
                    should_inject_message(inject, no_inject) && !is_thread_post,
                )
                .await?;
            }
            MessageAction::Inject { message_id } => {
                let response =
                    send_daemon_request(&socket_path, message_inject_request(message_id)).await?;
                print_response("message", response)?;
            }
        },
        Commands::Meeting(args) => match args.action {
            MeetingAction::Open {
                topic,
                participants,
                max_turns,
                kind,
                priority,
                body,
            } => {
                let request =
                    meeting_open_request(topic, participants, max_turns, kind, priority, body)?;
                let response = send_daemon_request(&socket_path, request).await?;
                print_response("meeting", response)?;
            }
            MeetingAction::Close { thread_id } => {
                let response =
                    send_daemon_request(&socket_path, meeting_close_request(thread_id)).await?;
                print_response("meeting", response)?;
            }
            MeetingAction::List => {
                let response = send_daemon_request(&socket_path, meeting_list_request()).await?;
                print_response("meeting", response)?;
            }
        },
        Commands::Context(args) => match args.action {
            ContextAction::Add { title } => {
                let response =
                    send_daemon_request(&socket_path, context_add_request(title)?).await?;
                print_response("context", response)?;
            }
            ContextAction::List => {
                let response = send_daemon_request(&socket_path, context_list_request()).await?;
                print_response("context", response)?;
            }
            ContextAction::Show { context_id } => {
                let response =
                    send_daemon_request(&socket_path, context_show_request(context_id)).await?;
                print_response("context", response)?;
            }
            ContextAction::Search { query } => {
                let response =
                    send_daemon_request(&socket_path, context_search_request(query)?).await?;
                print_response("context", response)?;
            }
            ContextAction::Attach {
                context_id,
                message_id,
            } => {
                let response = send_daemon_request(
                    &socket_path,
                    context_attach_request(context_id, message_id),
                )
                .await?;
                print_response("context", response)?;
            }
            ContextAction::Inject {
                context_id,
                agent_id,
            } => {
                let response =
                    send_daemon_request(&socket_path, context_inject_request(context_id, agent_id))
                        .await?;
                print_response("context", response)?;
            }
            ContextAction::Export { output } => {
                let response =
                    send_daemon_request(&socket_path, context_export_request(output)).await?;
                print_response("context", response)?;
            }
        },
        Commands::Worktree(args) => match args.action {
            WorktreeAction::List => {
                let response = send_daemon_request(&socket_path, worktree_list_request()).await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Diff { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_diff_request(worktree_id)).await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Test { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_test_request(worktree_id)).await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Promote { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_promote_request(worktree_id))
                        .await?;
                print_response("worktree", response)?;
            }
            WorktreeAction::Archive { worktree_id } => {
                let response =
                    send_daemon_request(&socket_path, worktree_archive_request(worktree_id))
                        .await?;
                print_response("worktree", response)?;
            }
        },
        Commands::Approval(args) => match args.action {
            ApprovalAction::List => {
                let response = send_daemon_request(&socket_path, approval_list_request()).await?;
                print_response("approval", response)?;
            }
            ApprovalAction::Approve { approval_id } => {
                let response =
                    send_daemon_request(&socket_path, approval_approve_request(approval_id))
                        .await?;
                print_response("approval", response)?;
            }
            ApprovalAction::Reject { approval_id } => {
                let response =
                    send_daemon_request(&socket_path, approval_reject_request(approval_id)).await?;
                print_response("approval", response)?;
            }
        },
        Commands::Attach(args) => {
            run_tui_session(&socket_path, Some(args.target)).await?;
        }
        Commands::Layout(args) => match args.action {
            LayoutAction::Save { name } => {
                let response =
                    send_daemon_request(&socket_path, layout_save_request(name)?).await?;
                print_response("layout", response)?;
            }
            LayoutAction::Load { name } => {
                let response = send_daemon_request(&socket_path, layout_load_request(name)).await?;
                print_response("layout", response)?;
            }
            LayoutAction::List => {
                let response = send_daemon_request(&socket_path, layout_list_request()).await?;
                print_response("layout", response)?;
            }
        },
    };

    Ok(())
}

#[cfg(test)]
mod tests;
