use crate::types::{Session, SessionId};
use rusqlite::Connection;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

/// SQLite-backed session persistence.
///
/// Each session is stored as a single JSON blob — the one source of truth.
/// The store holds one shared connection behind a mutex; saves run on the
/// blocking thread pool so they never stall the async runtime mid-stream.
#[derive(Clone)]
pub struct SessionStore {
    conn: Arc<Mutex<Connection>>,
}

impl SessionStore {
    pub fn new() -> anyhow::Result<Self> {
        Self::with_path(Self::default_path()?)
    }

    pub fn with_path(path: PathBuf) -> anyhow::Result<Self> {
        let conn = Connection::open(&path)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
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
        self.conn.lock().unwrap().execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);

            -- Legacy table: duplicated every turn as a separate row but was
            -- never read back (sessions.data embeds the turns). Drop it.
            DROP TABLE IF EXISTS turns;",
        )?;
        Ok(())
    }

    pub async fn save_session(&self, session: &Session) -> anyhow::Result<()> {
        let id = session.id.to_string();
        let data = serde_json::to_string(session)?;
        let created = session.created_at.to_rfc3339();
        let updated = session.updated_at.to_rfc3339();

        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            conn.lock().unwrap().execute(
                "INSERT OR REPLACE INTO sessions (id, data, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, data, created, updated],
            )
        })
        .await??;
        Ok(())
    }

    pub fn load_session(&self, id: SessionId) -> anyhow::Result<Session> {
        let data: String = self.conn.lock().unwrap().query_row(
            "SELECT data FROM sessions WHERE id = ?1",
            rusqlite::params![id.to_string()],
            |row| row.get(0),
        )?;
        let session: Session = serde_json::from_str(&data)?;
        Ok(session)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, updated_at FROM sessions ORDER BY updated_at DESC LIMIT 50",
        )?;

        let summaries = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let created: String = row.get(1)?;
            let updated: String = row.get(2)?;
            Ok(SessionSummary {
                id: SessionId::from_str(&id).unwrap_or_default(),
                created_at: created,
                updated_at: updated,
            })
        })?;

        summaries.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_session(&self, id: SessionId) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![id.to_string()],
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
