use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use friday_core::{Action, ActionOutcome, KnownApp, Locale};
use friday_engine::{
    ActionPermit, AudioInput, Cloud, Engine, EngineHandle, Failure, HistoryEntry, Intent,
    JobContext, ModelReply, ModelRequest, PcmClip, Phase, PlatformActions, PortFuture, Ports,
    SessionStatus, Settings, Snapshot, Store, ToolProposal,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Default)]
struct FakeStore {
    history: Mutex<Vec<HistoryEntry>>,
    dispatched: Mutex<Vec<Uuid>>,
    outcomes: Mutex<Vec<(Uuid, ActionOutcome)>>,
}

impl Store for FakeStore {
    fn save_settings(&self, _: Settings) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn record_dispatch(&self, id: Uuid, _: &'static str) -> PortFuture<'_, bool> {
        Box::pin(async move {
            self.dispatched.lock().unwrap().push(id);
            Ok(true)
        })
    }

    fn record_outcome(&self, id: Uuid, outcome: ActionOutcome) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.outcomes.lock().unwrap().push((id, outcome));
            Ok(())
        })
    }

    fn save_history(&self, entry: HistoryEntry) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.history.lock().unwrap().push(entry);
            Ok(())
        })
    }

    fn history(&self, limit: usize) -> PortFuture<'_, Vec<HistoryEntry>> {
        Box::pin(async move {
            Ok(self
                .history
                .lock()
                .unwrap()
                .iter()
                .take(limit)
                .cloned()
                .collect())
        })
    }

    fn clear_history(&self) -> PortFuture<'_, ()> {
        Box::pin(async move {
            self.history.lock().unwrap().clear();
            Ok(())
        })
    }
}

#[derive(Default)]
struct FakePlatform(Mutex<Vec<Action>>);

impl PlatformActions for FakePlatform {
    fn execute(&self, action: Action, permit: ActionPermit) -> PortFuture<'_, ActionOutcome> {
        Box::pin(async move {
            permit.check()?;
            self.0.lock().unwrap().push(action);
            Ok(ActionOutcome::AcceptedByOs)
        })
    }
}

struct HeldMic {
    entered: Notify,
    release: Notify,
}

impl AudioInput for HeldMic {
    fn capture(
        &self,
        finish: CancellationToken,
        context: JobContext,
        started: tokio::sync::oneshot::Sender<()>,
    ) -> PortFuture<'_, PcmClip> {
        Box::pin(async move {
            let _ = started.send(());
            self.entered.notify_one();
            tokio::select! {
                _ = context.token().cancelled() => {
                    self.release.notified().await;
                    Err(Failure::Cancelled)
                }
                _ = finish.cancelled() => PcmClip::new(vec![100; 1600]),
            }
        })
    }
}

#[derive(Default)]
struct FakeCloud {
    model_rounds: Mutex<Vec<usize>>,
}

impl Cloud for FakeCloud {
    fn ready(&self) -> PortFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn transcribe(&self, _: PcmClip, _: Locale, _: JobContext) -> PortFuture<'_, String> {
        Box::pin(async { Ok("Открой калькулятор".into()) })
    }

    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        _: JobContext,
    ) -> PortFuture<'a, ModelReply> {
        Box::pin(async move {
            self.model_rounds.lock().unwrap().push(request.tool_budget);
            if let Some(results) = request.rounds.last() {
                assert!(results[0].outcome.completed());
                return Ok(ModelReply::Answer(
                    "Unverified prose about the action.".into(),
                ));
            }
            if request.tool_budget == 0 {
                return Ok(ModelReply::Answer("A text response".into()));
            }
            Ok(ModelReply::Tools(vec![ToolProposal {
                id: "call_first".into(),
                action: Action::OpenApp {
                    app: KnownApp::Calculator,
                },
            }]))
        })
    }
}

async fn ended(handle: &EngineHandle) -> Snapshot {
    let mut subscription = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = handle.snapshot();
            if !snapshot.phase.active() && snapshot.turn_id.is_some() {
                return snapshot;
            }
            subscription.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

fn text(text: &str) -> Intent {
    Intent::SubmitText { text: text.into() }
}

#[tokio::test]
async fn local_commands_run_in_order_offline_and_ipc_replay_is_rejected() {
    let platform = Arc::new(FakePlatform::default());
    let store = Arc::new(FakeStore::default());
    let engine = Engine::start(
        Ports {
            platform: platform.clone(),
            store: store.clone(),
            audio: None,
            cloud: None,
            speech: None,
        },
        Settings::default(),
        SessionStatus::Unlocked,
    );
    let handle = engine.handle();
    let id = Uuid::new_v4();
    handle
        .submit(id, text("открой браузер и сделай тише"))
        .await
        .unwrap();
    let result = ended(&handle).await;
    assert_eq!(result.phase, Phase::Completed);
    assert_eq!(result.receipts.len(), 2);
    assert_eq!(
        *platform.0.lock().unwrap(),
        result.receipts.iter().map(|r| r.action).collect::<Vec<_>>()
    );
    assert_eq!(store.dispatched.lock().unwrap().len(), 2);
    assert_eq!(store.outcomes.lock().unwrap().len(), 2);
    assert!(handle.history().await.unwrap().is_empty());
    assert_eq!(
        handle.submit(id, text("открой браузер")).await,
        Err(Failure::Duplicate)
    );
    handle
        .submit(Uuid::new_v4(), text("открой браузер и неизвестный шаг"))
        .await
        .unwrap();
    assert_eq!(ended(&handle).await.failure, Some(Failure::Clarify));
    assert_eq!(store.dispatched.lock().unwrap().len(), 2);
    engine.shutdown().await;
}

#[tokio::test]
async fn cancelled_capture_retains_its_lease_until_the_device_releases_it() {
    let audio = Arc::new(HeldMic {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let engine = Engine::start(
        Ports {
            platform: Arc::new(FakePlatform::default()),
            store: Arc::new(FakeStore::default()),
            audio: Some(audio.clone()),
            cloud: Some(Arc::new(FakeCloud::default())),
            speech: None,
        },
        Settings {
            cloud_enabled: true,
            ..Settings::default()
        },
        SessionStatus::Unknown,
    );
    let handle = engine.handle();
    assert_eq!(
        handle.submit(Uuid::new_v4(), Intent::StartCapture).await,
        Err(Failure::Locked)
    );
    handle.set_session(SessionStatus::Unlocked);
    let turn = handle
        .submit(Uuid::new_v4(), Intent::StartCapture)
        .await
        .unwrap();
    audio.entered.notified().await;
    handle.set_session(SessionStatus::Locked);
    handle.set_session(SessionStatus::Unlocked);
    assert_eq!(
        handle.submit(Uuid::new_v4(), Intent::StartCapture).await,
        Err(Failure::Busy)
    );
    audio.release.notify_one();
    assert_eq!(ended(&handle).await.phase, Phase::Cancelled);
    assert_eq!(
        handle.finish_capture(turn.turn_id),
        Err(Failure::InvalidTurn)
    );
    let next = handle
        .submit(Uuid::new_v4(), Intent::StartCapture)
        .await
        .unwrap();
    handle.finish_capture(next.turn_id).unwrap();
    assert_eq!(ended(&handle).await.receipts.len(), 1);
    engine.shutdown().await;
}

#[tokio::test]
async fn model_gets_tools_only_for_action_requests_and_receipts_are_authoritative() {
    let cloud = Arc::new(FakeCloud::default());
    let engine = Engine::start(
        Ports {
            platform: Arc::new(FakePlatform::default()),
            store: Arc::new(FakeStore::default()),
            audio: None,
            cloud: Some(cloud.clone()),
            speech: None,
        },
        Settings {
            cloud_enabled: true,
            history_enabled: true,
            ..Settings::default()
        },
        SessionStatus::Unlocked,
    );
    let handle = engine.handle();
    handle
        .submit(Uuid::new_v4(), text("why is the sky blue?"))
        .await
        .unwrap();
    assert_eq!(
        ended(&handle).await.answer.as_deref(),
        Some("A text response")
    );
    handle
        .submit(Uuid::new_v4(), text("could you open the calculator"))
        .await
        .unwrap();
    let result = ended(&handle).await;
    assert_eq!(result.phase, Phase::Completed);
    assert_eq!(result.receipts.len(), 1);
    assert!(result.answer.is_none());
    assert_eq!(*cloud.model_rounds.lock().unwrap(), [0, 4, 3]);
    assert_eq!(handle.history().await.unwrap().len(), 2);
    handle.set_session(SessionStatus::Locked);
    assert!(matches!(handle.history().await, Err(Failure::Locked)));
    assert!(handle.snapshot().receipts.is_empty());
    handle.set_session(SessionStatus::Unlocked);
    assert!(handle.snapshot().receipts.is_empty());
    handle.clear_history().await.unwrap();
    assert!(handle.history().await.unwrap().is_empty());
    engine.shutdown().await;
}

#[tokio::test]
async fn cancellation_after_a_dispatch_preserves_uncertainty_and_prevents_the_next_step() {
    struct Platform(Notify);

    impl PlatformActions for Platform {
        fn execute(&self, _: Action, permit: ActionPermit) -> PortFuture<'_, ActionOutcome> {
            Box::pin(async move {
                permit.check()?;
                self.0.notify_one();
                std::future::pending().await
            })
        }
    }

    let platform = Arc::new(Platform(Notify::new()));
    let store = Arc::new(FakeStore::default());
    let engine = Engine::start(
        Ports {
            platform: platform.clone(),
            store: store.clone(),
            audio: None,
            cloud: None,
            speech: None,
        },
        Settings::default(),
        SessionStatus::Unlocked,
    );
    let handle = engine.handle();
    let turn = handle
        .submit(Uuid::new_v4(), text("open browser and volume down"))
        .await
        .unwrap();
    platform.0.notified().await;
    handle.cancel(turn.turn_id).unwrap();
    let result = ended(&handle).await;
    assert_eq!(result.failure, Some(Failure::ActionUncertain));
    assert_eq!(result.receipts.len(), 1);
    assert_eq!(result.receipts[0].outcome, ActionOutcome::Indeterminate);
    assert_eq!(store.dispatched.lock().unwrap().len(), 1);
    engine.shutdown().await;
}

#[tokio::test]
async fn a_thousand_sequential_turns_keep_one_receipt_per_effect() {
    let store = Arc::new(FakeStore::default());
    let engine = Engine::start(
        Ports {
            platform: Arc::new(FakePlatform::default()),
            store: store.clone(),
            audio: None,
            cloud: None,
            speech: None,
        },
        Settings::default(),
        SessionStatus::Unlocked,
    );
    let handle = engine.handle();
    let mut receipts = std::collections::HashSet::new();
    for _ in 0..1000 {
        handle
            .submit(Uuid::new_v4(), text("открой калькулятор"))
            .await
            .unwrap();
        let state = ended(&handle).await;
        assert_eq!(state.phase, Phase::Completed);
        assert_eq!(state.receipts.len(), 1);
        assert!(receipts.insert(state.receipts[0].id));
    }
    assert_eq!(store.dispatched.lock().unwrap().len(), 1000);
    assert_eq!(store.outcomes.lock().unwrap().len(), 1000);
    engine.shutdown().await;
}
