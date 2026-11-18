use std::{
    fs::{File, OpenOptions},
    path::Path,
    time::Duration,
};

use friday_core::ActionOutcome;
use friday_engine::{
    Failure, HISTORY_PAGE, HistoryEntry, PortFuture, Settings, Store, unix_seconds,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;
const KEEP_RECEIPTS_SECONDS: i64 = 7 * 24 * 3600;
const KEEP_HISTORY_SECONDS: i64 = 30 * 24 * 3600;
const MAX_HISTORY: i64 = 10_000;
const MAX_RECEIPTS: i64 = 10_000;

type Operation = Box<dyn FnOnce(&mut Connection) + Send>;

pub struct SqliteStore {
    queue: mpsc::Sender<Operation>,
}

impl SqliteStore {
    pub fn open_profile(directory: &Path) -> Result<(Self, Settings), Failure> {
        std::fs::create_dir_all(directory).map_err(|_| Failure::Storage)?;
        let guard = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("profile.lock"))
            .map_err(|_| Failure::Storage)?;
        guard.try_lock().map_err(|_| Failure::Storage)?;
        Self::open(&directory.join("profile.sqlite3"), guard)
    }

    fn open(path: &Path, guard: File) -> Result<(Self, Settings), Failure> {
        let mut db = Connection::open(path).map_err(|_| Failure::Storage)?;
        db.busy_timeout(Duration::from_millis(500))
            .map_err(|_| Failure::Storage)?;
        let version = db
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(|_| Failure::Storage)?;
        if version > SCHEMA_VERSION {
            return Err(Failure::Storage);
        }
        db.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA synchronous = FULL;
             PRAGMA secure_delete = ON;",
        )
        .map_err(|_| Failure::Storage)?;
        {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| Failure::Storage)?;
            if version == 0 {
                tx.execute_batch(include_str!("../migrations/0001_profile.sql"))
                    .map_err(|_| Failure::Storage)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(|_| Failure::Storage)?;
            }
            tx.execute(
                "UPDATE action_receipts SET state = 'indeterminate' WHERE state = 'dispatching'",
                [],
            )
            .map_err(|_| Failure::Storage)?;
            prune_history(&tx)?;
            prune_receipts(&tx)?;
            tx.commit().map_err(|_| Failure::Storage)?;
        }

        let stored: Option<String> = db
            .query_row("SELECT value FROM settings WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|_| Failure::Storage)?;
        let settings = stored
            .map(|value| serde_json::from_str::<Settings>(&value))
            .transpose()
            .map_err(|_| Failure::Storage)?
            .unwrap_or_default();
        settings.validate().map_err(|_| Failure::Storage)?;

        let (queue, mut receiver) = mpsc::channel::<Operation>(32);
        std::thread::Builder::new()
            .name("friday-sqlite".into())
            .spawn(move || {
                let _guard = guard;
                while let Some(operation) = receiver.blocking_recv() {
                    operation(&mut db);
                }
                drop(db);
            })
            .map_err(|_| Failure::Storage)?;
        Ok((Self { queue }, settings))
    }

    pub async fn unresolved_actions(&self) -> Result<i64, Failure> {
        self.run(|db| {
            db.query_row(
                "SELECT count(*) FROM action_receipts WHERE state = 'indeterminate'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| Failure::Storage)
        })
        .await
    }

    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, Failure> + Send + 'static,
    ) -> Result<T, Failure> {
        let (send, receive) = oneshot::channel();
        self.queue
            .try_send(Box::new(move |db| {
                if !send.is_closed() {
                    let _ = send.send(f(db));
                }
            }))
            .map_err(|_| Failure::Storage)?;
        receive.await.map_err(|_| Failure::Storage)?
    }
}

fn prune_history(db: &Connection) -> Result<(), Failure> {
    db.execute(
        "DELETE FROM history WHERE created_at < ?1 OR turn_id IN
            (SELECT turn_id FROM history ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?2)",
        params![
            unix_seconds().saturating_sub(KEEP_HISTORY_SECONDS),
            MAX_HISTORY
        ],
    )
    .map_err(|_| Failure::Storage)?;
    Ok(())
}

fn prune_receipts(db: &Connection) -> Result<(), Failure> {
    db.execute(
        "DELETE FROM action_receipts WHERE state <> 'dispatching' AND
         (created_at < ?1 OR action_id IN (SELECT action_id FROM action_receipts
           WHERE state <> 'dispatching' ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?2))",
        params![
            unix_seconds().saturating_sub(KEEP_RECEIPTS_SECONDS),
            MAX_RECEIPTS
        ],
    )
    .map_err(|_| Failure::Storage)?;
    Ok(())
}

impl Store for SqliteStore {
    fn save_settings(&self, settings: Settings) -> PortFuture<'_, ()> {
        Box::pin(self.run(move |db| {
            let value = serde_json::to_string(&settings).map_err(|_| Failure::Storage)?;
            db.execute(
                "INSERT INTO settings (id, value) VALUES (1, ?1)
                 ON CONFLICT (id) DO UPDATE SET value = excluded.value",
                [value],
            )
            .map_err(|_| Failure::Storage)?;
            Ok(())
        }))
    }

    fn record_dispatch(&self, id: Uuid, kind: &'static str) -> PortFuture<'_, bool> {
        Box::pin(self.run(move |db| {
            db.execute(
                "INSERT INTO action_receipts (action_id, created_at, kind, state)
                 VALUES (?1, ?2, ?3, 'dispatching') ON CONFLICT (action_id) DO NOTHING",
                params![id.to_string(), unix_seconds(), kind],
            )
            .map(|affected| affected == 1)
            .map_err(|_| Failure::Storage)
        }))
    }

    fn record_outcome(&self, id: Uuid, outcome: ActionOutcome) -> PortFuture<'_, ()> {
        let state = match outcome {
            ActionOutcome::AcceptedByOs => "accepted_by_os",
            ActionOutcome::VolumeApplied { .. } => "volume_applied",
            ActionOutcome::Blocked => "blocked",
            ActionOutcome::Failed => "failed",
            ActionOutcome::Indeterminate => "indeterminate",
        };
        Box::pin(self.run(move |db| {
            let tx = db.transaction().map_err(|_| Failure::Storage)?;
            let modified = tx.execute(
                "UPDATE action_receipts SET state = ?1 WHERE action_id = ?2 AND state = 'dispatching'",
                params![state, id.to_string()],
            ).map_err(|_| Failure::Storage)?;
            if modified != 1 {
                return Err(Failure::Storage);
            }
            prune_receipts(&tx)?;
            tx.commit().map_err(|_| Failure::Storage)
        }))
    }

    fn save_history(&self, entry: HistoryEntry) -> PortFuture<'_, ()> {
        Box::pin(self.run(move |db| {
            let receipts = serde_json::to_string(&entry.receipts).map_err(|_| Failure::Storage)?;
            let tx = db.transaction().map_err(|_| Failure::Storage)?;
            tx.execute(
                "INSERT INTO history (turn_id, created_at, input, answer, receipts)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    entry.turn_id.to_string(),
                    entry.created_at,
                    entry.input,
                    entry.answer,
                    receipts
                ],
            )
            .map_err(|_| Failure::Storage)?;
            prune_history(&tx)?;
            tx.commit().map_err(|_| Failure::Storage)
        }))
    }

    fn history(&self, limit: usize) -> PortFuture<'_, Vec<HistoryEntry>> {
        Box::pin(self.run(move |db| {
            let mut query = db
                .prepare(
                    "SELECT turn_id, created_at, input, answer, receipts FROM history
                 WHERE created_at >= ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
                )
                .map_err(|_| Failure::Storage)?;
            let mut rows = query
                .query(params![
                    unix_seconds().saturating_sub(KEEP_HISTORY_SECONDS),
                    limit.min(HISTORY_PAGE) as i64,
                ])
                .map_err(|_| Failure::Storage)?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next().map_err(|_| Failure::Storage)? {
                let turn_id: String = row.get(0).map_err(|_| Failure::Storage)?;
                let receipts: String = row.get(4).map_err(|_| Failure::Storage)?;
                entries.push(HistoryEntry {
                    turn_id: Uuid::parse_str(&turn_id).map_err(|_| Failure::Storage)?,
                    created_at: row.get(1).map_err(|_| Failure::Storage)?,
                    input: row.get(2).map_err(|_| Failure::Storage)?,
                    answer: row.get(3).map_err(|_| Failure::Storage)?,
                    receipts: serde_json::from_str(&receipts).map_err(|_| Failure::Storage)?,
                });
            }
            Ok(entries)
        }))
    }

    fn clear_history(&self) -> PortFuture<'_, ()> {
        Box::pin(self.run(|db| {
            db.execute("DELETE FROM history", [])
                .map_err(|_| Failure::Storage)?;
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn profile_serializes_access_and_recovers_an_interrupted_dispatch() {
        let directory = tempfile::tempdir().unwrap();
        let (store, settings) = SqliteStore::open_profile(directory.path()).unwrap();
        assert!(!settings.history_enabled);
        assert!(SqliteStore::open_profile(directory.path()).is_err());
        let id = Uuid::new_v4();
        assert!(store.record_dispatch(id, "open_app").await.unwrap());
        assert!(!store.record_dispatch(id, "open_app").await.unwrap());
        assert_eq!(store.unresolved_actions().await.unwrap(), 0);
        drop(store);

        let (reopened, _) = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(store) = SqliteStore::open_profile(directory.path()) {
                    break store;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(reopened.unresolved_actions().await.unwrap(), 1);
        assert!(!reopened.record_dispatch(id, "open_app").await.unwrap());
    }

    #[tokio::test]
    async fn history_is_bounded_and_can_be_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let (store, _) = SqliteStore::open_profile(directory.path()).unwrap();
        for (timestamp, answer) in [(0, "expired"), (unix_seconds(), "still here")] {
            store
                .save_history(HistoryEntry {
                    turn_id: Uuid::new_v4(),
                    created_at: timestamp,
                    input: "hello".into(),
                    answer: Some(answer.into()),
                    receipts: Vec::new(),
                })
                .await
                .unwrap();
        }
        let history = store.history(1000).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].answer.as_deref(), Some("still here"));
        store.clear_history().await.unwrap();
        assert!(store.history(100).await.unwrap().is_empty());
    }
}
