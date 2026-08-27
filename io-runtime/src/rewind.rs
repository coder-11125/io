use std::sync::Arc;
use tokio::sync::Mutex;

use crate::memory::SessionStore;
use crate::types::Session;

pub struct RewindResult {
    pub turns_dropped: usize,
    pub files_restored: Vec<String>,
    /// Paths written/edited in the reverted range with no snapshot to
    /// restore from (skipped for exceeding `tools::MAX_SNAPSHOT_BYTES`).
    pub files_skipped: Vec<String>,
}

/// Restore files to their state right before `turn_index`, then drop that
/// turn and every turn after it from the session.
pub async fn run(
    session: &Arc<Mutex<Session>>,
    memory: &Arc<SessionStore>,
    turn_index: usize,
) -> anyhow::Result<RewindResult> {
    let turns = {
        let s = session.lock().await;
        s.turns.clone()
    };

    if turn_index >= turns.len() {
        anyhow::bail!(
            "turn index {turn_index} out of range — session has {} turn(s)",
            turns.len()
        );
    }

    // Walk the reverted range in order, keeping only the first snapshot seen
    // per path — that's the file's state right before the reverted range
    // began. Later snapshots in the range are intermediate states.
    let mut restore: Vec<(String, Option<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut skipped: Vec<String> = Vec::new();

    for turn in &turns[turn_index..] {
        for tc in &turn.tool_calls {
            if !tc.success || (tc.tool_name != "write" && tc.tool_name != "edit") {
                continue;
            }
            let Some(path) = tc.input.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            if !seen.insert(path.to_string()) {
                continue;
            }
            match &tc.pre_edit_snapshot {
                Some(snap) => restore.push((snap.path.clone(), snap.prior_content.clone())),
                None => skipped.push(path.to_string()),
            }
        }
    }

    let mut files_restored = Vec::new();
    for (path, prior_content) in &restore {
        let result = match prior_content {
            Some(content) => std::fs::write(path, content),
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            },
        };
        match result {
            Ok(()) => files_restored.push(path.clone()),
            Err(e) => tracing::warn!("rewind: failed to restore {path}: {e}"),
        }
    }

    let turns_dropped = turns.len() - turn_index;

    let session_to_save = {
        let mut s = session.lock().await;
        s.turns.truncate(turn_index);
        s.updated_at = chrono::Utc::now();
        s.clone()
    };

    if let Err(e) = memory.save_session(&session_to_save).await {
        tracing::warn!("failed to save session after rewind: {e}");
    }

    Ok(RewindResult {
        turns_dropped,
        files_restored,
        files_skipped: skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::FileSnapshot;
    use crate::types::Turn;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("io_rewind_test_{}_{}", uuid::Uuid::new_v4(), name))
    }

    fn tool_call(
        tool_name: &str,
        path: &str,
        prior_content: Option<Option<&str>>,
    ) -> crate::types::ToolCallRecord {
        crate::types::ToolCallRecord {
            tool_name: tool_name.to_string(),
            input: serde_json::json!({ "path": path }),
            output: String::new(),
            success: true,
            duration_ms: 0,
            pre_edit_snapshot: prior_content.map(|c| FileSnapshot {
                path: path.to_string(),
                prior_content: c.map(|s| s.to_string()),
            }),
        }
    }

    fn turn(tool_calls: Vec<crate::types::ToolCallRecord>) -> Turn {
        Turn {
            id: uuid::Uuid::new_v4(),
            timestamp: chrono::Utc::now(),
            user_message: "do it".to_string(),
            assistant_message: Some("done".to_string()),
            tool_calls,
            usage: None,
        }
    }

    async fn session_with_turns(turns: Vec<Turn>) -> (Arc<Mutex<Session>>, Arc<SessionStore>) {
        let mut session = Session::new("mock-model".to_string(), "mock".to_string());
        session.turns = turns;
        let memory = Arc::new(SessionStore::with_path(temp_path("session.db")).unwrap());
        memory.save_session(&session).await.unwrap();
        (Arc::new(Mutex::new(session)), memory)
    }

    #[tokio::test]
    async fn restores_file_to_state_before_reverted_range() {
        let path = temp_path("file.txt");
        std::fs::write(&path, "v2").unwrap();
        let path_str = path.to_str().unwrap();

        let turns = vec![
            turn(vec![tool_call("write", path_str, Some(None))]), // turn 0: created, no prior content
            turn(vec![tool_call("edit", path_str, Some(Some("v1")))]), // turn 1: v1 -> v2
        ];
        let (session, memory) = session_with_turns(turns).await;

        let result = run(&session, &memory, 1).await.unwrap();

        assert_eq!(result.turns_dropped, 1);
        assert_eq!(result.files_restored, vec![path_str.to_string()]);
        assert!(result.files_skipped.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");
        assert_eq!(session.lock().await.turns.len(), 1);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn deletes_file_that_did_not_exist_before_the_range() {
        let path = temp_path("new_file.txt");
        std::fs::write(&path, "created this turn").unwrap();
        let path_str = path.to_str().unwrap();

        let turns = vec![turn(vec![tool_call("write", path_str, Some(None))])];
        let (session, memory) = session_with_turns(turns).await;

        let result = run(&session, &memory, 0).await.unwrap();

        assert_eq!(result.files_restored, vec![path_str.to_string()]);
        assert!(
            !path.exists(),
            "file created in reverted range should be deleted"
        );
        assert!(session.lock().await.turns.is_empty());
    }

    #[tokio::test]
    async fn reports_files_with_no_snapshot_as_skipped() {
        let path_str = "/tmp/io_rewind_never_snapshotted.txt";
        let turns = vec![turn(vec![tool_call("write", path_str, None)])];
        let (session, memory) = session_with_turns(turns).await;

        let result = run(&session, &memory, 0).await.unwrap();

        assert!(result.files_restored.is_empty());
        assert_eq!(result.files_skipped, vec![path_str.to_string()]);
    }

    #[tokio::test]
    async fn out_of_range_turn_index_is_an_error() {
        let (session, memory) = session_with_turns(vec![turn(vec![])]).await;
        assert!(run(&session, &memory, 5).await.is_err());
    }
}
