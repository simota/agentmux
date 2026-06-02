PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  root_path TEXT NOT NULL,
  default_branch TEXT NOT NULL,
  config_path TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  status TEXT NOT NULL,
  team_template TEXT NOT NULL,
  root_context_scope_id TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT
);

CREATE TABLE IF NOT EXISTS agent_sessions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
  name TEXT NOT NULL,
  provider TEXT NOT NULL,
  role TEXT NOT NULL,
  mode TEXT NOT NULL,
  process_id INTEGER,
  pane_id TEXT,
  terminal_buffer_id TEXT NOT NULL,
  worktree_id TEXT,
  cwd TEXT NOT NULL,
  env_json TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL,
  capabilities_json TEXT NOT NULL DEFAULT '{}',
  inbox_id TEXT NOT NULL,
  context_scope_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  last_activity_at TEXT NOT NULL,
  exited_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_agent_sessions_task ON agent_sessions(task_id);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_status ON agent_sessions(status);

CREATE TABLE IF NOT EXISTS pane_layouts (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  layout_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS worktrees (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  owner_agent_id TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL,
  path TEXT NOT NULL,
  branch_name TEXT NOT NULL,
  base_branch TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_worktrees_task ON worktrees(task_id);

CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY,
  task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
  from_json TEXT NOT NULL,
  to_json TEXT NOT NULL,
  kind TEXT NOT NULL,
  priority TEXT NOT NULL,
  body TEXT NOT NULL,
  context_refs_json TEXT NOT NULL DEFAULT '[]',
  artifact_refs_json TEXT NOT NULL DEFAULT '[]',
  delivery_mode TEXT NOT NULL,
  delivery_status TEXT NOT NULL,
  requires_response INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  delivered_at TEXT,
  read_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_messages_task ON messages(task_id);
CREATE INDEX IF NOT EXISTS idx_messages_kind ON messages(kind);
CREATE INDEX IF NOT EXISTS idx_messages_delivery ON messages(delivery_status);

CREATE TABLE IF NOT EXISTS context_items (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
  scope_json TEXT NOT NULL,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  source_json TEXT NOT NULL,
  visibility TEXT NOT NULL,
  confidence REAL NOT NULL DEFAULT 0.8,
  tags_json TEXT NOT NULL DEFAULT '[]',
  related_files_json TEXT NOT NULL DEFAULT '[]',
  artifact_refs_json TEXT NOT NULL DEFAULT '[]',
  redacted INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_context_project ON context_items(project_id);
CREATE INDEX IF NOT EXISTS idx_context_task ON context_items(task_id);
CREATE INDEX IF NOT EXISTS idx_context_kind ON context_items(kind);

CREATE VIRTUAL TABLE IF NOT EXISTS context_items_fts USING fts5(
  title,
  body,
  tags,
  content='context_items',
  content_rowid='rowid'
);

CREATE TABLE IF NOT EXISTS artifacts (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  path TEXT NOT NULL,
  title TEXT NOT NULL,
  mime_type TEXT,
  size_bytes INTEGER NOT NULL,
  checksum TEXT,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_artifacts_task ON artifacts(task_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_kind ON artifacts(kind);

CREATE TABLE IF NOT EXISTS approvals (
  id TEXT PRIMARY KEY,
  task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE,
  agent_id TEXT REFERENCES agent_sessions(id) ON DELETE SET NULL,
  kind TEXT NOT NULL,
  risk TEXT NOT NULL,
  title TEXT NOT NULL,
  description TEXT NOT NULL,
  proposed_input_json TEXT,
  command TEXT,
  context_refs_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  decided_at TEXT,
  decided_by TEXT
);

CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(status);
CREATE INDEX IF NOT EXISTS idx_approvals_task ON approvals(task_id);

CREATE TABLE IF NOT EXISTS event_log_index (
  id TEXT PRIMARY KEY,
  ts TEXT NOT NULL,
  type TEXT NOT NULL,
  project_id TEXT,
  task_id TEXT,
  agent_id TEXT,
  byte_offset INTEGER,
  payload_summary TEXT
);

CREATE INDEX IF NOT EXISTS idx_event_log_task ON event_log_index(task_id);
CREATE INDEX IF NOT EXISTS idx_event_log_agent ON event_log_index(agent_id);
CREATE INDEX IF NOT EXISTS idx_event_log_type ON event_log_index(type);
