use std::{collections::VecDeque, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    Failure, HISTORY_PAGE, HistoryEntry, Phase, Ports, SessionStatus, Settings, Snapshot,
    control::{Active, Control},
    turn::{Turn, Update},
};

const REQUESTS: usize = 64;
const RECENT_REQUESTS: usize = 128;
const RELEASE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Intent {
    SubmitText { text: String },
    StartCapture,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RequestAck {
    pub turn_id: Uuid,
}

enum Request {
    Turn(Uuid, Intent, oneshot::Sender<Result<RequestAck, Failure>>),
    Settings(Settings, oneshot::Sender<Result<(), Failure>>),
    DeleteHistory(oneshot::Sender<Result<(), Failure>>),
}

pub struct Engine {
    handle: EngineHandle,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct EngineHandle {
    control: Arc<Control>,
    snapshot: watch::Receiver<Snapshot>,
    settings: watch::Receiver<Settings>,
    requests: mpsc::Sender<Request>,
    ports: Arc<Ports>,
}

impl Engine {
    pub fn start(ports: Ports, settings: Settings, session: SessionStatus) -> Self {
        let ports = Arc::new(ports);
        let control = Arc::new(Control::new(session));
        let (snapshot_tx, snapshot) = watch::channel(Snapshot {
            session,
            ..Snapshot::default()
        });
        let (settings_tx, current_settings) = watch::channel(settings);
        let (requests, request_rx) = mpsc::channel(REQUESTS);
        let handle = EngineHandle {
            control: control.clone(),
            snapshot,
            settings: current_settings,
            requests,
            ports: ports.clone(),
        };
        let task = tokio::spawn(
            Runtime {
                control,
                ports,
                snapshot: snapshot_tx,
                settings: settings_tx,
                requests: request_rx,
                accepted: VecDeque::new(),
            }
            .run(),
        );
        Self { handle, task }
    }

    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }

    pub async fn shutdown(self) {
        self.handle.control.shutdown.cancel();
        let _ = self.task.await;
    }
}

impl EngineHandle {
    pub fn set_session(&self, status: SessionStatus) {
        self.control.set_session(status);
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.snapshot.borrow().clone();
        let (session, epoch) = self.control.session();
        snapshot.session = session;
        if session != SessionStatus::Unlocked || snapshot.session_epoch != epoch {
            snapshot.redact();
        }
        snapshot
    }

    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }

    pub fn settings(&self) -> Settings {
        self.settings.borrow().clone()
    }

    pub fn finish_capture(&self, id: Uuid) -> Result<(), Failure> {
        self.control.stop_capture(id)
    }

    pub fn cancel(&self, id: Uuid) -> Result<(), Failure> {
        self.control.cancel(Some(id))
    }

    pub fn cancel_current(&self) {
        let _ = self.control.cancel(None);
    }

    pub fn ensure_unlocked(&self) -> Result<(), Failure> {
        self.control.authorized_epoch().map(|_| ())
    }

    pub async fn submit(&self, id: Uuid, intent: Intent) -> Result<RequestAck, Failure> {
        self.request(|reply| Request::Turn(id, intent, reply)).await
    }

    pub async fn update_settings(&self, settings: Settings) -> Result<(), Failure> {
        self.request(|reply| Request::Settings(settings, reply))
            .await
    }

    pub async fn clear_history(&self) -> Result<(), Failure> {
        self.request(Request::DeleteHistory).await
    }

    pub async fn history(&self) -> Result<Vec<HistoryEntry>, Failure> {
        let epoch = self.control.authorized_epoch()?;
        let entries = self.ports.store.history(HISTORY_PAGE).await?;
        self.control.check_epoch(epoch)?;
        Ok(entries)
    }

    async fn request<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, Failure>>) -> Request,
    ) -> Result<T, Failure> {
        self.control.authorized_epoch()?;
        let (reply, result) = oneshot::channel();
        self.requests
            .try_send(make(reply))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => Failure::Busy,
                mpsc::error::TrySendError::Closed(_) => Failure::Stopped,
            })?;
        result.await.unwrap_or(Err(Failure::Stopped))
    }
}

struct Runtime {
    control: Arc<Control>,
    ports: Arc<Ports>,
    snapshot: watch::Sender<Snapshot>,
    settings: watch::Sender<Settings>,
    requests: mpsc::Receiver<Request>,
    accepted: VecDeque<Uuid>,
}

impl Runtime {
    fn publish(&self, change: impl FnOnce(&mut Snapshot)) {
        self.snapshot.send_modify(|snapshot| {
            change(snapshot);
            snapshot.revision = snapshot.revision.wrapping_add(1);
            let (session, epoch) = self.control.session();
            snapshot.session = session;
            if session != SessionStatus::Unlocked || snapshot.session_epoch != epoch {
                snapshot.redact();
            }
            snapshot.session_epoch = epoch;
        });
    }

    async fn run(mut self) {
        let mut sessions = self.control.updates.subscribe();
        let (updates, mut effects) = mpsc::channel::<(Uuid, Update)>(32);
        let mut running: Option<(Active, JoinHandle<Update>)> = None;
        let mut release_by = None;
        let mut cancelling = false;

        loop {
            let active_cancel = running.as_ref().map(|(active, _)| active.cancel.clone());
            tokio::select! {
                biased;
                _ = self.control.shutdown.cancelled() => {
                    self.requests.close();
                    if let Some((_, job)) = running.take() {
                        let _ = tokio::time::timeout(Duration::from_secs(4), job).await;
                    }
                    return;
                }
                _ = sessions.changed() => {
                    sessions.borrow_and_update();
                    self.publish(Snapshot::redact);
                }
                _ = wait_cancel(active_cancel), if !cancelling => {
                    cancelling = true;
                    release_by = Some(tokio::time::Instant::now() + RELEASE_BUDGET);
                    self.publish(|snapshot| snapshot.phase = Phase::Cancelling);
                }
                _ = wait_release(release_by), if release_by.is_some() => {
                    // Keep the active lease until the device or task actually completes.
                    release_by = None;
                    self.publish(|snapshot| snapshot.failure = Some(Failure::AudioStalled));
                }
                Some((turn_id, update)) = effects.recv() => {
                    if running.as_ref().is_some_and(|(active, _)| active.id == turn_id && self.control.check_epoch(active.epoch).is_ok()) {
                        self.publish(|snapshot| update.apply(snapshot, cancelling));
                    }
                }
                result = wait_job(&mut running) => {
                    if let Some((active, _)) = running.take() {
                        self.control.end(active.id);
                        release_by = None;
                        cancelling = false;
                        self.publish(|snapshot| {
                            let update = result.unwrap_or_else(|_| Update::terminal(Err(Failure::Stopped)));
                            update.apply(snapshot, false);
                            if self.control.check_epoch(active.epoch).is_err() {
                                snapshot.redact();
                            }
                        });
                    }
                }
                Some(request) = self.requests.recv() => {
                    match request {
                        Request::Turn(id, intent, reply) => {
                            let accepted = self.start(id, intent, &mut running, updates.clone());
                            let _ = reply.send(accepted);
                        }
                        Request::Settings(settings, reply) => {
                            let status = self.save_settings(settings, running.is_some()).await;
                            let _ = reply.send(status);
                        }
                        Request::DeleteHistory(reply) => {
                            let status = self.delete_history(running.is_some()).await;
                            let _ = reply.send(status);
                        }
                    }
                }
            }
        }
    }

    fn start(
        &mut self,
        request_id: Uuid,
        intent: Intent,
        running: &mut Option<(Active, JoinHandle<Update>)>,
        updates: mpsc::Sender<(Uuid, Update)>,
    ) -> Result<RequestAck, Failure> {
        if self.accepted.contains(&request_id) {
            return Err(Failure::Duplicate);
        }
        let voice = matches!(intent, Intent::StartCapture);
        let settings = self.settings.borrow().clone();
        match &intent {
            Intent::SubmitText { text }
                if text.len() > friday_core::MAX_INPUT_BYTES
                    || text.trim().is_empty()
                    || text.contains('\0') =>
            {
                return Err(Failure::InvalidInput);
            }
            Intent::StartCapture if !settings.cloud_enabled => return Err(Failure::CloudDisabled),
            _ => {}
        }
        let active = self.control.begin(voice)?;
        let id = active.id;
        if self.accepted.len() == RECENT_REQUESTS {
            self.accepted.pop_front();
        }
        self.accepted.push_back(request_id);
        self.publish(|snapshot| {
            *snapshot = Snapshot {
                revision: snapshot.revision,
                session_epoch: active.epoch,
                turn_id: Some(id),
                phase: if voice {
                    Phase::PreparingAudio
                } else {
                    Phase::Routing
                },
                ..Snapshot::default()
            };
        });
        let job = Turn::spawn(
            &active,
            intent,
            self.control.clone(),
            self.ports.clone(),
            settings,
            updates,
        );
        *running = Some((active, job));
        Ok(RequestAck { turn_id: id })
    }

    async fn save_settings(&self, settings: Settings, busy: bool) -> Result<(), Failure> {
        self.control.authorized_epoch()?;
        if busy {
            return Err(Failure::Busy);
        }
        settings.validate()?;
        self.ports.store.save_settings(settings.clone()).await?;
        self.settings.send_replace(settings);
        Ok(())
    }

    async fn delete_history(&self, busy: bool) -> Result<(), Failure> {
        self.control.authorized_epoch()?;
        if busy {
            return Err(Failure::Busy);
        }
        self.ports.store.clear_history().await?;
        self.publish(Snapshot::redact);
        Ok(())
    }
}

async fn wait_job(
    running: &mut Option<(Active, JoinHandle<Update>)>,
) -> Result<Update, tokio::task::JoinError> {
    if let Some((_, job)) = running {
        job.await
    } else {
        std::future::pending().await
    }
}

async fn wait_cancel(token: Option<tokio_util::sync::CancellationToken>) {
    if let Some(token) = token {
        token.cancelled().await;
    } else {
        std::future::pending().await
    }
}

async fn wait_release(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending().await
    }
}
