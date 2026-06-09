//! Parsing and normalization helpers for CLI arguments and wire values.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agentmux_core::{AgentmuxError, error::Result};
use agentmux_tui::layout::{LayoutChild, LayoutNode, PaneId, SplitDirection};
use agentmux_tui::state::AgentProviderChoice;

use crate::StartupPaneChoice;

static AGENT_NAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn unique_agent_name(prefix: &str) -> String {
    let prefix = sanitize_agent_name_prefix(prefix);
    let sequence = AGENT_NAME_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let entropy = nanos ^ ((std::process::id() as u64) << 32) ^ sequence;
    format!("{prefix}-{}", base36_suffix(entropy, 6))
}

pub(crate) fn sanitize_agent_name_prefix(prefix: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;
    for ch in prefix.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator && !output.is_empty() {
            output.push('-');
            last_was_separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "agent".to_string()
    } else {
        output
    }
}

pub(crate) fn base36_suffix(mut value: u64, len: usize) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut chars = vec!['0'; len];
    for slot in chars.iter_mut().rev() {
        *slot = DIGITS[(value % 36) as usize] as char;
        value /= 36;
    }
    chars.into_iter().collect()
}

/// A node in the parsed startup layout tree.
///
/// Leaves carry a [`StartupPaneChoice`] (a provider to spawn or the conversation
/// list). The actual `PaneId` is only known after the daemon spawns each agent, so
/// resolution to engine [`LayoutNode`]s happens at TUI bootstrap via
/// [`StartupLayout::resolve_root`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StartupLayoutNode {
    Leaf(StartupPaneChoice),
    Split {
        direction: SplitDirection,
        children: Vec<StartupChild>,
    },
}

/// A child slot inside a [`StartupLayoutNode::Split`], carrying its size ratio.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StartupChild {
    pub node: StartupLayoutNode,
    pub size: Option<u16>,
}

/// Result of parsing the `agentmux start "<spec>"` layout argument.
///
/// `root` carries the full split tree (Phase 2: `()` nesting and `:N` sizes).
/// `panes` is the depth-first leaf order, used to drive deterministic agent spawn
/// requests; the spawn order matches the order leaves appear in the spec.
///
/// `SplitDirection` follows the engine naming in [`agentmux_tui::layout::SplitDirection`],
/// NOT the spec notation. The spec notation is intentionally inverted relative to
/// the engine: spec `|` (left-right) maps to engine `Vertical`, and spec `―`
/// (top-bottom) maps to engine `Horizontal`. See [`vbar`]/[`hbar`] mapping below.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StartupLayout {
    pub root: StartupLayoutNode,
    pub panes: Vec<StartupPaneChoice>,
}

impl StartupLayout {
    /// An empty layout (no panes): preserves the picker behavior at startup.
    fn empty() -> Self {
        Self {
            root: StartupLayoutNode::Split {
                direction: SplitDirection::Vertical,
                children: Vec::new(),
            },
            panes: Vec::new(),
        }
    }

    /// Build the concrete engine [`LayoutNode`] tree by substituting each leaf's
    /// `StartupPaneChoice` with a resolved `PaneId`. `resolve` is called for each
    /// leaf in depth-first order — the same order as [`StartupLayout::panes`] — so
    /// callers can hand back spawned agent ids (and the conversation-list id) in
    /// sequence.
    pub fn resolve_root(
        &self,
        mut resolve: impl FnMut(StartupPaneChoice) -> PaneId,
    ) -> LayoutNode {
        resolve_node(&self.root, &mut resolve)
    }
}

fn resolve_node(
    node: &StartupLayoutNode,
    resolve: &mut impl FnMut(StartupPaneChoice) -> PaneId,
) -> LayoutNode {
    match node {
        StartupLayoutNode::Leaf(choice) => LayoutNode::Leaf {
            pane_id: resolve(*choice),
        },
        StartupLayoutNode::Split {
            direction,
            children,
        } => LayoutNode::Split {
            direction: *direction,
            children: children
                .iter()
                .map(|child| LayoutChild::sized(resolve_node(&child.node, resolve), child.size))
                .collect(),
        },
    }
}

/// Top-bottom split character `―` (U+2015, HORIZONTAL BAR).
const TOP_BOTTOM_BAR: char = '\u{2015}';

/// Lexer tokens for the start-layout DSL.
#[derive(Clone, Debug, Eq, PartialEq)]
enum LayoutToken {
    /// `|` or `/` — left-right split (engine `Vertical`).
    VBar,
    /// `―` (U+2015) or a standalone ` - ` — top-bottom split (engine `Horizontal`).
    HBar,
    /// `,` — legacy left-right alias (must not mix with `|`/`―`).
    Comma,
    /// `:` size separator.
    Colon,
    LParen,
    RParen,
    /// A pane identifier (provider alias / `messages`), preserving word-internal `-`.
    Ident(String),
    /// A size integer following `:`.
    Number(u16),
}

/// Tokenize the raw spec. Identifiers greedily consume alphanumerics plus
/// word-internal `-`/`_` so `claude-code` is one ident, while a standalone `-`
/// (whitespace/separator on both sides) lexes as an [`LayoutToken::HBar`].
fn tokenize_layout(raw: &str) -> Result<Vec<LayoutToken>> {
    let chars: Vec<char> = raw.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        match ch {
            '|' | '/' => {
                tokens.push(LayoutToken::VBar);
                index += 1;
            }
            TOP_BOTTOM_BAR => {
                tokens.push(LayoutToken::HBar);
                index += 1;
            }
            ',' => {
                tokens.push(LayoutToken::Comma);
                index += 1;
            }
            ':' => {
                tokens.push(LayoutToken::Colon);
                index += 1;
            }
            '(' => {
                tokens.push(LayoutToken::LParen);
                index += 1;
            }
            ')' => {
                tokens.push(LayoutToken::RParen);
                index += 1;
            }
            '-' => {
                // A standalone `-` is HBar. A `-` that begins an identifier run
                // would have been consumed as part of the ident below; reaching
                // here means it is surrounded by whitespace or separators.
                tokens.push(LayoutToken::HBar);
                index += 1;
            }
            c if c.is_ascii_digit() => {
                let start = index;
                while index < chars.len() && chars[index].is_ascii_digit() {
                    index += 1;
                }
                let text: String = chars[start..index].iter().collect();
                let value: u16 = text.parse().map_err(|_| {
                    AgentmuxError::UserError(format!("invalid pane size '{text}' (expected 1..=100)"))
                })?;
                tokens.push(LayoutToken::Number(value));
            }
            c if is_ident_start(c) => {
                let start = index;
                index += 1;
                while index < chars.len() && is_ident_continue(&chars, index) {
                    index += 1;
                }
                let text: String = chars[start..index].iter().collect();
                tokens.push(LayoutToken::Ident(text));
            }
            other => {
                return Err(AgentmuxError::UserError(format!(
                    "unexpected character '{other}' in layout spec"
                )));
            }
        }
    }
    Ok(tokens)
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// `-` and `_` continue an identifier only when not at a word boundary: a `-`
/// continues the ident when the previous char was an ident char and the next char
/// is also an ident char (so `claude-code` stays one ident, but `agy - codex`
/// breaks). `_` always continues.
fn is_ident_continue(chars: &[char], index: usize) -> bool {
    let ch = chars[index];
    if ch.is_ascii_alphanumeric() || ch == '_' {
        return true;
    }
    if ch == '-' {
        let prev_is_ident = index
            .checked_sub(1)
            .map(|i| chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '-')
            .unwrap_or(false);
        let next_is_ident = chars
            .get(index + 1)
            .map(|c| c.is_ascii_alphanumeric() || *c == '_')
            .unwrap_or(false);
        return prev_is_ident && next_is_ident;
    }
    false
}

/// Recursive-descent parser over [`LayoutToken`]s.
struct LayoutParser<'a> {
    tokens: &'a [LayoutToken],
    pos: usize,
}

impl<'a> LayoutParser<'a> {
    fn new(tokens: &'a [LayoutToken]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&LayoutToken> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&LayoutToken> {
        let token = self.tokens.get(self.pos);
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    /// `lr_expr = tb_expr { vbar tb_expr }` — left-right, weakest, left-assoc.
    ///
    /// Each `tb_expr` is parsed as a sized child so a `:N` decorating the whole
    /// top-bottom group (or a bare pane) attaches to the left-right slot.
    fn parse_lr(&mut self) -> Result<StartupChild> {
        let first = self.parse_tb()?;
        if !matches!(self.peek(), Some(LayoutToken::VBar)) {
            return Ok(first);
        }
        let mut children = vec![first];
        while matches!(self.peek(), Some(LayoutToken::VBar)) {
            self.bump();
            children.push(self.parse_tb()?);
        }
        // spec `|` (left-right) -> engine `Vertical`.
        Ok(StartupChild {
            node: StartupLayoutNode::Split {
                direction: SplitDirection::Vertical,
                children,
            },
            size: None,
        })
    }

    /// `tb_expr = primary { hbar primary }` — top-bottom, binds tighter than `|`.
    fn parse_tb(&mut self) -> Result<StartupChild> {
        let first = self.parse_sized()?;
        if !matches!(self.peek(), Some(LayoutToken::HBar)) {
            return Ok(first);
        }
        let mut children = vec![first];
        while matches!(self.peek(), Some(LayoutToken::HBar)) {
            self.bump();
            children.push(self.parse_sized()?);
        }
        // spec `―` (top-bottom) -> engine `Horizontal`.
        Ok(StartupChild {
            node: StartupLayoutNode::Split {
                direction: SplitDirection::Horizontal,
                children,
            },
            size: None,
        })
    }

    /// `sized_pane = ( pane | '(' lr_expr ')' ) [ ':' number ]`.
    fn parse_sized(&mut self) -> Result<StartupChild> {
        let node = match self.peek() {
            Some(LayoutToken::LParen) => {
                self.bump();
                if matches!(self.peek(), Some(LayoutToken::RParen)) {
                    return Err(AgentmuxError::UserError(
                        "empty group '()' is not a valid layout".to_string(),
                    ));
                }
                let inner = self.parse_lr()?;
                match self.bump() {
                    // The grouped expression's own size is set by the trailing
                    // `:N` below; discard the inner slot's (always-None) size.
                    Some(LayoutToken::RParen) => inner.node,
                    _ => {
                        return Err(AgentmuxError::UserError(
                            "unbalanced parentheses in layout spec".to_string(),
                        ));
                    }
                }
            }
            Some(LayoutToken::Ident(name)) => {
                let name = name.clone();
                self.bump();
                StartupLayoutNode::Leaf(parse_start_pane_choice(&name)?)
            }
            Some(LayoutToken::RParen) => {
                return Err(AgentmuxError::UserError(
                    "unexpected ')' in layout spec".to_string(),
                ));
            }
            Some(token) => {
                return Err(AgentmuxError::UserError(format!(
                    "expected a pane name or '(' but found {}",
                    describe_token(token)
                )));
            }
            None => {
                return Err(AgentmuxError::UserError(
                    "expected a pane name; got only separators".to_string(),
                ));
            }
        };

        let size = if matches!(self.peek(), Some(LayoutToken::Colon)) {
            self.bump();
            match self.bump() {
                Some(LayoutToken::Number(0)) => {
                    return Err(AgentmuxError::UserError(
                        "pane size must be at least 1 (':0' is not allowed)".to_string(),
                    ));
                }
                Some(LayoutToken::Number(value)) => Some(*value),
                _ => {
                    return Err(AgentmuxError::UserError(
                        "expected a number after ':' for the pane size".to_string(),
                    ));
                }
            }
        } else {
            None
        };

        Ok(StartupChild { node, size })
    }
}

fn describe_token(token: &LayoutToken) -> String {
    match token {
        LayoutToken::VBar => "'|'".to_string(),
        LayoutToken::HBar => "'―'".to_string(),
        LayoutToken::Comma => "','".to_string(),
        LayoutToken::Colon => "':'".to_string(),
        LayoutToken::LParen => "'('".to_string(),
        LayoutToken::RParen => "')'".to_string(),
        LayoutToken::Ident(name) => format!("'{name}'"),
        LayoutToken::Number(value) => format!("'{value}'"),
    }
}

/// Validate that every `Split`'s sized children do not exceed 100% in aggregate.
fn validate_sizes(node: &StartupLayoutNode) -> Result<()> {
    if let StartupLayoutNode::Split { children, .. } = node {
        let total: u32 = children
            .iter()
            .filter_map(|child| child.size)
            .map(u32::from)
            .sum();
        if total > 100 {
            return Err(AgentmuxError::UserError(format!(
                "pane sizes in a split sum to {total}%, which exceeds 100%"
            )));
        }
        for child in children {
            validate_sizes(&child.node)?;
        }
    }
    Ok(())
}

/// Collect leaf choices in depth-first order to drive deterministic spawning.
fn collect_choices(node: &StartupLayoutNode, out: &mut Vec<StartupPaneChoice>) {
    match node {
        StartupLayoutNode::Leaf(choice) => out.push(*choice),
        StartupLayoutNode::Split { children, .. } => {
            for child in children {
                collect_choices(&child.node, out);
            }
        }
    }
}

/// Parse the `agentmux start "<spec>"` layout argument (Phase 2: nesting + sizes).
///
/// Grammar (left-associative, `|` weaker than `―`, both flattened N-ary):
/// - `|` (U+007C) / `/`         -> left-right split  -> engine `Vertical`
/// - `―` (U+2015) / standalone `-` -> top-bottom split -> engine `Horizontal`
/// - `,` (legacy)               -> left-right; must not mix with `|`/`―`
/// - `()`                       -> grouping / nesting
/// - `name:N`                   -> size ratio within the enclosing split
/// - no separator / empty       -> empty layout (picker behavior preserved)
///
/// Mixing `|`-family and `―`-family at the same parenthesized level without a
/// group is rejected; wrap one side in `()`.
pub(crate) fn parse_start_layout(raw: Option<&str>) -> Result<StartupLayout> {
    let Some(raw) = raw else {
        return Ok(StartupLayout::empty());
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(StartupLayout::empty());
    }

    let tokens = tokenize_layout(raw)?;
    if tokens.is_empty() {
        return Ok(StartupLayout::empty());
    }

    // Legacy comma lists are left-right and must not mix with `|`/`―`. Handle them
    // up front so the recursive parser only deals with the modern operators.
    let has_comma = tokens.contains(&LayoutToken::Comma);
    if has_comma {
        return parse_comma_list(&tokens, raw);
    }

    let mut parser = LayoutParser::new(&tokens);
    let root = parser.parse_lr()?.node;
    if !parser.at_end() {
        // The precedence grammar (`|` weakest, `―` tighter, both left-assoc)
        // resolves every operator mix without ambiguity, so reaching here means a
        // stray token the grammar cannot place — most often an unbalanced ')'.
        // Surface the grouping hint so users know they can wrap one side in `()`.
        return Err(AgentmuxError::UserError(
            "unexpected token after a complete layout expression; check your \
             parentheses and wrap one side in '()' if mixing '|' and '―', \
             e.g. (a | b) ― c"
                .to_string(),
        ));
    }

    validate_sizes(&root)?;

    let mut panes = Vec::new();
    collect_choices(&root, &mut panes);
    if panes.is_empty() {
        return Err(AgentmuxError::UserError(format!(
            "expected a pane name; got only separators in '{raw}'"
        )));
    }

    Ok(StartupLayout { root, panes })
}

/// Parse a legacy comma-separated list. `,` is equivalent to `|` (left-right ->
/// engine `Vertical`). Mixing with `|`/`―` is a hard error to preserve the
/// historical contract.
fn parse_comma_list(tokens: &[LayoutToken], raw: &str) -> Result<StartupLayout> {
    if tokens
        .iter()
        .any(|token| matches!(token, LayoutToken::VBar | LayoutToken::HBar))
    {
        return Err(AgentmuxError::UserError(
            "legacy comma list cannot be mixed with '|'/'―' splitters; use one style \
             ('|' is the modern equivalent of ',')"
                .to_string(),
        ));
    }
    if tokens
        .iter()
        .any(|token| matches!(token, LayoutToken::LParen | LayoutToken::RParen))
    {
        return Err(AgentmuxError::UserError(
            "legacy comma list cannot be combined with '()' grouping; use '|' instead of ','"
                .to_string(),
        ));
    }

    let mut children = Vec::new();
    let mut iter = tokens.iter().peekable();
    loop {
        match iter.next() {
            Some(LayoutToken::Ident(name)) => {
                let choice = parse_start_pane_choice(name)?;
                let size = if matches!(iter.peek(), Some(LayoutToken::Colon)) {
                    iter.next();
                    match iter.next() {
                        Some(LayoutToken::Number(0)) => {
                            return Err(AgentmuxError::UserError(
                                "pane size must be at least 1 (':0' is not allowed)".to_string(),
                            ));
                        }
                        Some(LayoutToken::Number(value)) => Some(*value),
                        _ => {
                            return Err(AgentmuxError::UserError(
                                "expected a number after ':' for the pane size".to_string(),
                            ));
                        }
                    }
                } else {
                    None
                };
                children.push(StartupChild {
                    node: StartupLayoutNode::Leaf(choice),
                    size,
                });
                match iter.next() {
                    Some(LayoutToken::Comma) | None => {}
                    Some(other) => {
                        return Err(AgentmuxError::UserError(format!(
                            "unexpected {} in comma list",
                            describe_token(other)
                        )));
                    }
                }
            }
            Some(LayoutToken::Comma) => {
                return Err(AgentmuxError::UserError(format!(
                    "expected a pane name; got only separators in '{raw}'"
                )));
            }
            Some(other) => {
                return Err(AgentmuxError::UserError(format!(
                    "unexpected {} in comma list",
                    describe_token(other)
                )));
            }
            None => break,
        }
    }

    if children.is_empty() {
        return Err(AgentmuxError::UserError(format!(
            "expected a pane name; got only separators in '{raw}'"
        )));
    }

    let root = if children.len() == 1 {
        children.remove(0).node
    } else {
        StartupLayoutNode::Split {
            direction: SplitDirection::Vertical,
            children,
        }
    };
    validate_sizes(&root)?;

    let mut panes = Vec::new();
    collect_choices(&root, &mut panes);
    Ok(StartupLayout { root, panes })
}

pub(crate) fn parse_start_pane_choice(raw: &str) -> Result<StartupPaneChoice> {
    match raw.to_ascii_lowercase().as_str() {
        "messages" | "message" | "message-bus" | "message_bus" | "conversation-list"
        | "conversation_list" => Ok(StartupPaneChoice::Messages),
        "commands" | "command" | "broadcast" => Ok(StartupPaneChoice::Commands),
        _ => parse_provider_choice(raw).map(StartupPaneChoice::Agent),
    }
}

pub(crate) fn parse_provider_choice(raw: &str) -> Result<AgentProviderChoice> {
    match raw.to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claude_code" => Ok(AgentProviderChoice::Claude),
        "codex" => Ok(AgentProviderChoice::Codex),
        "agy" | "antigravity" => Ok(AgentProviderChoice::Agy),
        _ => Err(AgentmuxError::UserError(format!(
            "unknown start pane '{raw}' (expected claude, codex, agy, or messages)"
        ))),
    }
}

/// A single input action parsed from a `/keys` spec, mirroring the variants of
/// `agentmux_agent::InputAction` that a key sequence can express. Held as a
/// dedicated client-side enum so the parser stays unit-testable and `agentmux-cli`
/// does not need to depend on `agentmux-agent`; [`KeyAction::to_json`] emits the
/// exact serde wire shape the daemon already accepts for `agent.broadcast_input`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KeyAction {
    TypeText(String),
    SendRaw(Vec<u8>),
    PressEnter,
    PressEsc,
    PressTab,
    PressBackspace,
    PressCtrl(char),
    PressAlt(char),
}

impl KeyAction {
    /// Serialize to the `InputAction` serde encoding
    /// (`#[serde(rename_all = "snake_case")]`, externally tagged):
    /// `{"type_text":"…"}`, `{"send_raw":[u8…]}`, `{"press_ctrl":"c"}`,
    /// `{"press_alt":"x"}`, and the bare strings `"press_enter"` / `"press_esc"`
    /// / `"press_tab"` / `"press_backspace"` for the unit variants.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Self::TypeText(text) => json!({ "type_text": text }),
            Self::SendRaw(bytes) => json!({ "send_raw": bytes }),
            Self::PressEnter => json!("press_enter"),
            Self::PressEsc => json!("press_esc"),
            Self::PressTab => json!("press_tab"),
            Self::PressBackspace => json!("press_backspace"),
            Self::PressCtrl(ch) => json!({ "press_ctrl": ch }),
            Self::PressAlt(ch) => json!({ "press_alt": ch }),
        }
    }
}

/// Parse a pipe-separated key-sequence spec into a list of [`KeyAction`]s.
///
/// The grammar extends the `print_input_script` example DSL with modifier and
/// arrow/navigation keys:
/// - `text:<s>`              -> `TypeText(s)`
/// - `raw:<hex>`             -> `SendRaw(bytes)` (hex must be even-length, valid)
/// - `bs`                    -> `PressBackspace`
/// - `enter`                 -> `PressEnter`
/// - `esc`                   -> `PressEsc`
/// - `tab`                   -> `PressTab`
/// - `C-<c>` / `ctrl:<c>`    -> `PressCtrl(c)`
/// - `M-<c>` / `alt:<c>`     -> `PressAlt(c)`
/// - `up`/`down`/`left`/`right` -> `SendRaw` of the CSI arrow sequence
/// - `home`/`end`            -> `SendRaw(ESC [ H)` / `SendRaw(ESC [ F)`
///
/// Unlike the example (which panics), an empty spec or any unknown/malformed
/// step returns a [`AgentmuxError::UserError`] so callers can surface it.
pub(crate) fn parse_key_spec(spec: &str) -> Result<Vec<KeyAction>> {
    let mut actions = Vec::new();
    for raw_step in spec.split('|') {
        let step = raw_step.trim();
        if step.is_empty() {
            return Err(AgentmuxError::UserError(
                "empty key step in spec (steps are separated by '|')".to_string(),
            ));
        }
        actions.push(parse_key_step(step)?);
    }
    if actions.is_empty() {
        return Err(AgentmuxError::UserError(
            "key spec must contain at least one step".to_string(),
        ));
    }
    Ok(actions)
}

fn parse_key_step(step: &str) -> Result<KeyAction> {
    if let Some(text) = step.strip_prefix("text:") {
        return Ok(KeyAction::TypeText(text.to_string()));
    }
    if let Some(hex) = step.strip_prefix("raw:") {
        return parse_hex_bytes(hex).map(KeyAction::SendRaw);
    }
    if let Some(rest) = step.strip_prefix("ctrl:") {
        return single_char(rest, "ctrl:").map(KeyAction::PressCtrl);
    }
    if let Some(rest) = step.strip_prefix("alt:") {
        return single_char(rest, "alt:").map(KeyAction::PressAlt);
    }
    if let Some(rest) = step.strip_prefix("C-") {
        return single_char(rest, "C-").map(KeyAction::PressCtrl);
    }
    if let Some(rest) = step.strip_prefix("M-") {
        return single_char(rest, "M-").map(KeyAction::PressAlt);
    }
    match step {
        "bs" => Ok(KeyAction::PressBackspace),
        "enter" => Ok(KeyAction::PressEnter),
        "esc" => Ok(KeyAction::PressEsc),
        "tab" => Ok(KeyAction::PressTab),
        "up" => Ok(KeyAction::SendRaw(b"\x1b[A".to_vec())),
        "down" => Ok(KeyAction::SendRaw(b"\x1b[B".to_vec())),
        "right" => Ok(KeyAction::SendRaw(b"\x1b[C".to_vec())),
        "left" => Ok(KeyAction::SendRaw(b"\x1b[D".to_vec())),
        "home" => Ok(KeyAction::SendRaw(b"\x1b[H".to_vec())),
        "end" => Ok(KeyAction::SendRaw(b"\x1b[F".to_vec())),
        other => Err(AgentmuxError::UserError(format!(
            "unknown key step '{other}' (expected text:/raw:/ctrl:/alt:/C-/M-, \
             bs/enter/esc/tab, or up/down/left/right/home/end)"
        ))),
    }
}

/// Decode an even-length hexadecimal string into bytes, erroring on odd length
/// or any non-hex character.
fn parse_hex_bytes(hex: &str) -> Result<Vec<u8>> {
    if hex.is_empty() {
        return Err(AgentmuxError::UserError(
            "raw: requires hex bytes, e.g. raw:1b5b41".to_string(),
        ));
    }
    if hex.len() % 2 != 0 {
        return Err(AgentmuxError::UserError(format!(
            "raw hex '{hex}' has an odd length; each byte needs two hex digits"
        )));
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16).map_err(|_| {
                AgentmuxError::UserError(format!("invalid hex byte in raw:'{hex}'"))
            })
        })
        .collect()
}

/// Extract exactly one character for a `C-`/`M-`/`ctrl:`/`alt:` modifier step.
fn single_char(rest: &str, prefix: &str) -> Result<char> {
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => Ok(ch),
        _ => Err(AgentmuxError::UserError(format!(
            "'{prefix}' modifier expects exactly one character, got '{rest}'"
        ))),
    }
}

pub(crate) fn normalize_agent_target(raw: &str) -> String {
    let target = raw.trim();
    if target.starts_with("agent:") {
        target.to_string()
    } else {
        format!("agent:{target}")
    }
}

/// Map a user-supplied `--kind` value (the protocol's PascalCase names, accepted
/// case-insensitively) to the daemon's snake_case wire value. Returns a clear
/// error listing the allowed values for anything unrecognized.
pub(crate) fn normalize_message_kind(raw: &str) -> Result<String> {
    let wire = match raw.trim().to_ascii_lowercase().as_str() {
        "taskassignment" => "task_assignment",
        "question" => "question",
        "finding" => "finding",
        "patchproposal" => "patch_proposal",
        "reviewcomment" => "review_comment",
        "testresult" => "test_result",
        "failurereport" => "failure_report",
        "decision" => "decision",
        "handoff" => "handoff",
        "approvalrequest" => "approval_request",
        "contextupdate" => "context_update",
        "statusprobe" => "status_probe",
        _ => {
            return Err(AgentmuxError::UserError(format!(
                "invalid message kind '{raw}'. Allowed values: TaskAssignment, Question, \
                 Finding, PatchProposal, ReviewComment, TestResult, FailureReport, Decision, \
                 Handoff, ApprovalRequest, ContextUpdate, StatusProbe"
            )));
        }
    };
    Ok(wire.to_string())
}

/// Validate and normalize a `--priority` value to the wire form.
pub(crate) fn normalize_priority(raw: &str) -> Result<String> {
    let wire = match raw.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "normal" => "normal",
        "high" => "high",
        "urgent" => "urgent",
        _ => {
            return Err(AgentmuxError::UserError(format!(
                "invalid priority '{raw}'. Allowed values: low, normal, high, urgent"
            )));
        }
    };
    Ok(wire.to_string())
}

pub(crate) fn should_inject_message(_inject: bool, no_inject: bool) -> bool {
    !no_inject
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(provider: AgentProviderChoice) -> StartupPaneChoice {
        StartupPaneChoice::Agent(provider)
    }

    fn leaf(choice: StartupPaneChoice) -> StartupLayoutNode {
        StartupLayoutNode::Leaf(choice)
    }

    fn child(node: StartupLayoutNode) -> StartupChild {
        StartupChild { node, size: None }
    }

    fn sized(node: StartupLayoutNode, size: u16) -> StartupChild {
        StartupChild {
            node,
            size: Some(size),
        }
    }

    fn ok(raw: &str) -> StartupLayout {
        parse_start_layout(Some(raw)).expect("expected a valid layout")
    }

    /// The root split direction (the historical "flat" direction).
    fn dir(layout: &StartupLayout) -> SplitDirection {
        match &layout.root {
            StartupLayoutNode::Split { direction, .. } => *direction,
            StartupLayoutNode::Leaf(_) => SplitDirection::Vertical,
        }
    }

    fn err(raw: &str) -> String {
        match parse_start_layout(Some(raw)) {
            Err(AgentmuxError::UserError(message)) => message,
            other => panic!("expected UserError, got {other:?}"),
        }
    }

    #[test]
    fn none_input_yields_empty_panes_and_vertical() {
        let layout = parse_start_layout(None).expect("None must parse");
        assert!(layout.panes.is_empty());
        assert_eq!(dir(&layout), SplitDirection::Vertical);
    }

    #[test]
    fn blank_input_yields_empty_panes_and_vertical() {
        let layout = ok("   ");
        assert!(layout.panes.is_empty());
        assert_eq!(dir(&layout), SplitDirection::Vertical);
    }

    #[test]
    fn single_pane_is_a_bare_leaf() {
        let layout = ok("agy");
        assert_eq!(layout.panes, vec![agent(AgentProviderChoice::Agy)]);
        assert_eq!(layout.root, leaf(agent(AgentProviderChoice::Agy)));
        assert_eq!(dir(&layout), SplitDirection::Vertical);
    }

    #[test]
    fn legacy_comma_list_stays_vertical_with_order_preserved() {
        let layout = ok("agy,codex,messages");
        assert_eq!(
            layout.panes,
            vec![
                agent(AgentProviderChoice::Agy),
                agent(AgentProviderChoice::Codex),
                StartupPaneChoice::Messages,
            ]
        );
        assert_eq!(dir(&layout), SplitDirection::Vertical);
    }

    #[test]
    fn left_right_bar_is_vertical_with_and_without_spaces() {
        let expected = vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)];
        let spaced = ok("agy | codex");
        assert_eq!(spaced.panes, expected);
        assert_eq!(dir(&spaced), SplitDirection::Vertical);
        let tight = ok("agy|codex");
        assert_eq!(tight.panes, expected);
        assert_eq!(dir(&tight), SplitDirection::Vertical);
    }

    #[test]
    fn left_right_slash_alias_is_vertical_with_and_without_spaces() {
        let expected = vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)];
        let spaced = ok("agy / codex");
        assert_eq!(spaced.panes, expected);
        assert_eq!(dir(&spaced), SplitDirection::Vertical);
        let tight = ok("agy/codex");
        assert_eq!(tight.panes, expected);
        assert_eq!(dir(&tight), SplitDirection::Vertical);
    }

    #[test]
    fn top_bottom_bar_u2015_is_horizontal() {
        let layout = ok("agy ― codex");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(dir(&layout), SplitDirection::Horizontal);
    }

    #[test]
    fn top_bottom_spaced_dash_is_horizontal() {
        let layout = ok("agy - codex");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(dir(&layout), SplitDirection::Horizontal);
    }

    #[test]
    fn word_internal_hyphen_is_not_a_separator() {
        let layout = ok("claude-code | codex");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Claude), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(dir(&layout), SplitDirection::Vertical);
    }

    #[test]
    fn hyphen_without_spaces_is_a_single_unknown_token() {
        let message = err("agy-codex");
        assert!(
            message.contains("unknown start pane 'agy-codex'"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn operator_precedence_binds_top_bottom_tighter_than_left_right() {
        // `agy | codex ― messages` parses as `agy | (codex ― messages)` because
        // `―` binds tighter than `|` (per the ADR-0007 precedence grammar).
        let layout = ok("agy | codex ― messages");
        assert_eq!(
            layout.root,
            StartupLayoutNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    child(leaf(agent(AgentProviderChoice::Agy))),
                    child(StartupLayoutNode::Split {
                        direction: SplitDirection::Horizontal,
                        children: vec![
                            child(leaf(agent(AgentProviderChoice::Codex))),
                            child(leaf(StartupPaneChoice::Messages)),
                        ],
                    }),
                ],
            }
        );
    }

    #[test]
    fn grouping_forces_left_right_inside_top_bottom() {
        // Wrapping flips the precedence: (agy | codex) ― messages.
        let layout = ok("(agy | codex) ― messages");
        assert_eq!(
            layout.root,
            StartupLayoutNode::Split {
                direction: SplitDirection::Horizontal,
                children: vec![
                    child(StartupLayoutNode::Split {
                        direction: SplitDirection::Vertical,
                        children: vec![
                            child(leaf(agent(AgentProviderChoice::Agy))),
                            child(leaf(agent(AgentProviderChoice::Codex))),
                        ],
                    }),
                    child(leaf(StartupPaneChoice::Messages)),
                ],
            }
        );
    }

    #[test]
    fn mixing_comma_and_splitter_is_rejected() {
        let message = err("agy, codex | messages");
        assert!(
            message.contains("legacy comma list cannot be mixed"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn separator_only_input_is_rejected() {
        let message = err("|");
        assert!(
            message.contains("expected a pane name") || message.contains("only separators"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn pane_names_are_case_insensitive() {
        let layout = ok("AGY | CODEX");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Agy), agent(AgentProviderChoice::Codex)]
        );
        assert_eq!(dir(&layout), SplitDirection::Vertical);
    }

    // ---- Phase 2: nesting and sizes ----

    #[test]
    fn grouped_top_bottom_inside_left_right() {
        // (agy ― codex) | messages
        let layout = ok("(agy ― codex) | messages");
        assert_eq!(
            layout.panes,
            vec![
                agent(AgentProviderChoice::Agy),
                agent(AgentProviderChoice::Codex),
                StartupPaneChoice::Messages,
            ]
        );
        assert_eq!(
            layout.root,
            StartupLayoutNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    child(StartupLayoutNode::Split {
                        direction: SplitDirection::Horizontal,
                        children: vec![
                            child(leaf(agent(AgentProviderChoice::Agy))),
                            child(leaf(agent(AgentProviderChoice::Codex))),
                        ],
                    }),
                    child(leaf(StartupPaneChoice::Messages)),
                ],
            }
        );
    }

    #[test]
    fn explicit_sizes_are_attached_to_child_slots() {
        // agy:60 | codex:40
        let layout = ok("agy:60 | codex:40");
        assert_eq!(
            layout.root,
            StartupLayoutNode::Split {
                direction: SplitDirection::Vertical,
                children: vec![
                    sized(leaf(agent(AgentProviderChoice::Agy)), 60),
                    sized(leaf(agent(AgentProviderChoice::Codex)), 40),
                ],
            }
        );
    }

    #[test]
    fn mixed_sized_and_unsized_inside_a_group() {
        // (agy:50 | codex) ― messages
        let layout = ok("(agy:50 | codex) ― messages");
        assert_eq!(
            layout.root,
            StartupLayoutNode::Split {
                direction: SplitDirection::Horizontal,
                children: vec![
                    child(StartupLayoutNode::Split {
                        direction: SplitDirection::Vertical,
                        children: vec![
                            sized(leaf(agent(AgentProviderChoice::Agy)), 50),
                            child(leaf(agent(AgentProviderChoice::Codex))),
                        ],
                    }),
                    child(leaf(StartupPaneChoice::Messages)),
                ],
            }
        );
    }

    #[test]
    fn deeply_nested_groups_flatten_same_direction_chains() {
        // a | b | c flattens to a single 3-way vertical split.
        let layout = ok("agy | codex | claude");
        match &layout.root {
            StartupLayoutNode::Split {
                direction: SplitDirection::Vertical,
                children,
            } => assert_eq!(children.len(), 3),
            other => panic!("expected a flat 3-way vertical split, got {other:?}"),
        }
    }

    #[test]
    fn empty_group_is_rejected() {
        let message = err("() | codex");
        assert!(
            message.contains("empty group"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn unbalanced_parentheses_are_rejected() {
        let message = err("(agy | codex");
        assert!(
            message.contains("unbalanced parentheses") || message.contains("only separators"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn stray_close_paren_is_rejected() {
        let message = err("agy | codex)");
        assert!(
            message.contains("parentheses") || message.contains("')'"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn zero_size_is_rejected() {
        let message = err("agy:0 | codex");
        assert!(
            message.contains("at least 1"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn non_numeric_size_is_rejected() {
        let message = err("agy: | codex");
        assert!(
            message.contains("number after ':'"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn sizes_exceeding_one_hundred_are_rejected() {
        let message = err("agy:70 | codex:50");
        assert!(
            message.contains("exceeds 100%"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn commands_tokens_resolve_to_commands_pane_choice() {
        for token in ["commands", "command", "broadcast", "COMMANDS", "Broadcast"] {
            assert_eq!(
                parse_start_pane_choice(token).expect("token parses"),
                StartupPaneChoice::Commands,
                "token {token:?} should resolve to Commands"
            );
        }
    }

    #[test]
    fn commands_leaf_parses_inside_a_layout() {
        let layout = ok("agy | commands");
        assert_eq!(
            layout.panes,
            vec![agent(AgentProviderChoice::Agy), StartupPaneChoice::Commands]
        );
        assert_eq!(dir(&layout), SplitDirection::Vertical);
    }
}
