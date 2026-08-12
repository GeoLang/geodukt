//! Run records and the sqlite database holding them.
//!
//! One row per run: the id and the caller's subject as columns, because the
//! routes order by the first and filter by the second, and the record itself as
//! JSON, because nothing queries inside it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Env var naming the sqlite file the run history is kept in. Unset means an
/// in-memory database, so a restart starts an empty history.
pub const RUNS_DB_ENV: &str = "GEODUKT_RUNS_DB";

/// How long a write waits for another process to finish before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
            Some(path) => {
                let connection = Connection::open(path)?;
                // a file can be open in more than one process, so readers must
                // not block the writer and a writer must wait its turn rather
                // than fail on the spot
                connection.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
                connection.busy_timeout(BUSY_TIMEOUT)?;
                connection
            }
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
        // sqlite hands out the id, so two processes writing to one file cannot
        // pick the same one. The record carries its id, which is only known
        // once the row exists, so the row is written and then filled in.
        let transaction = connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO runs (sub, record) VALUES (?1, '')",
            params![sub],
        )?;
        let row_id = transaction.last_insert_rowid();
        let record = RunRecord {
            id: usize::try_from(row_id).unwrap_or(usize::MAX),
            status,
            manifest_name,
            manifest,
            steps,
            started_at,
            finished_at: now_rfc3339(),
            sub,
        };
        transaction.execute(
            "UPDATE runs SET record = ?1 WHERE id = ?2",
            params![encode(&record)?, row_id],
        )?;
        transaction.commit()?;
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
        assert_eq!(stored.id, 2);
        assert_eq!(stored.started_at, "2026-08-12T09:00:00.000Z");
        chrono::DateTime::parse_from_rfc3339(&stored.finished_at).unwrap();

        let reopened = RunStore::open(Some(path)).unwrap();
        let runs = reopened.list(None).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, 1);
        assert_eq!(runs[0].manifest_name, "first");
        assert_eq!(runs[0].steps[0].feature_count, 7);
        assert_eq!(runs[1].id, 2);
        assert_eq!(runs[1].started_at, stored.started_at);
        assert_eq!(runs[1].finished_at, stored.finished_at);

        // ids carry on from what is already stored
        let third = record_for(&reopened, "third", None);
        assert_eq!(third.id, 3);
    }

    // several replicas on one file, which is what each own connection stands
    // in for. Every write has to land with an id of its own: allocating from
    // MAX(id)+1 handed two writers the same one and the loser answered 500.
    #[test]
    fn concurrent_writers_on_one_file_all_get_their_own_id() {
        const WRITERS: usize = 4;
        const RUNS_EACH: usize = 25;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.sqlite");
        let path = path.to_str().unwrap().to_string();

        RunStore::open(Some(&path)).unwrap();

        let written: Vec<usize> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..WRITERS)
                .map(|writer| {
                    let path = path.clone();
                    scope.spawn(move || {
                        let store = RunStore::open(Some(&path)).unwrap();
                        (0..RUNS_EACH)
                            .map(|run| record_for(&store, &format!("w{writer}-r{run}"), None).id)
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect()
        });

        let unique: std::collections::HashSet<_> = written.iter().collect();
        assert_eq!(unique.len(), WRITERS * RUNS_EACH);

        let stored = RunStore::open(Some(&path)).unwrap().list(None).unwrap();
        assert_eq!(stored.len(), WRITERS * RUNS_EACH);
    }

    #[test]
    fn a_subject_reads_only_its_own_runs() {
        let store = RunStore::open(None).unwrap();
        record_for(&store, "mine", Some("user-a"));
        let theirs = record_for(&store, "theirs", Some("user-b")).id;
        let nobodys = record_for(&store, "nobodys", None).id;

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

        assert!(store.get(theirs, Some("user-a")).unwrap().is_none());
        assert!(store.get(theirs, Some("user-b")).unwrap().is_some());
        assert!(store.get(theirs, None).unwrap().is_some());
        // a run with no subject belongs to nobody, so no subject reads it
        assert!(store.get(nobodys, Some("user-a")).unwrap().is_none());
        assert!(store.get(9, None).unwrap().is_none());
        assert!(store.get(usize::MAX, None).unwrap().is_none());
    }
}
