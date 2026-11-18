#![forbid(unsafe_code)]

mod action;
mod routing;

pub use action::{
    Action, ActionOutcome, KnownApp, MAX_ACTIONS, OutputVolume, Plan, PlanError, VolumeChange,
};
pub use routing::{Route, route};
use serde::{Deserialize, Serialize};

pub const MAX_INPUT_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Locale {
    #[default]
    Ru,
    En,
}

impl Locale {
    pub fn language(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }
}
