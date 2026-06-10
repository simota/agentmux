//! Thin terminal I/O boundary for the interactive TUI.
//!
//! The run loop owns pure state; this module owns crossterm/ratatui side effects
//! such as raw mode, alternate screen, drawing, and event polling.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::{
    cursor,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

use crate::panic_hook::install_terminal_restore_panic_hook;

/// Side-effect boundary used by the TUI run loop.
pub trait TerminalIo {
    /// Enter raw mode and switch to the alternate screen.
    fn enter(&mut self) -> io::Result<()>;

    /// Restore cooked mode, leave the alternate screen, and show the cursor.
    fn exit(&mut self) -> io::Result<()>;

    /// Draw a single frame.
    fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>);

    /// Poll for one terminal event without blocking longer than `timeout`.
    fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<Event>>;

    /// Enable or disable terminal mouse capture for app-managed pointer gestures.
    fn set_mouse_capture(&mut self, _enabled: bool) -> io::Result<()> {
        Ok(())
    }

    /// Copy text through OSC52 when the host terminal supports it.
    fn copy_to_clipboard(&mut self, _text: &str) -> io::Result<()> {
        Ok(())
    }
}

/// Real crossterm + ratatui implementation for interactive sessions.
#[derive(Debug)]
pub struct CrosstermTerminalIo<W: Write> {
    terminal: Terminal<CrosstermBackend<W>>,
}

impl<W: Write> CrosstermTerminalIo<W> {
    pub fn new(writer: W) -> io::Result<Self> {
        let backend = CrosstermBackend::new(writer);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    pub fn terminal(&self) -> &Terminal<CrosstermBackend<W>> {
        &self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<W>> {
        &mut self.terminal
    }
}

impl<W: Write> TerminalIo for CrosstermTerminalIo<W> {
    fn enter(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        if let Err(error) = execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            cursor::Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        if let Err(error) = self.terminal.clear() {
            let _ = execute!(
                self.terminal.backend_mut(),
                DisableBracketedPaste,
                LeaveAlternateScreen,
                cursor::Show
            );
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(())
    }

    fn exit(&mut self) -> io::Result<()> {
        let raw_mode_error = disable_raw_mode().err();
        let screen_error = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            cursor::Show
        )
        .err();

        match (raw_mode_error, screen_error) {
            (Some(raw), Some(screen)) => Err(io::Error::other(format!(
                "failed to restore terminal raw mode ({raw}) and screen state ({screen})"
            ))),
            (Some(raw), None) => Err(raw),
            (None, Some(screen)) => Err(screen),
            (None, None) => Ok(()),
        }
    }

    fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.terminal.draw(render).map(|_| ())
    }

    fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
        if event::poll(timeout)? {
            event::read().map(Some)
        } else {
            Ok(None)
        }
    }

    fn set_mouse_capture(&mut self, enabled: bool) -> io::Result<()> {
        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)
        }
    }

    fn copy_to_clipboard(&mut self, text: &str) -> io::Result<()> {
        let encoded = base64_encode(text.as_bytes());
        write!(self.terminal.backend_mut(), "\x1b]52;c;{encoded}\x07")?;
        self.terminal.backend_mut().flush()
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let value = (u32::from(first) << 16) | (u32::from(second) << 8) | u32::from(third);

        encoded.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[((value >> 6) & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(value & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
    }

    encoded
}

/// RAII guard for an active terminal session.
#[derive(Debug)]
pub struct TerminalSession<T: TerminalIo> {
    io: T,
    active: bool,
}

impl<T: TerminalIo> TerminalSession<T> {
    /// Installs the terminal restore panic hook and enters raw alternate-screen mode.
    pub fn enter(io: T) -> io::Result<Self> {
        Self::enter_with_panic_hook(io, install_terminal_restore_panic_hook)
    }

    #[doc(hidden)]
    pub fn enter_with_panic_hook<F>(mut io: T, install_panic_hook: F) -> io::Result<Self>
    where
        F: FnOnce(),
    {
        install_panic_hook();
        io.enter()?;
        Ok(Self { io, active: true })
    }

    pub fn io(&self) -> &T {
        &self.io
    }

    pub fn io_mut(&mut self) -> &mut T {
        &mut self.io
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        if self.active {
            self.active = false;
            self.io.exit()?;
        }
        Ok(())
    }
}

impl<T: TerminalIo> Drop for TerminalSession<T> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.io.exit();
            self.active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Call {
        Enter,
        Exit,
        Draw,
        Poll(Duration),
        PanicHook,
    }

    #[derive(Clone, Debug, Default)]
    struct FakeTerminalIo {
        calls: Rc<RefCell<Vec<Call>>>,
        enter_error: Option<&'static str>,
        exit_error: Option<&'static str>,
    }

    impl FakeTerminalIo {
        fn with_calls(calls: Rc<RefCell<Vec<Call>>>) -> Self {
            Self {
                calls,
                enter_error: None,
                exit_error: None,
            }
        }

        fn failing_enter(calls: Rc<RefCell<Vec<Call>>>) -> Self {
            Self {
                calls,
                enter_error: Some("enter failed"),
                exit_error: None,
            }
        }
    }

    impl TerminalIo for FakeTerminalIo {
        fn enter(&mut self) -> io::Result<()> {
            self.calls.borrow_mut().push(Call::Enter);
            self.enter_error
                .map(|message| Err(io::Error::other(message)))
                .unwrap_or(Ok(()))
        }

        fn exit(&mut self) -> io::Result<()> {
            self.calls.borrow_mut().push(Call::Exit);
            self.exit_error
                .map(|message| Err(io::Error::other(message)))
                .unwrap_or(Ok(()))
        }

        fn draw<F>(&mut self, _render: F) -> io::Result<()>
        where
            F: FnOnce(&mut Frame<'_>),
        {
            self.calls.borrow_mut().push(Call::Draw);
            Ok(())
        }

        fn poll_event(&mut self, timeout: Duration) -> io::Result<Option<Event>> {
            self.calls.borrow_mut().push(Call::Poll(timeout));
            Ok(None)
        }
    }

    #[test]
    fn session_installs_panic_hook_before_entering_terminal() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let calls_for_hook = Rc::clone(&calls);

        let session = TerminalSession::enter_with_panic_hook(
            FakeTerminalIo::with_calls(Rc::clone(&calls)),
            move || calls_for_hook.borrow_mut().push(Call::PanicHook),
        )
        .expect("session should enter");

        assert_eq!(*calls.borrow(), vec![Call::PanicHook, Call::Enter]);
        drop(session);
    }

    #[test]
    fn session_restores_terminal_on_drop() {
        let calls = Rc::new(RefCell::new(Vec::new()));

        {
            let _session = TerminalSession::enter_with_panic_hook(
                FakeTerminalIo::with_calls(Rc::clone(&calls)),
                || {},
            )
            .expect("session should enter");
        }

        assert_eq!(*calls.borrow(), vec![Call::Enter, Call::Exit]);
    }

    #[test]
    fn failed_enter_does_not_create_active_session_or_exit() {
        let calls = Rc::new(RefCell::new(Vec::new()));

        let error = TerminalSession::enter_with_panic_hook(
            FakeTerminalIo::failing_enter(Rc::clone(&calls)),
            || {},
        )
        .expect_err("enter failure should be returned");

        assert_eq!(error.to_string(), "enter failed");
        assert_eq!(*calls.borrow(), vec![Call::Enter]);
    }

    #[test]
    fn explicit_shutdown_restores_once() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut session = TerminalSession::enter_with_panic_hook(
            FakeTerminalIo::with_calls(Rc::clone(&calls)),
            || {},
        )
        .expect("session should enter");

        session.shutdown().expect("shutdown should restore");

        assert_eq!(*calls.borrow(), vec![Call::Enter, Call::Exit]);
    }

    #[test]
    fn terminal_io_exposes_nonblocking_event_poll_boundary() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut io = FakeTerminalIo::with_calls(Rc::clone(&calls));
        let timeout = Duration::from_millis(25);

        let event = io.poll_event(timeout).expect("poll should not fail");

        assert!(event.is_none());
        assert_eq!(*calls.borrow(), vec![Call::Poll(timeout)]);
    }

    #[test]
    fn base64_encoder_handles_osc52_payloads() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode("hello\n".as_bytes()), "aGVsbG8K");
    }
}
