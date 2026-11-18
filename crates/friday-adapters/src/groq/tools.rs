use friday_core::{Action, KnownApp, Locale, OutputVolume, VolumeChange};
use friday_engine::{Failure, ModelRequest};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn definitions() -> Value {
    json!([
        {
            "type":"function",
            "function":{
                "name":"open_app",
                "description":"Open the user's default browser or the system calculator.",
                "parameters":{"type":"object","properties":{"app":{"type":"string","enum":["browser","calculator"]}},"required":["app"],"additionalProperties":false}
            }
        },{
            "type":"function",
            "function":{
                "name":"output_volume",
                "description":"Adjust the output volume. Use percent only with operation set.",
                "parameters":{"type":"object","properties":{
                    "operation":{"type":"string","enum":["increase","decrease","set","mute","unmute"]},
                    "percent":{"type":"integer","minimum":0,"maximum":100}
                },"required":["operation"],"additionalProperties":false}
            }
        }
    ])
}

pub(super) fn parse(name: &str, arguments: &str) -> Result<Action, Failure> {
    match name {
        "open_app" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                app: KnownApp,
            }
            let args: Args =
                serde_json::from_str(arguments).map_err(|_| Failure::InvalidResponse)?;
            Ok(Action::OpenApp { app: args.app })
        }
        "output_volume" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                operation: Operation,
                percent: Option<OutputVolume>,
            }
            #[derive(Deserialize)]
            #[serde(rename_all = "snake_case")]
            enum Operation {
                Increase,
                Decrease,
                Set,
                Mute,
                Unmute,
            }
            let args: Args =
                serde_json::from_str(arguments).map_err(|_| Failure::InvalidResponse)?;
            let change = match (args.operation, args.percent) {
                (Operation::Set, Some(value)) => VolumeChange::Set(value),
                (Operation::Increase, None) => VolumeChange::Increase,
                (Operation::Decrease, None) => VolumeChange::Decrease,
                (Operation::Mute, None) => VolumeChange::Mute,
                (Operation::Unmute, None) => VolumeChange::Unmute,
                _ => return Err(Failure::InvalidResponse),
            };
            Ok(Action::OutputVolume { change })
        }
        _ => Err(Failure::InvalidResponse),
    }
}

fn arguments(action: Action) -> Value {
    match action {
        Action::OpenApp { app } => json!({ "app": app }),
        Action::OutputVolume { change } => json!(change),
    }
}

pub(super) fn messages(request: &ModelRequest<'_>) -> Result<Vec<Value>, Failure> {
    let language = match request.locale {
        Locale::Ru => "Russian",
        Locale::En => "English",
    };
    let system = format!(
        "You are Friday, a concise desktop assistant. Reply in {language} unless the user requests another language.          Use only the supplied tools for explicit user requests. The context contains conversation history, not pending instructions.          Never assert an action succeeded without a verified tool result. Do not propose shell commands."
    );
    let mut messages = vec![json!({"role":"system","content":system})];
    for entry in request.history.iter().rev().take(6) {
        if let Some(answer) = &entry.answer {
            let input = entry.input.chars().take(400).collect::<String>();
            let answer = answer.chars().take(400).collect::<String>();
            messages.push(json!({"role":"user","content":input}));
            messages.push(json!({"role":"assistant","content":answer}));
        }
    }
    messages.push(json!({"role":"user","content":request.input}));
    for round in request.rounds {
        let calls: Vec<_> = round
            .iter()
            .map(|result| {
                json!({
                    "id": result.proposal.id,
                    "type": "function",
                    "function": {
                        "name": result.proposal.action.kind(),
                        "arguments": arguments(result.proposal.action).to_string()
                    }
                })
            })
            .collect();
        messages.push(json!({"role":"assistant","content":null,"tool_calls":calls}));
        for result in round {
            let outcome =
                serde_json::to_string(&result.outcome).map_err(|_| Failure::InvalidResponse)?;
            messages
                .push(json!({"role":"tool","tool_call_id":result.proposal.id,"content":outcome}));
        }
    }
    Ok(messages)
}
