//! State-change/command-effect signals and provider-picker choice types.

use crate::keymap::TuiCommand;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateChange {
    AddedPane(String),
    UpdatedPane(String),
    FocusedPane(String),
    RemovedPane(String),
    UpdatedMessages,
    Ignored,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandEffect {
    Continue,
    Detach,
    Quit,
    SpawnAgentPane(AgentProviderChoice),
    OpenConversationListPane,
    #[cfg(feature = "activity-feed")]
    ToggleActivityFeedPane {
        visible: bool,
    },
    #[cfg(feature = "activity-feed")]
    FocusPaneById(String),
    #[cfg(feature = "arena")]
    ArenaAdopt(String),
    StopPane(String),
    RefreshMessages,
    Unhandled(TuiCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentProviderChoice {
    Claude,
    Codex,
    Agy,
}

impl AgentProviderChoice {
    pub fn provider(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Agy => "Antigravity",
        }
    }

    pub fn default_name(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::Agy => "agy",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NewPaneChoice {
    Agent(AgentProviderChoice),
    ConversationList,
}

impl NewPaneChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent(provider) => provider.label(),
            Self::ConversationList => "Conversation List",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderOption {
    pub choice: NewPaneChoice,
    pub hint: &'static str,
}

pub(crate) const PROVIDER_OPTIONS: &[ProviderOption] = &[
    ProviderOption {
        choice: NewPaneChoice::Agent(AgentProviderChoice::Claude),
        hint: "Claude Code",
    },
    ProviderOption {
        choice: NewPaneChoice::Agent(AgentProviderChoice::Codex),
        hint: "OpenAI Codex",
    },
    ProviderOption {
        choice: NewPaneChoice::Agent(AgentProviderChoice::Agy),
        hint: "Google Antigravity CLI",
    },
    ProviderOption {
        choice: NewPaneChoice::ConversationList,
        hint: "Message history panel",
    },
];
