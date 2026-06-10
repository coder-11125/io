use crate::types::{Session, SessionId};
use rusqlite::Connection;
use std::path::PathBuf;
use std::str::FromStr;

pub struct SessionStore {
    db_path: PathBuf,
}

impl SessionStore {
    pub fn new() -> anyhow::Result<Self> {
        let db_path = Self::default_path()?;
        let store = Self { db_path };
        store.initialize_db()?;
        Ok(store)
    }

    pub fn with_path(path: PathBuf) -> anyhow::Result<Self> {
        let store = Self { db_path: path };
        store.initialize_db()?;
        Ok(store)
    }

    fn default_path() -> anyhow::Result<PathBuf> {
        let base = dirs::data_dir()
            .map(|d| d.join("io"))
            .unwrap_or_else(|| PathBuf::from("~/.local/share/io"));
        std::fs::create_dir_all(&base)?;
        Ok(base.join("sessions.db"))
    }

    fn initialize_db(&self) -> anyhow::Result<()> {
        let conn = Connection::open(&self.db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS turns (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                data TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);",
        )?;
        Ok(())
    }

    pub async fn save_session(&self, session: &Session) -> anyhow::Result<()> {
        let conn = Connection::open(&self.db_path)?;
        let id = session.id.to_string();
        let data = serde_json::to_string(session)?;
        let created = session.created_at.to_rfc3339();
        let updated = session.updated_at.to_rfc3339();

        conn.execute(
            "INSERT OR REPLACE INTO sessions (id, data, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, data, created, updated],
        )?;

        for turn in &session.turns {
            let turn_id = turn.id.to_string();
            let turn_data = serde_json::to_string(turn)?;
            let timestamp = turn.timestamp.to_rfc3339();
            conn.execute(
                "INSERT OR REPLACE INTO turns (id, session_id, data, timestamp) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![turn_id, id, turn_data, timestamp],
            )?;
        }

        Ok(())
    }

    pub fn load_session(&self, id: SessionId) -> anyhow::Result<Session> {
        let conn = Connection::open(&self.db_path)?;
        let id_str = id.to_string();
        let data: String = conn.query_row(
            "SELECT data FROM sessions WHERE id = ?1",
            rusqlite::params![id_str],
            |row| row.get(0),
        )?;
        let session: Session = serde_json::from_str(&data)?;
        Ok(session)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionSummary>> {
        let conn = Connection::open(&self.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, created_at, updated_at FROM sessions ORDER BY updated_at DESC LIMIT 50",
        )?;

        let summaries = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let created: String = row.get(1)?;
            let updated: String = row.get(2)?;
            Ok(SessionSummary {
                id: SessionId::from_str(&id).unwrap_or(SessionId::new()),
                created_at: created,
                updated_at: updated,
            })
        })?;

        summaries.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_session(&self, id: SessionId) -> anyhow::Result<()> {
        let conn = Connection::open(&self.db_path)?;
        let id_str = id.to_string();
        conn.execute(
            "DELETE FROM turns WHERE session_id = ?1",
            rusqlite::params![id_str],
        )?;
        conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![id_str],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub created_at: String,
    pub updated_at: String,
}
