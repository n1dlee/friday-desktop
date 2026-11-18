use crate::{Action, KnownApp, MAX_INPUT_BYTES, OutputVolume, Plan, VolumeChange};

#[derive(Debug, Eq, PartialEq)]
pub enum Route {
    Local(Plan),
    AskModel,
    Clarify,
    Invalid,
}

/// A partial match never produces an action. The entire phrase must form a valid plan.
pub fn route(input: &str) -> Route {
    if input.len() > MAX_INPUT_BYTES || input.trim().is_empty() || input.contains('\0') {
        return Route::Invalid;
    }

    let lower = input.to_lowercase().replace('ё', "е");
    let text = lower
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return Route::Invalid;
    }

    let text = text
        .strip_prefix("пожалуйста, ")
        .or_else(|| text.strip_prefix("please, "))
        .unwrap_or(&text);
    let text = text
        .strip_suffix(", пожалуйста")
        .or_else(|| text.strip_suffix(", please"))
        .unwrap_or(text);
    let parts = text
        .split([';', ','])
        .flat_map(|part| part.split(" and "))
        .flat_map(|part| part.split(" и "))
        .map(|part| {
            part.trim()
                .trim_start_matches("затем ")
                .trim_start_matches("потом ")
                .trim_start_matches("then ")
        })
        .collect::<Vec<_>>();
    let actions: Vec<_> = parts.iter().filter_map(|part| parse_action(part)).collect();
    if actions.is_empty() {
        if parts.iter().any(|part| action_like(part)) {
            return Route::Clarify;
        }
        return Route::AskModel;
    }
    if actions.len() != parts.len() {
        return Route::Clarify;
    }
    Plan::new(actions).map_or(Route::Invalid, Route::Local)
}

fn action_like(text: &str) -> bool {
    [
        "открой ",
        "запусти ",
        "open ",
        "launch ",
        "громкость ",
        "установи громкость ",
        "поставь громкость ",
        "set the volume ",
        "set volume ",
        "volume ",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

fn parse_action(text: &str) -> Option<Action> {
    let text = text
        .strip_prefix("пожалуйста ")
        .or_else(|| text.strip_prefix("please "))
        .unwrap_or(text);
    if let Some(app) = ["открой ", "запусти ", "open ", "launch "]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
    {
        let app = match app {
            "браузер" | "browser" | "the browser" | "default browser" => KnownApp::Browser,
            "калькулятор" | "calculator" | "the calculator" => KnownApp::Calculator,
            _ => return None,
        };
        return Some(Action::OpenApp { app });
    }

    let change = match text {
        "тише"
        | "сделай тише"
        | "убавь громкость"
        | "уменьши громкость"
        | "volume down"
        | "turn down the volume"
        | "make it quieter"
        | "lower the volume" => VolumeChange::Decrease,
        "громче"
        | "сделай громче"
        | "прибавь громкость"
        | "увеличь громкость"
        | "volume up"
        | "turn up the volume"
        | "make it louder"
        | "raise the volume" => VolumeChange::Increase,
        "выключи звук" | "отключи звук" | "mute" | "mute audio" => {
            VolumeChange::Mute
        }
        "включи звук" | "unmute" | "unmute audio" => VolumeChange::Unmute,
        _ => {
            let level = [
                "громкость на ",
                "громкость ",
                "установи громкость на ",
                "поставь громкость на ",
                "set the volume to ",
                "set volume to ",
                "set volume ",
                "volume ",
            ]
            .iter()
            .find_map(|prefix| text.strip_prefix(prefix))?;
            let level = ["%", " процентов", " процента", " процент", " percent"]
                .iter()
                .find_map(|suffix| level.strip_suffix(suffix))
                .unwrap_or(level)
                .trim()
                .parse::<u8>()
                .ok()?;
            VolumeChange::Set(OutputVolume::try_from(level).ok()?)
        }
    };
    Some(Action::OutputVolume { change })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn supports_a_full_russian_and_english_compound_plan() {
        for phrase in [
            "Открой браузер и сделай тише.",
            "Open the browser, then volume down!",
            "Пожалуйста, открой браузер и сделай тише!",
            "Open the browser and volume down, please.",
        ] {
            let Route::Local(plan) = route(phrase) else {
                panic!("expected a local plan for {phrase}")
            };
            assert_eq!(
                plan.into_actions(),
                [
                    Action::OpenApp {
                        app: KnownApp::Browser
                    },
                    Action::OutputVolume {
                        change: VolumeChange::Decrease
                    }
                ]
            );
        }
    }

    #[test]
    fn plans_are_all_or_nothing() {
        assert_eq!(route("открой браузер и удали файл"), Route::Clarify);
        assert_eq!(route("open an unknown program"), Route::Clarify);
        assert_eq!(route("почему небо синее?"), Route::AskModel);
        assert_eq!(route("volume 101%"), Route::Clarify);
        assert_eq!(route("!!!"), Route::Invalid);
        assert_eq!(route("тише и тише и тише и тише и тише"), Route::Invalid);
    }

    #[test]
    fn tool_deserialization_rejects_unlisted_apps_and_out_of_range_values() {
        assert!(serde_json::from_str::<Action>(r#"{"kind":"open_app","app":"cmd"}"#).is_err());
        assert!(
            serde_json::from_str::<Action>(
                r#"{"kind":"output_volume","change":{"operation":"set","percent":101}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Action>(
                r#"{"kind":"open_app","app":"browser","arguments":["file"]}"#
            )
            .is_err()
        );
    }

    proptest! {
        #[test]
        fn a_volume_step_saturates(current in 0_u8..=100) {
            let volume = OutputVolume::try_from(current).unwrap();
            let up = VolumeChange::Increase.level_after(volume).unwrap().percent();
            let down = VolumeChange::Decrease.level_after(volume).unwrap().percent();
            prop_assert!(down <= current && current <= up && up <= 100);
        }
    }
}
