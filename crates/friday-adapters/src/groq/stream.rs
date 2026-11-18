use std::collections::BTreeMap;

use friday_engine::{Failure, ModelReply, ToolProposal};
use serde::Deserialize;
use tokio::{sync::watch, time::Instant};

use super::{MAX_RESPONSE, tools};

const MAX_EVENT: usize = 64 * 1024;
const MAX_TEXT: usize = 16 * 1024;
const MAX_ARGUMENTS: usize = 4096;

pub(super) struct Decoder {
    consumed: usize,
    line: Vec<u8>,
    event: Vec<u8>,
    text: String,
    tools: BTreeMap<usize, PartialTool>,
    tool_budget: usize,
    preview: Option<watch::Sender<String>>,
    last_preview: Option<Instant>,
    reason: Option<String>,
    done: bool,
}

#[derive(Default)]
struct PartialTool {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    index: usize,
    delta: Delta,
    finish_reason: Option<String>,
}
#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<ToolDelta>>,
}
#[derive(Deserialize)]
struct ToolDelta {
    index: usize,
    id: Option<String>,
    r#type: Option<String>,
    function: Option<FunctionDelta>,
}
#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

impl Decoder {
    pub(super) fn new(tool_budget: usize, preview: Option<watch::Sender<String>>) -> Self {
        Self {
            consumed: 0,
            line: Vec::new(),
            event: Vec::new(),
            text: String::new(),
            tools: BTreeMap::new(),
            tool_budget,
            preview,
            last_preview: None,
            reason: None,
            done: false,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        self.consumed += bytes.len();
        if self.consumed > MAX_RESPONSE {
            return Err(Failure::InvalidResponse);
        }
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.line.len() + line.len() > MAX_EVENT {
                return Err(Failure::InvalidResponse);
            }
            self.line.extend_from_slice(line);
            if line.ends_with(b"\n") {
                self.line.pop();
                if self.line.ends_with(b"\r") {
                    self.line.pop();
                }
                self.consume_line()?;
                self.line.clear();
            }
        }
        Ok(())
    }

    fn consume_line(&mut self) -> Result<(), Failure> {
        if self.line.is_empty() {
            if !self.event.is_empty() {
                let event = std::mem::take(&mut self.event);
                if self.done {
                    return Err(Failure::InvalidResponse);
                }
                if event == b"[DONE]" {
                    self.done = true;
                } else {
                    self.consume_json(&event)?;
                }
            }
        } else if let Some(data) = self.line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if self.event.len() + data.len() + 1 > MAX_EVENT {
                return Err(Failure::InvalidResponse);
            }
            if !self.event.is_empty() {
                self.event.push(b'\n');
            }
            self.event.extend_from_slice(data);
        }
        Ok(())
    }

    fn consume_json(&mut self, event: &[u8]) -> Result<(), Failure> {
        let chunk: Completion =
            serde_json::from_slice(event).map_err(|_| Failure::InvalidResponse)?;
        for choice in chunk.choices {
            if choice.index != 0 || self.reason.is_some() {
                return Err(Failure::InvalidResponse);
            }
            if let Some(content) = choice.delta.content {
                append(&mut self.text, &content, MAX_TEXT)?;
                if let Some(preview) = &self.preview
                    && self
                        .last_preview
                        .is_none_or(|last| last.elapsed().as_millis() >= 65)
                {
                    preview.send_replace(self.text.clone());
                    self.last_preview = Some(Instant::now());
                }
            }
            if let Some(calls) = choice.delta.tool_calls {
                for call in calls {
                    if call.index >= self.tool_budget
                        || call
                            .r#type
                            .as_deref()
                            .is_some_and(|kind| kind != "function")
                    {
                        return Err(Failure::InvalidResponse);
                    }
                    let partial = self.tools.entry(call.index).or_default();
                    if let Some(id) = call.id {
                        append(&mut partial.id, &id, 96)?;
                    }
                    if let Some(function) = call.function {
                        if let Some(name) = function.name {
                            append(&mut partial.name, &name, 64)?;
                        }
                        if let Some(arguments) = function.arguments {
                            append(&mut partial.arguments, &arguments, MAX_ARGUMENTS)?;
                        }
                    }
                }
            }
            self.reason = choice.finish_reason;
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<ModelReply, Failure> {
        if !self.done || !self.event.is_empty() || !self.line.is_empty() {
            return Err(Failure::InvalidResponse);
        }
        match self.reason.as_deref() {
            Some("stop") if self.tools.is_empty() && !self.text.trim().is_empty() => {
                Ok(ModelReply::Answer(self.text))
            }
            Some("tool_calls") if !self.tools.is_empty() => {
                let actions = self
                    .tools
                    .into_iter()
                    .enumerate()
                    .map(|(expected, (index, tool))| {
                        if index != expected
                            || tool.id.is_empty()
                            || !tool
                                .id
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                        {
                            return Err(Failure::InvalidResponse);
                        }
                        let action = tools::parse(&tool.name, &tool.arguments)?;
                        Ok(ToolProposal {
                            id: tool.id,
                            action,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ModelReply::Tools(actions))
            }
            _ => Err(Failure::InvalidResponse),
        }
    }
}

fn append(output: &mut String, fragment: &str, limit: usize) -> Result<(), Failure> {
    if output.len() + fragment.len() > limit {
        return Err(Failure::InvalidResponse);
    }
    output.push_str(fragment);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use friday_core::{Action, KnownApp};

    #[test]
    fn parses_utf8_deltas_and_crlf_at_every_byte_boundary() {
        let mut decoder = Decoder::new(0, None);
        let wire = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Привет\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\n",
            "data: [DONE]\r\n\r\n",
        );
        for byte in wire.as_bytes() {
            decoder.push(std::slice::from_ref(byte)).unwrap();
        }
        assert!(matches!(decoder.finish(), Ok(ModelReply::Answer(text)) if text == "Привет"));
    }

    #[test]
    fn only_complete_validated_tools_reach_the_engine() {
        let record = |delta: serde_json::Value, reason: Option<&str>| {
            format!(
                "data: {}\n\n",
                serde_json::json!({
                    "choices":[{"index":0,"delta":delta,"finish_reason":reason}]
                })
            )
        };
        let a = record(
            serde_json::json!({
                "tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"open_app","arguments":"{\"app\":\"cal"}}]
            }),
            None,
        );
        let b = record(
            serde_json::json!({
                "tool_calls":[{"index":0,"function":{"arguments":"culator\"}"}}]
            }),
            Some("tool_calls"),
        );
        let mut decoder = Decoder::new(1, None);
        decoder.push(a.as_bytes()).unwrap();
        decoder.push(b.as_bytes()).unwrap();
        decoder.push(b"data: [DONE]\n\n").unwrap();
        let Ok(ModelReply::Tools(tools)) = decoder.finish() else {
            panic!("no validated calls")
        };
        assert_eq!(
            tools[0].action,
            Action::OpenApp {
                app: KnownApp::Calculator
            }
        );

        let mut decoder = Decoder::new(0, None);
        assert_eq!(decoder.push(a.as_bytes()), Err(Failure::InvalidResponse));
        assert!(tools::parse("open_app", r#"{"app":"cmd","args":["/c"]}"#).is_err());
        assert!(tools::parse("output_volume", r#"{"operation":"mute","percent":40}"#).is_err());
    }

    #[test]
    fn interrupted_or_oversized_responses_fail() {
        let mut decoder = Decoder::new(0, None);
        assert_eq!(
            decoder.push(&vec![b'X'; MAX_EVENT + 1]),
            Err(Failure::InvalidResponse)
        );
        let decoder = Decoder::new(0, None);
        assert!(matches!(decoder.finish(), Err(Failure::InvalidResponse)));
    }
}
