use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_ACTIONS: usize = 4;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnownApp {
    Browser,
    Calculator,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct OutputVolume(u8);

impl OutputVolume {
    pub const fn percent(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Error)]
#[error("volume must be between 0 and 100 percent")]
pub struct InvalidVolume;

impl TryFrom<u8> for OutputVolume {
    type Error = InvalidVolume;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(InvalidVolume)
        }
    }
}

impl From<OutputVolume> for u8 {
    fn from(value: OutputVolume) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "operation",
    content = "percent",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum VolumeChange {
    Increase,
    Decrease,
    Set(OutputVolume),
    Mute,
    Unmute,
}

impl VolumeChange {
    pub fn level_after(self, current: OutputVolume) -> Option<OutputVolume> {
        match self {
            Self::Increase => Some(OutputVolume(current.0.saturating_add(10).min(100))),
            Self::Decrease => Some(OutputVolume(current.0.saturating_sub(10))),
            Self::Set(volume) => Some(volume),
            Self::Mute | Self::Unmute => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    OpenApp { app: KnownApp },
    OutputVolume { change: VolumeChange },
}

impl Action {
    pub const fn kind(self) -> &'static str {
        match self {
            Self::OpenApp { .. } => "open_app",
            Self::OutputVolume { .. } => "output_volume",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActionOutcome {
    AcceptedByOs,
    VolumeApplied { volume: OutputVolume, muted: bool },
    Blocked,
    Failed,
    Indeterminate,
}

impl ActionOutcome {
    pub const fn completed(self) -> bool {
        matches!(self, Self::AcceptedByOs | Self::VolumeApplied { .. })
    }
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum PlanError {
    #[error("a plan must contain at least one action")]
    Empty,
    #[error("the action budget was exceeded")]
    Limit,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Plan(Vec<Action>);

impl Plan {
    pub fn new(actions: Vec<Action>) -> Result<Self, PlanError> {
        match actions.len() {
            0 => Err(PlanError::Empty),
            1..=MAX_ACTIONS => Ok(Self(actions)),
            _ => Err(PlanError::Limit),
        }
    }

    pub fn into_actions(self) -> Vec<Action> {
        self.0
    }
}
