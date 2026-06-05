use crate::*;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
}

impl DaemonConfig {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredAgentSession {
    pub id: AgentSessionId,
    pub name: String,
    pub role: AgentRole,
    pub status: Option<AgentStatus>,
    pub process_id: Option<u32>,
    pub attached_clients: BTreeSet<ClientSessionId>,
}

impl RegisteredAgentSession {
    pub(crate) fn with_role(name: String, role: AgentRole, process_id: Option<u32>) -> Self {
        Self {
            id: AgentSessionId::new(),
            name,
            role,
            status: None,
            process_id,
            attached_clients: BTreeSet::new(),
        }
    }

    pub(crate) fn restored_with_role(id: AgentSessionId, name: String, role: AgentRole) -> Self {
        Self {
            id,
            name,
            role,
            status: None,
            process_id: None,
            attached_clients: BTreeSet::new(),
        }
    }
}
