use std::{
    future::Future,
    pin::Pin,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use friday_core::{Action, ActionOutcome, Locale};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Failure>> + Send + 'a>>;
pub const SAMPLE_RATE: u32 = 16_000;
pub const MAX_SAMPLES: usize = 30 * SAMPLE_RATE as usize;
pub const HISTORY_PAGE: usize = 100;
pub const DEFAULT_SHORTCUT: &str = "Ctrl+Shift+Space";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    #[error("another turn is active")]
    Busy,
    #[error("this request was already accepted")]
    Duplicate,
    #[error("the user session is not unlocked")]
    Locked,
    #[error("the request does not match the active turn")]
    InvalidTurn,
    #[error("the engine is stopping")]
    Stopped,
    #[error("the operation was cancelled")]
    Cancelled,
    #[error("the operation took too long")]
    Timeout,
    #[error("the input is invalid or too large")]
    InvalidInput,
    #[error("the request needs clarification")]
    Clarify,
    #[error("cloud processing is disabled")]
    CloudDisabled,
    #[error("no cloud API key is configured")]
    MissingKey,
    #[error("access to the credential store failed")]
    CredentialStore,
    #[error("the provider rejected the credentials")]
    Unauthorized,
    #[error("the provider rate limit was reached")]
    RateLimited,
    #[error("the provider is unavailable")]
    ProviderUnavailable,
    #[error("the provider returned an invalid response")]
    InvalidResponse,
    #[error("the action limit was reached")]
    ActionLimit,
    #[error("a local action failed")]
    ActionFailed,
    #[error("a local action may have happened but could not be confirmed")]
    ActionUncertain,
    #[error("the microphone is not available")]
    AudioUnavailable,
    #[error("the audio device was lost")]
    AudioLost,
    #[error("the audio buffer overflowed")]
    AudioOverflow,
    #[error("the audio device has not released its resources")]
    AudioStalled,
    #[error("the capture did not contain audio")]
    EmptyCapture,
    #[error("no suitable local voice is available")]
    SpeechUnavailable,
    #[error("the keyboard shortcut is unavailable")]
    ShortcutUnavailable,
    #[error("signed updates are not configured or no offer is selected")]
    UpdateUnavailable,
    #[error("checking or installing an update failed")]
    UpdateFailed,
    #[error("the settings or history store is not available")]
    Storage,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub locale: Locale,
    pub cloud_enabled: bool,
    pub history_enabled: bool,
    pub speak_answers: bool,
    pub shortcut: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            locale: Locale::Ru,
            cloud_enabled: false,
            history_enabled: false,
            speak_answers: false,
            shortcut: DEFAULT_SHORTCUT.into(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), Failure> {
        if self.shortcut.is_empty()
            || self.shortcut.len() > 64
            || !self.shortcut.chars().all(|c| c.is_ascii_graphic())
        {
            return Err(Failure::InvalidInput);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Unknown,
    Unlocked,
    Locked,
    Suspended,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    PreparingAudio,
    Capturing,
    Transcribing,
    Routing,
    Planning,
    Executing,
    Speaking,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::PreparingAudio
                | Self::Capturing
                | Self::Transcribing
                | Self::Routing
                | Self::Planning
                | Self::Executing
                | Self::Speaking
                | Self::Cancelling
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActionReceipt {
    pub id: Uuid,
    pub action: Action,
    pub outcome: ActionOutcome,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Snapshot {
    #[serde(skip)]
    pub(crate) session_epoch: u64,
    pub revision: u64,
    pub session: SessionStatus,
    pub turn_id: Option<Uuid>,
    pub phase: Phase,
    pub transcript: Option<String>,
    pub answer: Option<String>,
    pub receipts: Vec<ActionReceipt>,
    pub failure: Option<Failure>,
    pub speech_warning: Option<Failure>,
}

impl Snapshot {
    pub(crate) fn redact(&mut self) {
        self.transcript = None;
        self.answer = None;
        self.receipts.clear();
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct HistoryEntry {
    pub turn_id: Uuid,
    pub created_at: i64,
    pub input: String,
    pub answer: Option<String>,
    pub receipts: Vec<ActionReceipt>,
}

pub fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[derive(Clone)]
pub struct JobContext {
    token: CancellationToken,
    deadline: Instant,
}

impl JobContext {
    pub(crate) fn new(token: CancellationToken, duration: Duration) -> Self {
        Self {
            token,
            deadline: Instant::now() + duration,
        }
    }

    pub fn capped(&self, limit: Duration) -> Self {
        Self {
            token: self.token.clone(),
            deadline: self.deadline.min(Instant::now() + limit),
        }
    }

    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn check(&self) -> Result<(), Failure> {
        if self.token.is_cancelled() {
            Err(Failure::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(Failure::Timeout)
        } else {
            Ok(())
        }
    }

    /// For cancellation-safe futures. Hardware leases are awaited separately until released.
    pub async fn run<T>(
        &self,
        future: impl Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        self.check()?;
        tokio::select! {
            biased;
            _ = self.token.cancelled() => Err(Failure::Cancelled),
            _ = tokio::time::sleep_until(self.deadline) => Err(Failure::Timeout),
            result = future => result,
        }
    }
}

pub struct PcmClip(Vec<i16>);

impl PcmClip {
    pub fn new(samples: Vec<i16>) -> Result<Self, Failure> {
        if samples.is_empty() {
            return Err(Failure::EmptyCapture);
        }
        if samples.len() > MAX_SAMPLES {
            return Err(Failure::InvalidInput);
        }
        Ok(Self(samples))
    }

    pub fn into_samples(self) -> Vec<i16> {
        self.0
    }
}

pub struct ModelRequest<'a> {
    pub locale: Locale,
    pub input: &'a str,
    pub history: &'a [HistoryEntry],
    pub rounds: &'a [Vec<ToolResult>],
    pub tool_budget: usize,
    /// Enables a coalescing text preview only during the answer-only round.
    pub preview: Option<tokio::sync::watch::Sender<String>>,
}

pub struct ToolProposal {
    pub id: String,
    pub action: Action,
}

pub struct ToolResult {
    pub proposal: ToolProposal,
    pub outcome: ActionOutcome,
}

pub enum ModelReply {
    Answer(String),
    Tools(Vec<ToolProposal>),
}
