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
            "SELECT id, created_at, updated_at,
                    json_extract(data, '$.metadata.name'),
                    json_extract(data, '$.turns[0].user_message'),
                    json_array_length(data, '$.turns'),
                    json_extract(data, '$.metadata.model')
             FROM sessions ORDER BY updated_at DESC LIMIT 50",
        )?;

        let summaries = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let created: String = row.get(1)?;
            let updated: String = row.get(2)?;
            let first: Option<String> = row.get(4)?;
            Ok(SessionSummary {
                id: SessionId::from_str(&id).unwrap_or_default(),
                created_at: created,
                updated_at: updated,
                name: row.get(3)?,
                first_message: first.map(|m| m.split_whitespace().collect::<Vec<_>>().join(" ")),
                turns: row.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as usize,
                model: row.get(6)?,
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
    /// Set with `/rename`.
    pub name: Option<String>,
    /// The first thing the user said, whitespace-collapsed.
    pub first_message: Option<String>,
    pub turns: usize,
    pub model: Option<String>,
}

impl SessionSummary {
    /// What to call this session in a list: its name, else how it started.
    pub fn title(&self) -> String {
        let raw = self
            .name
            .clone()
            .or_else(|| self.first_message.clone())
            .unwrap_or_else(|| "(empty session)".to_string());
        let mut title: String = raw.chars().take(60).collect();
        if raw.chars().count() > 60 {
            title.push('…');
        }
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Session, Turn};

    fn store() -> SessionStore {
        let path = std::env::temp_dir().join(format!("io_mem_{}.db", uuid::Uuid::new_v4()));
        SessionStore::with_path(path).unwrap()
    }

    fn turn(text: &str) -> Turn {
        Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: text.to_string(),
            assistant_message: Some("ok".to_string()),
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    #[tokio::test]
    async fn summaries_carry_a_title_turn_count_and_model() {
        let store = store();
        let mut named = Session::new("m1".into(), "p".into());
        named.metadata.name = Some("my refactor".into());
        named.add_turn(turn("first\n  thing"));
        named.add_turn(turn("second"));
        store.save_session(&named).await.unwrap();
        let mut unnamed = Session::new("m2".into(), "p".into());
        unnamed.add_turn(turn(&"long ".repeat(40)));
        store.save_session(&unnamed).await.unwrap();
        let empty = Session::new("m3".into(), "p".into());
        store.save_session(&empty).await.unwrap();

        let list = store.list_sessions().unwrap();
        let find = |id| list.iter().find(|s| s.id == id).unwrap();
        let s = find(named.id);
        assert_eq!((s.title().as_str(), s.turns), ("my refactor", 2));
        assert_eq!(s.first_message.as_deref(), Some("first thing"));
        assert_eq!(s.model.as_deref(), Some("m1"));
        let u = find(unnamed.id);
        assert!(u.title().starts_with("long long") && u.title().ends_with('…'));
        let e = find(empty.id);
        assert_eq!((e.title().as_str(), e.turns), ("(empty session)", 0));
    }

    #[tokio::test]
    async fn old_sessions_without_a_name_still_load() {
        let store = store();
        let s = Session::new("m".into(), "p".into());
        let mut json = serde_json::to_value(&s).unwrap();
        json["metadata"].as_object_mut().unwrap().remove("name");
        let loaded: Session = serde_json::from_value(json).unwrap();
        assert!(loaded.metadata.name.is_none());
        store.save_session(&loaded).await.unwrap();
        assert!(store.load_session(loaded.id).is_ok());
    }
}
