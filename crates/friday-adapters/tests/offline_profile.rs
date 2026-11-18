use std::{sync::Arc, time::Duration};

use friday_adapters::SqliteStore;
use friday_core::{Action, ActionOutcome, OutputVolume};
use friday_engine::{
    ActionPermit, Engine, Intent, Phase, PlatformActions, PortFuture, Ports, SessionStatus, Store,
};
use uuid::Uuid;

struct ConfirmedActions;

impl PlatformActions for ConfirmedActions {
    fn execute(&self, action: Action, permit: ActionPermit) -> PortFuture<'_, ActionOutcome> {
        Box::pin(async move {
            permit.check()?;
            Ok(match action {
                Action::OpenApp { .. } => ActionOutcome::AcceptedByOs,
                Action::OutputVolume { .. } => ActionOutcome::VolumeApplied {
                    volume: OutputVolume::try_from(30).unwrap(),
                    muted: false,
                },
            })
        })
    }
}

#[tokio::test]
async fn offline_turns_commit_effects_and_opt_in_history_across_a_restart() {
    let profile = tempfile::tempdir().unwrap();
    let (store, mut settings) = SqliteStore::open_profile(profile.path()).unwrap();
    settings.history_enabled = true;
    store.save_settings(settings.clone()).await.unwrap();
    let engine = Engine::start(
        Ports {
            platform: Arc::new(ConfirmedActions),
            store: Arc::new(store),
            audio: None,
            cloud: None,
            speech: None,
        },
        settings,
        SessionStatus::Unlocked,
    );
    let handle = engine.handle();
    let mut updates = handle.subscribe();
    let mut last_receipts = Vec::new();
    for _ in 0..128 {
        let turn_id = handle
            .submit(
                Uuid::new_v4(),
                Intent::SubmitText {
                    text: "open calculator and volume down".into(),
                },
            )
            .await
            .unwrap()
            .turn_id;
        let finished = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = handle.snapshot();
                if current.turn_id == Some(turn_id) && !current.phase.active() {
                    break current;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(finished.phase, Phase::Completed);
        assert_eq!(finished.receipts.len(), 2);
        last_receipts = finished.receipts;
    }
    engine.shutdown().await;
    drop(handle);

    let (reopened, settings) = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(profile) = SqliteStore::open_profile(profile.path()) {
                break profile;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(settings.history_enabled);
    assert_eq!(
        reopened.history(1).await.unwrap()[0].receipts,
        last_receipts
    );
    assert_eq!(reopened.unresolved_actions().await.unwrap(), 0);

    let db = rusqlite::Connection::open(profile.path().join("profile.sqlite3")).unwrap();
    let completed: i64 = db
        .query_row(
            "SELECT count(*) FROM action_receipts WHERE state IN ('accepted_by_os', 'volume_applied')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(completed, 256);
}
