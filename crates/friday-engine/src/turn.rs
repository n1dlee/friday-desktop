use std::{collections::HashSet, sync::Arc, time::Duration};

use friday_core::{
    Action, ActionOutcome, Locale, MAX_ACTIONS, MAX_INPUT_BYTES, Plan, Route, route,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, info, info_span};
use uuid::Uuid;

use crate::{
    ActionPermit, ActionReceipt, Failure, HistoryEntry, Intent, JobContext, ModelReply,
    ModelRequest, Phase, Ports, Settings, Snapshot, ToolResult,
    control::{Active, Control},
    unix_seconds,
};

const PROCESSING: Duration = Duration::from_secs(35);
const ACTION_BUDGET: Duration = Duration::from_secs(2);
const TRANSCRIPTION: Duration = Duration::from_secs(10);
const MODEL_ROUND: Duration = Duration::from_secs(12);
const MAX_ROUNDS: usize = 4;
const MAX_ANSWER_BYTES: usize = 16 * 1024;

pub(crate) enum Update {
    Phase(Phase),
    Transcript(String),
    Preview(String),
    Presented(Option<String>, Vec<ActionReceipt>),
    Terminal {
        result: Result<(), Failure>,
        answer: Option<String>,
        receipts: Vec<ActionReceipt>,
        speech_warning: Option<Failure>,
    },
}

impl Update {
    pub(crate) fn terminal(result: Result<(), Failure>) -> Self {
        Self::Terminal {
            result,
            answer: None,
            receipts: Vec::new(),
            speech_warning: None,
        }
    }

    pub(crate) fn apply(self, snapshot: &mut Snapshot, cancelling: bool) {
        match self {
            Self::Phase(phase) if !cancelling => snapshot.phase = phase,
            Self::Transcript(text) if !cancelling => snapshot.transcript = Some(text),
            Self::Preview(text) if !cancelling => snapshot.answer = Some(text),
            Self::Presented(answer, receipts) if !cancelling => {
                snapshot.answer = answer;
                snapshot.receipts = receipts;
            }
            Self::Terminal {
                result,
                answer,
                receipts,
                speech_warning,
            } => {
                snapshot.answer = answer;
                snapshot.receipts = receipts;
                snapshot.speech_warning = speech_warning;
                snapshot.phase = match result {
                    Ok(()) => Phase::Completed,
                    Err(Failure::Cancelled) => Phase::Cancelled,
                    Err(_) => Phase::Failed,
                };
                snapshot.failure = result.err().filter(|error| *error != Failure::Cancelled);
            }
            _ => {}
        }
    }
}

pub(crate) struct Turn {
    id: Uuid,
    control: Arc<Control>,
    ports: Arc<Ports>,
    settings: Settings,
    updates: mpsc::Sender<(Uuid, Update)>,
    cancel: CancellationToken,
    answer: Option<String>,
    receipts: Vec<ActionReceipt>,
    speech_warning: Option<Failure>,
}

impl Turn {
    pub(crate) fn spawn(
        active: &Active,
        intent: Intent,
        control: Arc<Control>,
        ports: Arc<Ports>,
        settings: Settings,
        updates: mpsc::Sender<(Uuid, Update)>,
    ) -> JoinHandle<Update> {
        let mut turn = Self {
            id: active.id,
            cancel: active.cancel.clone(),
            control,
            ports,
            settings,
            updates,
            answer: None,
            receipts: Vec::new(),
            speech_warning: None,
        };
        let finish = active.finish.clone();
        let span = info_span!("turn", id = %active.id);
        tokio::spawn(
            async move {
                let start = tokio::time::Instant::now();
                let result = turn.run(intent, finish).await;
                info!(
                    result = ?result,
                    effects = turn.receipts.len(),
                    duration_ms = start.elapsed().as_millis() as u64,
                    "turn finished"
                );
                Update::Terminal {
                    result,
                    answer: turn.answer,
                    receipts: turn.receipts,
                    speech_warning: turn.speech_warning,
                }
            }
            .instrument(span),
        )
    }

    async fn update(&self, update: Update) {
        if let Update::Phase(phase) = &update {
            info!(stage = ?phase, "turn stage entered");
        }
        let _ = self.updates.send((self.id, update)).await;
    }

    async fn run(
        &mut self,
        intent: Intent,
        finish: Option<CancellationToken>,
    ) -> Result<(), Failure> {
        let clip = match intent {
            Intent::StartCapture => {
                let cloud = self.ports.cloud(self.settings.cloud_enabled)?;
                let audio = self.ports.audio.as_ref().ok_or(Failure::AudioUnavailable)?;
                let readiness = JobContext::new(self.cancel.clone(), Duration::from_secs(2));
                readiness.run(cloud.ready()).await?;
                let capture = JobContext::new(self.cancel.clone(), Duration::from_secs(30));
                let (started, begin) = tokio::sync::oneshot::channel();
                let recording =
                    audio.capture(finish.ok_or(Failure::InvalidTurn)?, capture, started);
                tokio::pin!(recording);
                let result = tokio::select! {
                    result = &mut recording => result,
                    Ok(()) = begin => {
                        self.update(Update::Phase(Phase::Capturing)).await;
                        recording.await
                    }
                };
                self.control.capture_finished(self.id);
                Some(result?)
            }
            Intent::SubmitText { .. } => None,
        };

        let context = JobContext::new(self.cancel.clone(), PROCESSING);
        let text = match intent {
            Intent::SubmitText { text } => text,
            Intent::StartCapture => {
                self.update(Update::Phase(Phase::Transcribing)).await;
                let cloud = self.ports.cloud(self.settings.cloud_enabled)?;
                let job = context.capped(TRANSCRIPTION);
                let transcript = job
                    .run(cloud.transcribe(
                        clip.ok_or(Failure::EmptyCapture)?,
                        self.settings.locale,
                        job.clone(),
                    ))
                    .await?;
                if transcript.trim().is_empty() {
                    return Err(Failure::EmptyCapture);
                }
                if transcript.len() > MAX_INPUT_BYTES || transcript.contains('\0') {
                    return Err(Failure::InvalidResponse);
                }
                self.update(Update::Transcript(transcript.clone())).await;
                transcript
            }
        };

        context.check()?;
        self.update(Update::Phase(Phase::Routing)).await;
        let text = text.trim();
        match route(text) {
            Route::Local(plan) => self.execute_plan(plan, &context).await?,
            Route::AskModel => self.ask_model(text, &context).await?,
            Route::Clarify => return Err(Failure::Clarify),
            Route::Invalid => return Err(Failure::InvalidInput),
        }

        context.check()?;
        if self.settings.history_enabled {
            let entry = HistoryEntry {
                turn_id: self.id,
                created_at: unix_seconds(),
                input: text.to_owned(),
                answer: self.answer.clone(),
                receipts: self.receipts.clone(),
            };
            self.ports.store.save_history(entry).await?;
        }

        self.update(Update::Presented(
            self.answer.clone(),
            self.receipts.clone(),
        ))
        .await;
        self.speak().await;
        if self.cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        Ok(())
    }

    async fn execute_plan(&mut self, plan: Plan, context: &JobContext) -> Result<(), Failure> {
        self.update(Update::Phase(Phase::Executing)).await;
        for action in plan.into_actions() {
            require_completion(self.execute(action, context).await?)?;
        }
        Ok(())
    }

    async fn ask_model(&mut self, input: &str, context: &JobContext) -> Result<(), Failure> {
        let cloud_enabled = self.settings.cloud_enabled;
        self.ports.cloud(cloud_enabled)?;
        let history = if self.settings.history_enabled {
            context.run(self.ports.store.history(6)).await?
        } else {
            Vec::new()
        };
        let allow_tools = requests_model_tools(input);
        let mut rounds = Vec::new();
        let mut seen = HashSet::new();
        for index in 0..MAX_ROUNDS {
            let remaining = MAX_ACTIONS - self.receipts.len();
            let tool_budget = if !allow_tools || index + 1 == MAX_ROUNDS {
                0
            } else {
                remaining
            };
            self.update(Update::Phase(Phase::Planning)).await;
            let reply = self
                .respond(input, &history, &rounds, tool_budget, context)
                .await?;
            match reply {
                ModelReply::Answer(text) => {
                    if self.receipts.is_empty() {
                        if text.trim().is_empty() || text.len() > MAX_ANSWER_BYTES {
                            return Err(Failure::InvalidResponse);
                        }
                        self.answer = Some(text);
                    }
                    return Ok(());
                }
                ModelReply::Tools(proposals) => {
                    if proposals.is_empty() || proposals.len() > tool_budget {
                        return Err(Failure::ActionLimit);
                    }
                    for proposal in &proposals {
                        if proposal.id.is_empty()
                            || proposal.id.len() > 96
                            || !seen.insert(proposal.id.clone())
                        {
                            return Err(Failure::InvalidResponse);
                        }
                    }
                    self.update(Update::Phase(Phase::Executing)).await;
                    let mut results = Vec::with_capacity(proposals.len());
                    for proposal in proposals {
                        let outcome = self.execute(proposal.action, context).await?;
                        require_completion(outcome)?;
                        results.push(ToolResult { proposal, outcome });
                    }
                    rounds.push(results);
                }
            }
        }
        Ok(())
    }

    async fn respond(
        &self,
        input: &str,
        history: &[HistoryEntry],
        rounds: &[Vec<ToolResult>],
        tool_budget: usize,
        context: &JobContext,
    ) -> Result<ModelReply, Failure> {
        let cloud = self.ports.cloud(self.settings.cloud_enabled)?;
        let job = context.capped(MODEL_ROUND);
        let show_preview = tool_budget == 0 && rounds.is_empty();
        let (preview, mut text) = watch::channel(String::new());
        let response = job.run(cloud.respond(
            ModelRequest {
                locale: self.settings.locale,
                input,
                history,
                rounds,
                tool_budget,
                preview: show_preview.then_some(preview),
            },
            job.clone(),
        ));
        tokio::pin!(response);
        if !show_preview {
            return response.await;
        }
        loop {
            tokio::select! {
                biased;
                result = &mut response => return result,
                Ok(()) = text.changed() => {
                    let next = text.borrow_and_update().clone();
                    if next.len() <= MAX_ANSWER_BYTES {
                        self.update(Update::Preview(next)).await;
                    }
                }
            }
        }
    }

    async fn execute(
        &mut self,
        action: Action,
        context: &JobContext,
    ) -> Result<ActionOutcome, Failure> {
        context.check()?;
        let id = Uuid::new_v4();
        let job = context.capped(ACTION_BUDGET);
        let permit = ActionPermit::new(self.control.clone(), self.id, job.clone());
        permit.check()?;

        if !self.ports.store.record_dispatch(id, action.kind()).await? {
            return Err(Failure::Storage);
        }

        let outcome = if permit.check().is_err() {
            ActionOutcome::Blocked
        } else {
            match job.run(self.ports.platform.execute(action, permit)).await {
                Ok(outcome) => outcome,
                Err(Failure::Cancelled | Failure::Timeout | Failure::ActionUncertain) => {
                    ActionOutcome::Indeterminate
                }
                Err(Failure::Locked | Failure::InvalidTurn) => ActionOutcome::Blocked,
                Err(_) => ActionOutcome::Failed,
            }
        };
        let write = self.ports.store.record_outcome(id, outcome).await;
        self.receipts.push(ActionReceipt {
            id,
            action,
            outcome: if write.is_ok() {
                outcome
            } else {
                ActionOutcome::Indeterminate
            },
        });
        info!(action = action.kind(), result = ?outcome, "local action reached a result");
        self.update(Update::Presented(
            self.answer.clone(),
            self.receipts.clone(),
        ))
        .await;
        write?;
        Ok(outcome)
    }

    async fn speak(&mut self) {
        if !self.settings.speak_answers || self.cancel.is_cancelled() {
            return;
        }
        let mut text = self
            .answer
            .clone()
            .unwrap_or_else(|| self.summarize_receipts());
        if text.is_empty() {
            return;
        }
        if let Some((end, _)) = text.char_indices().nth(1600) {
            text.truncate(end);
        }
        let Some(speech) = self.ports.speech.clone() else {
            self.speech_warning = Some(Failure::SpeechUnavailable);
            return;
        };
        let context = JobContext::new(self.cancel.clone(), Duration::from_secs(60));
        self.update(Update::Phase(Phase::Speaking)).await;
        self.speech_warning = speech
            .speak(text, self.settings.locale, context)
            .await
            .err()
            .filter(|failure| *failure != Failure::Cancelled);
    }

    fn summarize_receipts(&self) -> String {
        self.receipts
            .iter()
            .map(|receipt| match (self.settings.locale, receipt.outcome) {
                (Locale::Ru, ActionOutcome::AcceptedByOs) => {
                    "Windows приняла запрос на запуск.".into()
                }
                (Locale::En, ActionOutcome::AcceptedByOs) => {
                    "Windows accepted the launch request.".into()
                }
                (Locale::Ru, ActionOutcome::VolumeApplied { volume, muted }) => {
                    format!(
                        "Громкость {} процентов.{}",
                        volume.percent(),
                        if muted {
                            " Звук отключён."
                        } else {
                            ""
                        }
                    )
                }
                (Locale::En, ActionOutcome::VolumeApplied { volume, muted }) => {
                    format!(
                        "Volume {} percent.{}",
                        volume.percent(),
                        if muted { " Audio is muted." } else { "" }
                    )
                }
                (Locale::Ru, ActionOutcome::Indeterminate) => {
                    "Не удалось подтвердить результат.".into()
                }
                (Locale::En, ActionOutcome::Indeterminate) => {
                    "The result could not be confirmed.".into()
                }
                (Locale::Ru, _) => "Действие не выполнено.".into(),
                (Locale::En, _) => "The action did not complete.".into(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn require_completion(outcome: ActionOutcome) -> Result<(), Failure> {
    match outcome {
        ActionOutcome::AcceptedByOs | ActionOutcome::VolumeApplied { .. } => Ok(()),
        ActionOutcome::Indeterminate => Err(Failure::ActionUncertain),
        ActionOutcome::Blocked | ActionOutcome::Failed => Err(Failure::ActionFailed),
    }
}

fn requests_model_tools(input: &str) -> bool {
    let text = input.to_lowercase();
    [
        "please open ",
        "please launch ",
        "please turn ",
        "could you open ",
        "can you open ",
        "could you turn ",
        "could you launch ",
        "can you turn ",
        "could you set ",
        "можешь открыть ",
        "можешь запустить ",
        "можешь сделать ",
        "можешь изменить громкость ",
        "пожалуйста, открой ",
        "пожалуйста, запусти ",
        "пожалуйста, сделай ",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}
