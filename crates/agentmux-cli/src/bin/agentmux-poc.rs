//! Phase 0 PTY host PoC.
//!
//! Manual verification path from `docs/spec/12_implementation_roadmap.md`:
//! `agentmux-poc codex` and `agentmux-poc claude`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use agentmux_core::{AgentmuxError, error::Result};
use agentmux_pty::{CTRL_C, PtyHandle, PtySpawnSpec, TerminalSize, bracketed_paste_bytes};
use clap::{Parser, ValueEnum};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};

#[derive(Debug, Parser)]
#[command(
    name = "agentmux-poc",
    about = "Run Codex or Claude Code inside a PTY and bridge it to this terminal"
)]
struct PocCli {
    /// Agent provider to spawn in the PTY.
    provider: PocProvider,

    /// Working directory for the spawned agent.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,

    /// Prompt text to send as bracketed paste after startup.
    #[arg(long, conflicts_with = "paste_file")]
    paste: Option<String>,

    /// Prompt file to send as bracketed paste after startup.
    #[arg(long, conflicts_with = "paste")]
    paste_file: Option<PathBuf>,

    /// Append Enter after --paste or --paste-file.
    #[arg(long)]
    enter: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PocProvider {
    Codex,
    Claude,
}

impl PocProvider {
    fn command(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        enable_raw_mode().map_err(|error| AgentmuxError::PtyError(error.to_string()))?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn main() -> Result<()> {
    let cli = PocCli::parse();
    run(cli)
}

fn run(cli: PocCli) -> Result<()> {
    let mut handle = PtyHandle::spawn(spawn_spec(cli.provider, cli.cwd)?)?;
    let mut reader = handle.try_clone_reader()?;
    let _raw_mode = RawModeGuard::enable()?;

    let output_thread = thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let mut buffer = [0_u8; 8192];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(bytes_read) => {
                    if stdout.write_all(&buffer[..bytes_read]).is_err() {
                        break;
                    }
                    if stdout.flush().is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });

    if let Some(bytes) = paste_bytes(cli.paste, cli.paste_file, cli.enter)? {
        handle.write_bytes(&bytes)?;
    }

    while handle.try_wait()?.is_none() {
        if !event::poll(Duration::from_millis(50))
            .map_err(|error| AgentmuxError::PtyError(error.to_string()))?
        {
            continue;
        }

        match event::read().map_err(|error| AgentmuxError::PtyError(error.to_string()))? {
            Event::Key(key) => {
                if let Some(bytes) = key_event_bytes(key) {
                    handle.write_bytes(&bytes)?;
                }
            }
            Event::Resize(cols, rows) => {
                handle.resize(TerminalSize { rows, cols })?;
            }
            _ => {}
        }
    }

    let _ = output_thread.join();
    Ok(())
}

fn spawn_spec(provider: PocProvider, cwd: PathBuf) -> Result<PtySpawnSpec> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let mut env = BTreeMap::new();
    env.insert("TERM".to_string(), "xterm-256color".to_string());

    Ok(PtySpawnSpec {
        command: provider.command().to_string(),
        args: Vec::new(),
        cwd,
        env,
        size: TerminalSize { rows, cols },
    })
}

fn paste_bytes(
    paste: Option<String>,
    paste_file: Option<PathBuf>,
    enter: bool,
) -> Result<Option<Vec<u8>>> {
    let Some(text) = paste_text(paste, paste_file)? else {
        return Ok(None);
    };

    let mut bytes = bracketed_paste_bytes(&text);
    if enter {
        bytes.push(b'\n');
    }
    Ok(Some(bytes))
}

fn paste_text(paste: Option<String>, paste_file: Option<PathBuf>) -> Result<Option<String>> {
    if let Some(text) = paste {
        return Ok(Some(text));
    }

    paste_file
        .map(fs::read_to_string)
        .transpose()
        .map_err(|error| AgentmuxError::UserError(error.to_string()))
}

fn key_event_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => Some(CTRL_C.to_vec()),
            KeyCode::Char(ch) if ch.is_ascii_alphabetic() => {
                Some(vec![ch.to_ascii_lowercase() as u8 - b'a' + 1])
            }
            _ => None,
        };
    }

    match key.code {
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::Esc => Some(b"\x1b".to_vec()),
        KeyCode::Char(ch) => Some(ch.to_string().into_bytes()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn provider_maps_to_expected_command() {
        assert_eq!(PocProvider::Codex.command(), "codex");
        assert_eq!(PocProvider::Claude.command(), "claude");
    }

    #[test]
    fn paste_option_uses_bracketed_paste_without_enter_by_default() {
        let bytes = paste_bytes(Some("hello\nworld".to_string()), None, false)
            .expect("paste bytes")
            .expect("some bytes");

        assert_eq!(bytes, b"\x1b[200~hello\nworld\x1b[201~");
    }

    #[test]
    fn paste_option_can_append_enter() {
        let bytes = paste_bytes(Some("hello".to_string()), None, true)
            .expect("paste bytes")
            .expect("some bytes");

        assert_eq!(bytes, b"\x1b[200~hello\x1b[201~\n");
    }

    #[test]
    fn absent_paste_has_no_startup_input() {
        assert!(
            paste_bytes(None, None, false)
                .expect("paste bytes")
                .is_none()
        );
    }

    #[test]
    fn ctrl_c_key_event_maps_to_interrupt_byte() {
        let bytes =
            key_event_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL)).expect("ctrl-c bytes");

        assert_eq!(bytes, CTRL_C);
    }

    #[test]
    fn arrow_key_event_maps_to_escape_sequence() {
        let bytes =
            key_event_bytes(key(KeyCode::Right, KeyModifiers::empty())).expect("right arrow bytes");

        assert_eq!(bytes, b"\x1b[C");
    }
}
