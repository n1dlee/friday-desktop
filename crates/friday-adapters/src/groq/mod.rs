mod stream;
mod tools;

use std::{io::Cursor, sync::Arc, time::Duration};

use friday_core::Locale;
use friday_engine::{
    Cloud, Failure, JobContext, ModelReply, ModelRequest, PcmClip, PortFuture, SAMPLE_RATE,
};
use reqwest::{
    Client, Response, StatusCode, Url,
    multipart::{Form, Part},
    redirect::Policy,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::Mutex, time::Instant};

use crate::{CredentialSource, validate_key};

const BASE: &str = "https://api.groq.com/openai/v1/";
const MAX_RESPONSE: usize = 256 * 1024;
const STT_MODELS: [&str; 2] = ["whisper-large-v3-turbo", "whisper-large-v3"];
const CHAT_MODELS: [&str; 2] = ["openai/gpt-oss-120b", "openai/gpt-oss-20b"];

pub struct Groq {
    client: Client,
    base: Url,
    credentials: Arc<dyn CredentialSource>,
    catalog: Mutex<Option<Catalog>>,
}

struct Catalog {
    revision: u64,
    expires: Instant,
    stt: Option<&'static str>,
    chat: Option<&'static str>,
}

fn build_client() -> Result<Client, Failure> {
    Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| Failure::ProviderUnavailable)
}

impl Groq {
    pub fn new(credentials: Arc<dyn CredentialSource>) -> Result<Self, Failure> {
        Ok(Self {
            client: build_client()?,
            base: Url::parse(BASE).map_err(|_| Failure::ProviderUnavailable)?,
            credentials,
            catalog: Mutex::new(None),
        })
    }

    fn endpoint(&self, path: &'static str) -> Result<Url, Failure> {
        self.base
            .join(path)
            .map_err(|_| Failure::ProviderUnavailable)
    }

    async fn key(&self) -> Result<SecretString, Failure> {
        let secret = self
            .credentials
            .read_key()
            .await?
            .ok_or(Failure::MissingKey)?;
        validate_key(secret.expose_secret()).map_err(|_| Failure::Unauthorized)?;
        Ok(secret)
    }

    async fn model(&self, key: &SecretString, transcribe: bool) -> Result<&'static str, Failure> {
        let revision = self.credentials.revision();
        let mut catalog = self.catalog.lock().await;
        if catalog
            .as_ref()
            .is_none_or(|entry| entry.revision != revision || entry.expires <= Instant::now())
        {
            #[derive(Deserialize)]
            struct Models {
                data: Vec<Model>,
            }
            #[derive(Deserialize)]
            struct Model {
                id: String,
            }
            let url = self.endpoint("models")?;
            let response = self
                .client
                .get(url)
                .bearer_auth(key.expose_secret())
                .send()
                .await
                .map_err(map_http_error)?;
            check_status(&response)?;
            let models: Models = serde_json::from_slice(&bounded_body(response).await?)
                .map_err(|_| Failure::InvalidResponse)?;
            let find = |known: [&'static str; 2]| {
                known
                    .into_iter()
                    .find(|id| models.data.iter().any(|item| item.id == *id))
            };
            *catalog = Some(Catalog {
                revision,
                expires: Instant::now() + Duration::from_secs(1800),
                stt: find(STT_MODELS),
                chat: find(CHAT_MODELS),
            });
        }
        catalog
            .as_ref()
            .and_then(|entry| if transcribe { entry.stt } else { entry.chat })
            .ok_or(Failure::ProviderUnavailable)
    }
}

impl Cloud for Groq {
    fn ready(&self) -> PortFuture<'_, ()> {
        Box::pin(async { self.key().await.map(drop) })
    }

    fn transcribe(
        &self,
        audio: PcmClip,
        locale: Locale,
        context: JobContext,
    ) -> PortFuture<'_, String> {
        Box::pin(async move {
            #[derive(Deserialize)]
            struct Transcript {
                text: String,
            }
            context
                .run(async {
                    let key = self.key().await?;
                    let model = self.model(&key, true).await?;
                    let bytes = encode_wav(audio)?;
                    context.check()?;
                    let audio = Part::bytes(bytes)
                        .file_name("speech.wav")
                        .mime_str("audio/wav")
                        .map_err(|_| Failure::InvalidInput)?;
                    let form = Form::new()
                        .text("model", model)
                        .text("language", locale.language())
                        .text("response_format", "json")
                        .part("file", audio);
                    let response = self
                        .client
                        .post(self.endpoint("audio/transcriptions")?)
                        .bearer_auth(key.expose_secret())
                        .multipart(form)
                        .send()
                        .await
                        .map_err(map_http_error)?;
                    check_status(&response)?;
                    let result: Transcript = serde_json::from_slice(&bounded_body(response).await?)
                        .map_err(|_| Failure::InvalidResponse)?;
                    Ok(result.text)
                })
                .await
        })
    }

    fn respond<'a>(
        &'a self,
        request: ModelRequest<'a>,
        context: JobContext,
    ) -> PortFuture<'a, ModelReply> {
        Box::pin(async move {
            context
                .run(async {
                    let key = self.key().await?;
                    let model = self.model(&key, false).await?;
                    let mut body = json!({
                        "model": model,
                        "messages": tools::messages(&request)?,
                        "stream": true,
                        "max_completion_tokens": 1024,
                    });
                    if request.tool_budget > 0 {
                        body["tools"] = tools::definitions();
                        body["tool_choice"] = Value::from("auto");
                    }
                    let make_request = || -> Result<reqwest::RequestBuilder, Failure> {
                        Ok(self
                            .client
                            .post(self.endpoint("chat/completions")?)
                            .bearer_auth(key.expose_secret())
                            .json(&body))
                    };
                    context.check()?;
                    let mut response = make_request()?.send().await.map_err(map_http_error)?;
                    if response.status() == StatusCode::TOO_MANY_REQUESTS
                        && let Some(wait) = retry_delay(&response)
                        && Instant::now() + wait < context.deadline()
                    {
                        drop(response);
                        tokio::time::sleep(wait).await;
                        context.check()?;
                        response = make_request()?.send().await.map_err(map_http_error)?;
                    }
                    check_status(&response)?;
                    let mut decoder = stream::Decoder::new(request.tool_budget, request.preview);
                    while let Some(chunk) = response.chunk().await.map_err(map_http_error)? {
                        decoder.push(&chunk)?;
                    }
                    decoder.finish()
                })
                .await
        })
    }
}

fn retry_delay(response: &Response) -> Option<Duration> {
    let seconds: u64 = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse()
        .ok()?;
    (seconds <= 2).then(|| Duration::from_secs(seconds))
}

fn check_status(response: &Response) -> Result<(), Failure> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    Err(match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Failure::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => Failure::RateLimited,
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => Failure::Timeout,
        _ => Failure::ProviderUnavailable,
    })
}

fn map_http_error(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        Failure::Timeout
    } else {
        Failure::ProviderUnavailable
    }
}

async fn bounded_body(mut response: Response) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(map_http_error)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err(Failure::InvalidResponse);
        }
        bytes.extend(chunk);
    }
    Ok(bytes)
}

fn encode_wav(audio: PcmClip) -> Result<Vec<u8>, Failure> {
    let mut bytes = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::new(&mut bytes, spec).map_err(|_| Failure::InvalidInput)?;
    for sample in audio.into_samples() {
        wav.write_sample(sample)
            .map_err(|_| Failure::InvalidInput)?;
    }
    wav.finalize().map_err(|_| Failure::InvalidInput)?;
    Ok(bytes.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;
    use friday_core::{Action, ActionOutcome};
    use friday_engine::{
        ActionPermit, Engine, Intent, Phase, PlatformActions, Ports, SessionStatus, Settings,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    use uuid::Uuid;

    struct SyntheticKey;
    impl CredentialSource for SyntheticKey {
        fn read_key(&self) -> PortFuture<'_, Option<SecretString>> {
            Box::pin(async { Ok(Some(SecretString::from("gsk_fixture_only"))) })
        }
        fn revision(&self) -> u64 {
            0
        }
    }

    struct DisallowedPlatform;
    impl PlatformActions for DisallowedPlatform {
        fn execute(&self, _: Action, _: ActionPermit) -> PortFuture<'_, ActionOutcome> {
            panic!("text responses may not dispatch tool actions")
        }
    }

    async fn request(stream: &mut TcpStream) -> (String, Vec<u8>) {
        let mut incoming = Vec::new();
        let mut chunk = [0_u8; 2048];
        let headers_end = loop {
            let received = stream.read(&mut chunk).await.unwrap();
            assert!(received > 0);
            incoming.extend_from_slice(&chunk[..received]);
            if let Some(end) = incoming.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let head = String::from_utf8(incoming[..headers_end].to_vec()).unwrap();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while incoming.len() - headers_end < content_length {
            let received = stream.read(&mut chunk).await.unwrap();
            assert!(received > 0);
            incoming.extend_from_slice(&chunk[..received]);
        }
        (
            head,
            incoming[headers_end..headers_end + content_length].to_vec(),
        )
    }

    async fn reply(stream: &mut TcpStream, status: &str, headers: &str, body: &str) {
        let response = format!(
            "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn real_http_adapter_respects_short_retry_and_streams_only_an_answer() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (head, body) = request(&mut stream).await;
                assert!(head.contains("authorization: Bearer gsk_fixture_only"));
                if index == 0 {
                    assert!(head.starts_with("GET /v1/models "));
                    reply(
                        &mut stream,
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        r#"{"data":[{"id":"openai/gpt-oss-20b"}]}"#,
                    )
                    .await;
                } else {
                    assert!(head.starts_with("POST /v1/chat/completions "));
                    let input: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(input["model"], "openai/gpt-oss-20b");
                    assert_eq!(input["stream"], true);
                    assert!(input.get("tools").is_none());
                    assert_eq!(input["messages"][1]["content"], "Why is the sky blue?");
                    if index == 1 {
                        reply(
                            &mut stream,
                            "429 Too Many Requests",
                            "Retry-After: 0\r\n",
                            "",
                        )
                        .await;
                    } else {
                        reply(
                            &mut stream, "200 OK", "Content-Type: text/event-stream\r\n",
                            concat!(
                                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Light scatters.\"}}]}\n\n",
                                "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                                "data: [DONE]\n\n",
                            ),
                        ).await;
                    }
                }
            }
        });
        let groq = Groq {
            client: Client::builder().no_proxy().build().unwrap(),
            base: Url::parse(&format!("http://{address}/v1/")).unwrap(),
            credentials: Arc::new(SyntheticKey),
            catalog: Mutex::new(None),
        };
        let directory = tempfile::tempdir().unwrap();
        let (store, _) = SqliteStore::open_profile(directory.path()).unwrap();
        let engine = Engine::start(
            Ports {
                platform: Arc::new(DisallowedPlatform),
                store: Arc::new(store),
                audio: None,
                cloud: Some(Arc::new(groq)),
                speech: None,
            },
            Settings {
                cloud_enabled: true,
                ..Settings::default()
            },
            SessionStatus::Unlocked,
        );
        let handle = engine.handle();
        let mut updates = handle.subscribe();
        handle
            .submit(
                Uuid::new_v4(),
                Intent::SubmitText {
                    text: "Why is the sky blue?".into(),
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = handle.snapshot();
                if !state.phase.active() {
                    assert_eq!(state.phase, Phase::Completed, "{:?}", state.failure);
                    assert_eq!(state.answer.as_deref(), Some("Light scatters."));
                    break;
                }
                updates.changed().await.unwrap();
            }
            server.await.unwrap();
        })
        .await
        .unwrap();
        engine.shutdown().await;
    }

    #[test]
    fn encodes_a_pcm_clip_without_writing_to_disk() {
        let samples = vec![512, -512, 128];
        let bytes = encode_wav(PcmClip::new(samples.clone()).unwrap()).unwrap();
        let mut reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            samples
        );
    }
}
