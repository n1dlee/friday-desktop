use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{Failure, JobContext, SessionStatus};

pub(crate) struct Control {
    state: Mutex<State>,
    pub(crate) updates: watch::Sender<SessionStatus>,
    pub(crate) shutdown: CancellationToken,
}

struct State {
    session: SessionStatus,
    epoch: u64,
    active: Option<Active>,
}

pub(crate) struct Active {
    pub(crate) id: Uuid,
    pub(crate) epoch: u64,
    pub(crate) cancel: CancellationToken,
    pub(crate) finish: Option<CancellationToken>,
}

impl Control {
    pub(crate) fn new(session: SessionStatus) -> Self {
        Self {
            state: Mutex::new(State {
                session,
                epoch: 0,
                active: None,
            }),
            updates: watch::channel(session).0,
            shutdown: CancellationToken::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn session(&self) -> (SessionStatus, u64) {
        let state = self.state();
        (state.session, state.epoch)
    }

    pub(crate) fn authorized_epoch(&self) -> Result<u64, Failure> {
        let state = self.state();
        if self.shutdown.is_cancelled() {
            Err(Failure::Stopped)
        } else if state.session != SessionStatus::Unlocked {
            Err(Failure::Locked)
        } else {
            Ok(state.epoch)
        }
    }

    pub(crate) fn check_epoch(&self, epoch: u64) -> Result<(), Failure> {
        if self.authorized_epoch()? != epoch {
            return Err(Failure::Locked);
        }
        Ok(())
    }

    pub(crate) fn begin(&self, voice: bool) -> Result<Active, Failure> {
        let mut state = self.state();
        if self.shutdown.is_cancelled() {
            return Err(Failure::Stopped);
        }
        if state.session != SessionStatus::Unlocked {
            return Err(Failure::Locked);
        }
        if state.active.is_some() {
            return Err(Failure::Busy);
        }
        let active = Active {
            id: Uuid::new_v4(),
            epoch: state.epoch,
            cancel: self.shutdown.child_token(),
            finish: voice.then(CancellationToken::new),
        };
        state.active = Some(Active {
            id: active.id,
            epoch: active.epoch,
            cancel: active.cancel.clone(),
            finish: active.finish.clone(),
        });
        Ok(active)
    }

    pub(crate) fn capture_finished(&self, id: Uuid) {
        if let Some(active) = self.state().active.as_mut()
            && active.id == id
        {
            active.finish = None;
        }
    }

    pub(crate) fn end(&self, id: Uuid) {
        let mut state = self.state();
        if state.active.as_ref().is_some_and(|turn| turn.id == id) {
            state.active = None;
        }
    }

    pub(crate) fn stop_capture(&self, id: Uuid) -> Result<(), Failure> {
        let state = self.state();
        let active = state
            .active
            .as_ref()
            .filter(|active| active.id == id && !active.cancel.is_cancelled())
            .ok_or(Failure::InvalidTurn)?;
        active.finish.as_ref().ok_or(Failure::InvalidTurn)?.cancel();
        Ok(())
    }

    pub(crate) fn cancel(&self, id: Option<Uuid>) -> Result<(), Failure> {
        let state = self.state();
        let active = state
            .active
            .as_ref()
            .filter(|active| id.is_none_or(|id| active.id == id))
            .ok_or(Failure::InvalidTurn)?;
        active.cancel.cancel();
        Ok(())
    }

    pub(crate) fn set_session(&self, session: SessionStatus) {
        let mut state = self.state();
        if state.session != session {
            state.session = session;
            state.epoch = state.epoch.wrapping_add(1);
            if let Some(active) = &state.active {
                active.cancel.cancel();
            }
            self.updates.send_replace(session);
        }
    }
}

/// The engine is the only constructor. Check again at the native dispatch boundary.
#[derive(Clone)]
pub struct ActionPermit {
    control: Arc<Control>,
    turn_id: Uuid,
    context: JobContext,
}

impl ActionPermit {
    pub(crate) fn new(control: Arc<Control>, turn_id: Uuid, context: JobContext) -> Self {
        Self {
            control,
            turn_id,
            context,
        }
    }

    pub fn check(&self) -> Result<(), Failure> {
        let state = self.control.state();
        if state.session != SessionStatus::Unlocked {
            return Err(Failure::Locked);
        }
        if !state
            .active
            .as_ref()
            .is_some_and(|turn| turn.id == self.turn_id)
        {
            return Err(Failure::InvalidTurn);
        }
        self.context.check()
    }

    pub fn context(&self) -> &JobContext {
        &self.context
    }
}
