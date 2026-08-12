//! Run records and the sqlite database holding them.
//!
//! One row per run: the id and the caller's subject as columns, because the
//! routes order by the first and filter by the second, and the record itself as
//! JSON, because nothing queries inside it.

use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Env var naming the sqlite file the run history is kept in. Unset means an
/// in-memory database, so a restart starts an empty history.
pub const RUNS_DB_ENV: &str = "GEODUKT_RUNS_DB";

/// Record of a pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: usize,
    pub status: RunStatus,
    pub manifest_name: String,
    /// The manifest TOML exactly as submitted, so the run can be repeated.
    pub manifest: String,
    pub steps: Vec<StepRecord>,
    /// RFC 3339 UTC, read before the pipeline started.
    pub started_at: String,
    /// RFC 3339 UTC, read when the run ended and the record was stored.
    pub finished_at: String,
    /// Token subject that triggered the run, absent when auth is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
}

/// Step record for API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub name: String,
    pub feature_count: usize,
    /// Absent from records stored before failed runs kept their steps, and
    /// those only ever came from runs that completed.
    #[serde(default)]
    pub status: StepStatus,
}

/// How a single step ended.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum StepStatus {
    #[default]
    Completed,
    Failed(String),
    NotRun,
}

/// Pipeline run status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed(String),
}

/// The run history. Cloning it shares the one connection.
#[derive(Clone)]
pub struct RunStore {
    connection: Arc<Mutex<Connection>>,
}

impl RunStore {
    /// Open the database named by [`RUNS_DB_ENV`], or an in-memory one when it
    /// is unset.
    pub fn from_env() -> rusqlite::Result<Self> {
        let path = std::env::var(RUNS_DB_ENV)
            .ok()
            .filter(|path| !path.is_empty());
        Self::open(path.as_deref())
    }

    pub fn open(path: Option<&str>) -> rusqlite::Result<Self> {
        let connection = match path {
            Some(path) => Connection::open(path)?,
            None => Connection::open_in_memory()?,
        };
        connection.execute(
            "CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY,
                sub TEXT,
                record TEXT NOT NULL
            )",
            [],
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Append a run attempt, completed or failed, and hand back the stored
    /// record. Called once the run has ended, so it stamps `finished_at`.
    pub fn record(
        &self,
        status: RunStatus,
        manifest_name: String,
        manifest: String,
        steps: Vec<StepRecord>,
        sub: Option<String>,
        started_at: String,
    ) -> rusqlite::Result<RunRecord> {
        let connection = self.connection.lock().unwrap();
        let id: usize =
            connection.query_row("SELECT COALESCE(MAX(id) + 1, 0) FROM runs", [], |row| {
                row.get(0)
            })?;
        let record = RunRecord {
            id,
            status,
            manifest_name,
            manifest,
            steps,
            started_at,
            finished_at: now_rfc3339(),
            sub,
        };
        connection.execute(
            "INSERT INTO runs (id, sub, record) VALUES (?1, ?2, ?3)",
            params![id, record.sub, encode(&record)?],
        )?;
        Ok(record)
    }

    /// Every run carrying `subject`, oldest first. `None` reads all of them.
    pub fn list(&self, subject: Option<&str>) -> rusqlite::Result<Vec<RunRecord>> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection
            .prepare("SELECT record FROM runs WHERE (?1 IS NULL OR sub = ?1) ORDER BY id")?;
        let rows = statement.query_map([subject], |row| row.get::<_, String>(0))?;
        rows.map(|text| decode(&text?)).collect()
    }

    /// The run with this id, when it carries `subject`. `None` reads all of them.
    pub fn get(&self, id: usize, subject: Option<&str>) -> rusqlite::Result<Option<RunRecord>> {
        // sqlite counts ids in i64, so a bigger one is missing rather than a
        // value the query could take
        let Ok(id) = i64::try_from(id) else {
            return Ok(None);
        };
        let connection = self.connection.lock().unwrap();
        let stored = connection
            .query_row(
                "SELECT record FROM runs WHERE id = ?1 AND (?2 IS NULL OR sub = ?2)",
                params![id, subject],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        stored.map(|text| decode(&text)).transpose()
    }
}

pub(crate) fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn encode(record: &RunRecord) -> rusqlite::Result<String> {
    serde_json::to_string(record).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn decode(stored: &str) -> rusqlite::Result<RunRecord> {
    serde_json::from_str(stored)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, Type::Text, Box::new(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_for(store: &RunStore, name: &str, sub: Option<&str>) -> RunRecord {
        store
            .record(
                RunStatus::Completed,
                name.to_string(),
                format!("[project]\nname = \"{name}\"\n"),
                vec![StepRecord {
                    name: "src".to_string(),
                    feature_count: 7,
                    status: StepStatus::Completed,
                }],
                sub.map(str::to_string),
                "2026-08-12T09:00:00.000Z".to_string(),
            )
            .unwrap()
    }

    #[test]
    fn a_record_survives_reopening_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.sqlite");
        let path = path.to_str().unwrap();

        let stored = {
            let store = RunStore::open(Some(path)).unwrap();
            record_for(&store, "first", Some("user-a"));
            record_for(&store, "second", None)
        };
        assert_eq!(stored.id, 1);
        assert_eq!(stored.started_at, "2026-08-12T09:00:00.000Z");
        chrono::DateTime::parse_from_rfc3339(&stored.finished_at).unwrap();

        let reopened = RunStore::open(Some(path)).unwrap();
        let runs = reopened.list(None).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, 0);
        assert_eq!(runs[0].manifest_name, "first");
        assert_eq!(runs[0].steps[0].feature_count, 7);
        assert_eq!(runs[1].id, 1);
        assert_eq!(runs[1].started_at, stored.started_at);
        assert_eq!(runs[1].finished_at, stored.finished_at);

        // ids carry on from what is already stored
        let third = record_for(&reopened, "third", None);
        assert_eq!(third.id, 2);
    }

    #[test]
    fn a_subject_reads_only_its_own_runs() {
        let store = RunStore::open(None).unwrap();
        record_for(&store, "mine", Some("user-a"));
        record_for(&store, "theirs", Some("user-b"));
        record_for(&store, "nobodys", None);

        let names = |subject| {
            store
                .list(subject)
                .unwrap()
                .into_iter()
                .map(|run| run.manifest_name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(Some("user-a")), ["mine"]);
        assert_eq!(names(None), ["mine", "theirs", "nobodys"]);

        assert!(store.get(1, Some("user-a")).unwrap().is_none());
        assert!(store.get(1, Some("user-b")).unwrap().is_some());
        assert!(store.get(1, None).unwrap().is_some());
        // a run with no subject belongs to nobody, so no subject reads it
        assert!(store.get(2, Some("user-a")).unwrap().is_none());
        assert!(store.get(9, None).unwrap().is_none());
        assert!(store.get(usize::MAX, None).unwrap().is_none());
    }
}
