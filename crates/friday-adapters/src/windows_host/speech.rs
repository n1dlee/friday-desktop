use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use friday_core::Locale;
use friday_engine::{Failure, JobContext, PortFuture, SpeechOutput};
use windows::{
    Foundation::TypedEventHandler,
    Media::{
        Core::MediaSource,
        Playback::MediaPlayer,
        SpeechSynthesis::{SpeechSynthesizer, VoiceInformation},
    },
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    core::HSTRING,
};
use windows_future::AsyncStatus;

#[derive(Default)]
pub struct WindowsSpeech;

impl WindowsSpeech {
    pub async fn voice_available(locale: Locale) -> bool {
        tokio::task::spawn_blocking(move || {
            let Ok(_worker) = Apartment::start() else {
                return false;
            };
            voice_for(locale).is_ok()
        })
        .await
        .unwrap_or(false)
    }
}

impl SpeechOutput for WindowsSpeech {
    fn speak(&self, text: String, locale: Locale, context: JobContext) -> PortFuture<'_, ()> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || speak(&text, locale, &context))
                .await
                .map_err(|_| Failure::SpeechUnavailable)?
        })
    }
}

struct Apartment;

impl Apartment {
    fn start() -> Result<Self, Failure> {
        // SAFETY: initialization and lifetime of the guard are on the same worker.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|_| Failure::SpeechUnavailable)?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: this worker has a matching successful RoInitialize call.
        unsafe { RoUninitialize() };
    }
}

struct Player(MediaPlayer);

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.0.Pause();
        let _ = self.0.Close();
    }
}

fn speak(text: &str, locale: Locale, context: &JobContext) -> Result<(), Failure> {
    context.check()?;
    let _apartment = Apartment::start()?;
    let synth = SpeechSynthesizer::new().map_err(|_| Failure::SpeechUnavailable)?;
    let voice = voice_for(locale)?;
    synth
        .SetVoice(&voice)
        .map_err(|_| Failure::SpeechUnavailable)?;
    let operation = synth
        .SynthesizeTextToStreamAsync(&HSTRING::from(text))
        .map_err(|_| Failure::SpeechUnavailable)?;

    let stream = loop {
        if let Err(error) = context.check() {
            let _ = operation.Cancel();
            return Err(error);
        }
        match operation.Status().map_err(|_| Failure::SpeechUnavailable)? {
            AsyncStatus::Started => std::thread::sleep(Duration::from_millis(10)),
            AsyncStatus::Completed => {
                break operation
                    .GetResults()
                    .map_err(|_| Failure::SpeechUnavailable)?;
            }
            _ => return Err(Failure::SpeechUnavailable),
        }
    };
    let content_type = stream
        .ContentType()
        .map_err(|_| Failure::SpeechUnavailable)?;
    let media = MediaSource::CreateFromStream(&stream, &content_type)
        .map_err(|_| Failure::SpeechUnavailable)?;
    let player = Player(MediaPlayer::new().map_err(|_| Failure::SpeechUnavailable)?);
    let playback = Arc::new(AtomicU8::new(0));
    let ended = playback.clone();
    let completed = player
        .0
        .MediaEnded(&TypedEventHandler::new(move |_, _| {
            ended.store(1, Ordering::Release);
            Ok(())
        }))
        .map_err(|_| Failure::SpeechUnavailable)?;
    let errored = playback.clone();
    let failed = player
        .0
        .MediaFailed(&TypedEventHandler::new(move |_, _| {
            errored.store(2, Ordering::Release);
            Ok(())
        }))
        .map_err(|_| Failure::SpeechUnavailable)?;
    player
        .0
        .SetSource(&media)
        .map_err(|_| Failure::SpeechUnavailable)?;
    context.check()?;
    if super::session::current_session() != friday_engine::SessionStatus::Unlocked {
        return Err(Failure::Locked);
    }
    player.0.Play().map_err(|_| Failure::SpeechUnavailable)?;
    let result = loop {
        if let Err(error) = context.check() {
            break Err(error);
        }
        match playback.load(Ordering::Acquire) {
            1 => break Ok(()),
            2 => break Err(Failure::SpeechUnavailable),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let _ = player.0.RemoveMediaEnded(completed);
    let _ = player.0.RemoveMediaFailed(failed);
    drop(player);
    let _ = synth.Close();
    result
}

fn voice_for(locale: Locale) -> Result<VoiceInformation, Failure> {
    let voices = SpeechSynthesizer::AllVoices().map_err(|_| Failure::SpeechUnavailable)?;
    let wanted = locale.language();
    voices
        .into_iter()
        .find(|voice| {
            voice
                .Language()
                .is_ok_and(|lang| lang.to_string_lossy().split('-').next() == Some(wanted))
        })
        .ok_or(Failure::SpeechUnavailable)
}
