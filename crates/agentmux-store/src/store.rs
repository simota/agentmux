//! `Store` — SQLite connection wrapper.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use agentmux_core::{
    ActorId, AgentMode, AgentProvider, AgentRole, AgentSessionId, AgentStatus, ContextScopeId,
    DateTimeUtc, InboxId, PaneId, ProjectId, TaskId, TaskStatus, TerminalBufferId, WorktreeId,
    error::Result,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use time::format_description::well_known::Rfc3339;

use agentmux_core::AgentmuxError;

const SCHEMA_SQL: &str = include_str!("../../../docs/sql/schema.sql");
const INITIAL_SCHEMA_VERSION: i64 = 1;

/// Persisted project metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecord {
    pub id: ProjectId,
    pub name: String,
    pub root_path: PathBuf,
    pub default_branch: String,
    pub config_path: Option<PathBuf>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

/// Persisted task metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: TaskId,
    pub project_id: ProjectId,
    pub title: String,
    pub body: String,
    pub status: TaskStatus,
    pub team_template: String,
    pub root_context_scope_id: ContextScopeId,
    pub created_by: ActorId,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub completed_at: Option<DateTimeUtc>,
}

/// Persisted agent-session metadata. Live PTY handles remain daemon state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    pub id: AgentSessionId,
    pub project_id: ProjectId,
    pub task_id: Option<TaskId>,
    pub name: String,
    pub provider: AgentProvider,
    pub role: AgentRole,
    pub mode: AgentMode,
    pub process_id: Option<u32>,
    pub pane_id: Option<PaneId>,
    pub terminal_buffer_id: TerminalBufferId,
    pub worktree_id: Option<WorktreeId>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub status: AgentStatus,
    pub capabilities: serde_json::Value,
    pub inbox_id: InboxId,
    pub context_scope_id: ContextScopeId,
    pub created_at: DateTimeUtc,
    pub last_activity_at: DateTimeUtc,
    pub exited_at: Option<DateTimeUtc>,
}

/// The main persistence handle for the agentmux daemon.
///
/// Wraps a `rusqlite::Connection` (single-writer model for v0.1).
/// For async usage the store writer runs on a dedicated blocking task.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Open (or create) the SQLite database at `db_path`.
    ///
    /// Runs embedded DDL migrations on first open.
    ///
    /// # Errors
    /// Returns `AgentmuxError::StoreError` if the file cannot be opened or
    /// migrations fail.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                AgentmuxError::StoreError(format!(
                    "failed to create store directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }

        let connection = Connection::open(db_path).map_err(store_error)?;
        Self::from_connection(connection)
    }

    /// Open an in-memory database. Intended for unit tests and short-lived tools.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(store_error)?)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(store_error)?;
        let store = Self { connection };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        self.connection
            .execute_batch(SCHEMA_SQL)
            .map_err(store_error)?;
        self.connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations (version, name, applied_at)
                 VALUES (?1, ?2, ?3)",
                params![
                    INITIAL_SCHEMA_VERSION,
                    "initial_schema",
                    format_ts(DateTimeUtc::now_utc())?
                ],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// Insert or update a project.
    pub fn upsert_project(&self, project: &ProjectRecord) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO projects
                 (id, name, root_path, default_branch, config_path, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                   name = excluded.name,
                   root_path = excluded.root_path,
                   default_branch = excluded.default_branch,
                   config_path = excluded.config_path,
                   updated_at = excluded.updated_at",
                params![
                    project.id.to_string(),
                    project.name,
                    path_to_db(&project.root_path),
                    project.default_branch,
                    project.config_path.as_ref().map(|path| path_to_db(path)),
                    format_ts(project.created_at)?,
                    format_ts(project.updated_at)?,
                ],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// Fetch a project by ID.
    pub fn get_project(&self, id: &ProjectId) -> Result<Option<ProjectRecord>> {
        let row = self
            .connection
            .query_row(
                "SELECT id, name, root_path, default_branch, config_path, created_at, updated_at
                 FROM projects WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(store_error)?;
        row.map(project_from_row).transpose()
    }

    /// Insert or update a task.
    pub fn upsert_task(&self, task: &TaskRecord) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO tasks
                 (id, project_id, title, body, status, team_template, root_context_scope_id,
                  created_by, created_at, updated_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(id) DO UPDATE SET
                   title = excluded.title,
                   body = excluded.body,
                   status = excluded.status,
                   team_template = excluded.team_template,
                   root_context_scope_id = excluded.root_context_scope_id,
                   updated_at = excluded.updated_at,
                   completed_at = excluded.completed_at",
                params![
                    task.id.to_string(),
                    task.project_id.to_string(),
                    task.title,
                    task.body,
                    to_json_text(&task.status)?,
                    task.team_template,
                    task.root_context_scope_id.to_string(),
                    task.created_by.to_string(),
                    format_ts(task.created_at)?,
                    format_ts(task.updated_at)?,
                    task.completed_at.map(format_ts).transpose()?,
                ],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// Fetch all tasks for a project in creation order.
    pub fn list_tasks(&self, project_id: &ProjectId) -> Result<Vec<TaskRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, project_id, title, body, status, team_template,
                        root_context_scope_id, created_by, created_at, updated_at, completed_at
                 FROM tasks WHERE project_id = ?1 ORDER BY created_at, id",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![project_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            })
            .map_err(store_error)?;
        collect_rows(rows)?.into_iter().map(task_from_row).collect()
    }

    /// Insert or update an agent session metadata row.
    pub fn upsert_agent_session(&self, agent: &AgentSessionRecord) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO agent_sessions
                 (id, project_id, task_id, name, provider, role, mode, process_id, pane_id,
                  terminal_buffer_id, worktree_id, cwd, env_json, status, capabilities_json,
                  inbox_id, context_scope_id, created_at, last_activity_at, exited_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                         ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                 ON CONFLICT(id) DO UPDATE SET
                   task_id = excluded.task_id,
                   name = excluded.name,
                   provider = excluded.provider,
                   role = excluded.role,
                   mode = excluded.mode,
                   process_id = excluded.process_id,
                   pane_id = excluded.pane_id,
                   terminal_buffer_id = excluded.terminal_buffer_id,
                   worktree_id = excluded.worktree_id,
                   cwd = excluded.cwd,
                   env_json = excluded.env_json,
                   status = excluded.status,
                   capabilities_json = excluded.capabilities_json,
                   inbox_id = excluded.inbox_id,
                   context_scope_id = excluded.context_scope_id,
                   last_activity_at = excluded.last_activity_at,
                   exited_at = excluded.exited_at",
                params![
                    agent.id.to_string(),
                    agent.project_id.to_string(),
                    agent.task_id.as_ref().map(ToString::to_string),
                    agent.name,
                    to_json_text(&agent.provider)?,
                    to_json_text(&agent.role)?,
                    to_json_text(&agent.mode)?,
                    agent.process_id,
                    agent.pane_id.as_ref().map(ToString::to_string),
                    agent.terminal_buffer_id.to_string(),
                    agent.worktree_id.as_ref().map(ToString::to_string),
                    path_to_db(&agent.cwd),
                    to_json_text(&agent.env)?,
                    to_json_text(&agent.status)?,
                    agent.capabilities.to_string(),
                    agent.inbox_id.to_string(),
                    agent.context_scope_id.to_string(),
                    format_ts(agent.created_at)?,
                    format_ts(agent.last_activity_at)?,
                    agent.exited_at.map(format_ts).transpose()?,
                ],
            )
            .map_err(store_error)?;
        Ok(())
    }

    /// Fetch all agent sessions for a project in creation order.
    pub fn list_agent_sessions(&self, project_id: &ProjectId) -> Result<Vec<AgentSessionRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, project_id, task_id, name, provider, role, mode, process_id,
                        pane_id, terminal_buffer_id, worktree_id, cwd, env_json, status,
                        capabilities_json, inbox_id, context_scope_id, created_at,
                        last_activity_at, exited_at
                 FROM agent_sessions WHERE project_id = ?1 ORDER BY created_at, id",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map(params![project_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<u32>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, Option<String>>(19)?,
                ))
            })
            .map_err(store_error)?;
        collect_rows(rows)?
            .into_iter()
            .map(agent_from_row)
            .collect()
    }
}

type ProjectRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
);

type TaskRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

type AgentRow = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<u32>,
    Option<String>,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

fn project_from_row(row: ProjectRow) -> Result<ProjectRecord> {
    Ok(ProjectRecord {
        id: parse_id(row.0)?,
        name: row.1,
        root_path: PathBuf::from(row.2),
        default_branch: row.3,
        config_path: row.4.map(PathBuf::from),
        created_at: parse_ts(row.5)?,
        updated_at: parse_ts(row.6)?,
    })
}

fn task_from_row(row: TaskRow) -> Result<TaskRecord> {
    Ok(TaskRecord {
        id: parse_id(row.0)?,
        project_id: parse_id(row.1)?,
        title: row.2,
        body: row.3,
        status: from_json_text(&row.4)?,
        team_template: row.5,
        root_context_scope_id: parse_id(row.6)?,
        created_by: parse_id(row.7)?,
        created_at: parse_ts(row.8)?,
        updated_at: parse_ts(row.9)?,
        completed_at: row.10.map(parse_ts).transpose()?,
    })
}

fn agent_from_row(row: AgentRow) -> Result<AgentSessionRecord> {
    Ok(AgentSessionRecord {
        id: parse_id(row.0)?,
        project_id: parse_id(row.1)?,
        task_id: row.2.map(parse_id).transpose()?,
        name: row.3,
        provider: from_json_text(&row.4)?,
        role: from_json_text(&row.5)?,
        mode: from_json_text(&row.6)?,
        process_id: row.7,
        pane_id: row.8.map(parse_id).transpose()?,
        terminal_buffer_id: parse_id(row.9)?,
        worktree_id: row.10.map(parse_id).transpose()?,
        cwd: PathBuf::from(row.11),
        env: from_json_text(&row.12)?,
        status: from_json_text(&row.13)?,
        capabilities: serde_json::from_str(&row.14).map_err(json_error)?,
        inbox_id: parse_id(row.15)?,
        context_scope_id: parse_id(row.16)?,
        created_at: parse_ts(row.17)?,
        last_activity_at: parse_ts(row.18)?,
        exited_at: row.19.map(parse_ts).transpose()?,
    })
}

fn collect_rows<T>(
    rows: impl Iterator<Item = std::result::Result<T, rusqlite::Error>>,
) -> Result<Vec<T>> {
    rows.map(|row| row.map_err(store_error)).collect()
}

fn path_to_db(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn to_json_text<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(json_error)
}

fn from_json_text<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(json_error)
}

fn parse_id<T>(value: String) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| AgentmuxError::StoreError(format!("invalid stored id '{value}': {error}")))
}

fn format_ts(timestamp: DateTimeUtc) -> Result<String> {
    timestamp
        .format(&Rfc3339)
        .map_err(|error| AgentmuxError::StoreError(format!("invalid timestamp: {error}")))
}

fn parse_ts(value: String) -> Result<DateTimeUtc> {
    DateTimeUtc::parse(&value, &Rfc3339).map_err(|error| {
        AgentmuxError::StoreError(format!("invalid stored timestamp '{value}': {error}"))
    })
}

fn json_error(error: serde_json::Error) -> AgentmuxError {
    AgentmuxError::StoreError(format!("invalid stored JSON: {error}"))
}

fn store_error(error: rusqlite::Error) -> AgentmuxError {
    AgentmuxError::StoreError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_core::{AgentMode, AgentProvider, AgentRole, AgentStatus};
    use serde_json::json;

    #[test]
    fn open_runs_initial_schema_migration() {
        let store = Store::open_in_memory().expect("store opens");

        let version: i64 = store
            .connection
            .query_row(
                "SELECT version FROM schema_migrations WHERE version = 1",
                [],
                |row| row.get(0),
            )
            .expect("migration row exists");
        let project_count: i64 = store
            .connection
            .query_row("SELECT count(*) FROM projects", [], |row| row.get(0))
            .expect("projects table exists");

        assert_eq!(version, INITIAL_SCHEMA_VERSION);
        assert_eq!(project_count, 0);
    }

    #[test]
    fn upserts_and_fetches_project_metadata() {
        let store = Store::open_in_memory().expect("store opens");
        let now = DateTimeUtc::now_utc();
        let project = ProjectRecord {
            id: ProjectId::new(),
            name: "agentmux".to_owned(),
            root_path: PathBuf::from("/tmp/agentmux"),
            default_branch: "main".to_owned(),
            config_path: Some(PathBuf::from("/tmp/agentmux/.agentmux/config.toml")),
            created_at: now,
            updated_at: now,
        };

        store.upsert_project(&project).expect("project is stored");
        let fetched = store
            .get_project(&project.id)
            .expect("project query succeeds")
            .expect("project exists");

        assert_eq!(fetched, project);
    }

    #[test]
    fn open_creates_parent_directory_and_persists_across_reopen() {
        let now = DateTimeUtc::now_utc();
        let root = std::env::temp_dir().join(format!("agentmux-store-{}", ProjectId::new()));
        let db_path = root.join(".agentmux").join("state.db");
        let project = project(now);

        {
            let store = Store::open(&db_path).expect("file store opens");
            store.upsert_project(&project).expect("project is stored");
        }

        {
            let store = Store::open(&db_path).expect("file store reopens");
            let fetched = store
                .get_project(&project.id)
                .expect("project query succeeds")
                .expect("project exists");
            assert_eq!(fetched, project);
        }

        std::fs::remove_dir_all(root).expect("temporary store directory is removed");
    }

    #[test]
    fn persists_tasks_for_a_project() {
        let store = Store::open_in_memory().expect("store opens");
        let now = DateTimeUtc::now_utc();
        let project = project(now);
        store.upsert_project(&project).expect("project is stored");
        let task = TaskRecord {
            id: TaskId::new(),
            project_id: project.id.clone(),
            title: "Implement store".to_owned(),
            body: "Persist project/task/agent metadata".to_owned(),
            status: TaskStatus::Running,
            team_template: "claude-codex".to_owned(),
            root_context_scope_id: ContextScopeId::new(),
            created_by: ActorId::new(),
            created_at: now,
            updated_at: now,
            completed_at: None,
        };

        store.upsert_task(&task).expect("task is stored");
        let tasks = store
            .list_tasks(&project.id)
            .expect("task list query succeeds");

        assert_eq!(tasks, vec![task]);
    }

    #[test]
    fn persists_agent_session_metadata_with_json_fields() {
        let store = Store::open_in_memory().expect("store opens");
        let now = DateTimeUtc::now_utc();
        let project = project(now);
        store.upsert_project(&project).expect("project is stored");
        let task = TaskRecord {
            id: TaskId::new(),
            project_id: project.id.clone(),
            title: "Run implementer".to_owned(),
            body: String::new(),
            status: TaskStatus::Created,
            team_template: "claude-codex".to_owned(),
            root_context_scope_id: ContextScopeId::new(),
            created_by: ActorId::new(),
            created_at: now,
            updated_at: now,
            completed_at: None,
        };
        store.upsert_task(&task).expect("task is stored");
        let agent = AgentSessionRecord {
            id: AgentSessionId::new(),
            project_id: project.id.clone(),
            task_id: Some(task.id),
            name: "impl-codex".to_owned(),
            provider: AgentProvider::Codex,
            role: AgentRole::Implementer,
            mode: AgentMode::InteractiveTui,
            process_id: Some(42),
            pane_id: Some(PaneId::new()),
            terminal_buffer_id: TerminalBufferId::new(),
            worktree_id: None,
            cwd: project.root_path.clone(),
            env: BTreeMap::from([("AGENTMUX".to_owned(), "1".to_owned())]),
            status: AgentStatus::InteractiveReady,
            capabilities: json!({"supports_bracketed_paste": true}),
            inbox_id: InboxId::new(),
            context_scope_id: ContextScopeId::new(),
            created_at: now,
            last_activity_at: now,
            exited_at: None,
        };

        store
            .upsert_agent_session(&agent)
            .expect("agent session is stored");
        let agents = store
            .list_agent_sessions(&project.id)
            .expect("agent list query succeeds");

        assert_eq!(agents, vec![agent]);
    }

    fn project(now: DateTimeUtc) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId::new(),
            name: "agentmux".to_owned(),
            root_path: PathBuf::from("/tmp/agentmux"),
            default_branch: "main".to_owned(),
            config_path: None,
            created_at: now,
            updated_at: now,
        }
    }
}
