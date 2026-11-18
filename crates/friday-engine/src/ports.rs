use std::sync::Arc;

use friday_core::{Action, ActionOutcome, Locale};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    ActionPermit, Failure, HistoryEntry, JobContext, ModelReply, ModelRequest, PcmClip, PortFuture,
    Settings,
};

pub trait PlatformActions: Send + Sync {
    fn execute(&self, action: Action, permit: ActionPermit) -> PortFuture<'_, ActionOutcome>;
}

pub trait AudioInput: Send + Sync {
    /// Resolves only once this capture has released its stream, including on cancellation.
    fn capture(
        &self,
        finish: CancellationToken,
        context: JobContext,
        started: tokio::sync::oneshot::Sender<()>,
    ) -> PortFuture<'_, PcmClip>;
}

pub trait SpeechOutput: Send + Sync {
    /// Resolves only once playback has stopped or the device reports completion.
    fn speak(&self, text: String, locale: Locale, context: JobContext) -> PortFuture<'_, ()>;
}

pub trait Cloud: Send + Sync {
    fn ready(&self) -> PortFuture<'_, ()>;
    fn transcribe(
        &self,
        clip: PcmClip,
        locale: Locale,
        context: JobContext,
    ) -> PortFuture<'_, String>;
    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        context: JobContext,
    ) -> PortFuture<'a, ModelReply>;
}

pub trait Store: Send + Sync {
    fn save_settings(&self, settings: Settings) -> PortFuture<'_, ()>;
    fn record_dispatch(&self, id: Uuid, kind: &'static str) -> PortFuture<'_, bool>;
    fn record_outcome(&self, id: Uuid, outcome: ActionOutcome) -> PortFuture<'_, ()>;
    fn save_history(&self, entry: HistoryEntry) -> PortFuture<'_, ()>;
    fn history(&self, limit: usize) -> PortFuture<'_, Vec<HistoryEntry>>;
    fn clear_history(&self) -> PortFuture<'_, ()>;
}

pub struct Ports {
    pub platform: Arc<dyn PlatformActions>,
    pub store: Arc<dyn Store>,
    pub audio: Option<Arc<dyn AudioInput>>,
    pub cloud: Option<Arc<dyn Cloud>>,
    pub speech: Option<Arc<dyn SpeechOutput>>,
}

impl Ports {
    pub(crate) fn cloud(&self, enabled: bool) -> Result<&dyn Cloud, Failure> {
        if !enabled {
            return Err(Failure::CloudDisabled);
        }
        self.cloud.as_deref().ok_or(Failure::ProviderUnavailable)
    }
}
