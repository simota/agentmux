//! Panic hook support for restoring terminal state.

use std::fmt;
use std::io::{self, Write};
use std::panic;

use crossterm::{
    cursor,
    event::{DisableBracketedPaste, DisableMouseCapture},
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};

/// Error returned when one or more terminal restore actions fail.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct TerminalRestoreError {
    raw_mode: Option<String>,
    screen: Option<String>,
}

impl TerminalRestoreError {
    fn from_parts(raw_mode: Option<io::Error>, screen: Option<io::Error>) -> Option<Self> {
        if raw_mode.is_none() && screen.is_none() {
            return None;
        }

        Some(Self {
            raw_mode: raw_mode.map(|error| error.to_string()),
            screen: screen.map(|error| error.to_string()),
        })
    }

    pub fn raw_mode_error(&self) -> Option<&str> {
        self.raw_mode.as_deref()
    }

    pub fn screen_error(&self) -> Option<&str> {
        self.screen.as_deref()
    }
}

impl fmt::Display for TerminalRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.raw_mode.as_deref(), self.screen.as_deref()) {
            (Some(raw_mode), Some(screen)) => {
                write!(
                    formatter,
                    "failed to restore terminal raw mode ({raw_mode}) and screen state ({screen})"
                )
            }
            (Some(raw_mode), None) => {
                write!(formatter, "failed to restore terminal raw mode: {raw_mode}")
            }
            (None, Some(screen)) => write!(
                formatter,
                "failed to restore terminal screen state: {screen}"
            ),
            (None, None) => write!(formatter, "terminal restore failed"),
        }
    }
}

impl std::error::Error for TerminalRestoreError {}

/// Restores the process terminal after TUI shutdown or panic.
pub fn restore_terminal() -> Result<(), TerminalRestoreError> {
    restore_terminal_with(io::stderr().lock(), disable_raw_mode)
}

fn restore_terminal_with<W, F>(
    mut writer: W,
    disable_raw_mode_fn: F,
) -> Result<(), TerminalRestoreError>
where
    W: Write,
    F: FnOnce() -> io::Result<()>,
{
    let raw_mode_error = disable_raw_mode_fn().err();
    // Keep this sequence identical to `CrosstermTerminalIo::exit`: mouse
    // capture must be disabled too, or a panic during copy mode leaves the
    // host terminal emitting mouse-report escape sequences.
    let screen_error = execute!(
        writer,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    )
    .err();

    TerminalRestoreError::from_parts(raw_mode_error, screen_error).map_or(Ok(()), Err)
}

/// Installs a panic hook that restores terminal state before delegating to the previous hook.
pub fn install_terminal_restore_panic_hook() {
    install_terminal_restore_panic_hook_with(|| {
        let _ = restore_terminal();
    });
}

#[doc(hidden)]
pub fn install_terminal_restore_panic_hook_with<F>(restore: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let previous_hook = panic::take_hook();

    panic::set_hook(Box::new(move |panic_info| {
        restore();
        previous_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    static PANIC_HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn restore_terminal_writes_screen_restore_commands_even_when_raw_mode_fails() {
        let mut output = Vec::new();

        let result = restore_terminal_with(&mut output, || {
            Err(io::Error::other("raw mode unavailable"))
        });

        let error = result.expect_err("raw mode failure should be reported");
        assert_eq!(error.raw_mode_error(), Some("raw mode unavailable"));
        assert!(error.screen_error().is_none());
        // Mouse capture and bracketed paste are disabled alongside the
        // screen/cursor restore — the same sequence as the regular exit path —
        // so a panic during copy mode never leaves the host terminal in
        // mouse-reporting or paste-reporting mode.
        assert_eq!(
            output,
            b"\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?2004l\x1b[?1049l\x1b[?25h"
        );
    }

    #[test]
    fn installed_panic_hook_restores_terminal_before_previous_hook() {
        let _guard = PANIC_HOOK_TEST_LOCK.lock().expect("panic hook test lock");
        let previous_hook = panic::take_hook();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_hook = Arc::clone(&calls);
        let order = Arc::new(Mutex::new(Vec::new()));
        let order_for_previous = Arc::clone(&order);
        let order_for_restore = Arc::clone(&order);

        panic::set_hook(Box::new(move |_| {
            order_for_previous
                .lock()
                .expect("panic hook order lock")
                .push("previous");
        }));
        install_terminal_restore_panic_hook_with(move || {
            calls_for_hook.fetch_add(1, Ordering::SeqCst);
            order_for_restore
                .lock()
                .expect("panic hook order lock")
                .push("restore");
        });

        let panic_result = panic::catch_unwind(|| panic!("exercise panic hook"));

        let _installed_hook = panic::take_hook();
        panic::set_hook(previous_hook);

        assert!(panic_result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *order.lock().expect("panic hook order lock"),
            vec!["restore", "previous"]
        );
    }
}
