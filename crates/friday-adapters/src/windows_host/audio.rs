use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use friday_engine::{AudioInput, Failure, JobContext, PcmClip, PortFuture};
use rtrb::{Consumer, Producer, RingBuffer};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::pcm::encode_capture;

const OVERFLOW: u8 = 1;
const DEVICE_LOST: u8 = 2;

#[derive(Default)]
pub struct WindowsAudio;

impl WindowsAudio {
    pub async fn input_available() -> bool {
        tokio::task::spawn_blocking(|| {
            cpal::default_host()
                .default_input_device()
                .is_some_and(|device| device.default_input_config().is_ok())
        })
        .await
        .unwrap_or(false)
    }
}

impl AudioInput for WindowsAudio {
    fn capture(
        &self,
        finish: CancellationToken,
        context: JobContext,
        started: oneshot::Sender<()>,
    ) -> PortFuture<'_, PcmClip> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || capture(finish, context, started))
                .await
                .map_err(|_| Failure::AudioUnavailable)?
        })
    }
}

fn capture(
    finish: CancellationToken,
    context: JobContext,
    started: oneshot::Sender<()>,
) -> Result<PcmClip, Failure> {
    context.check()?;
    if finish.is_cancelled() {
        return Err(Failure::EmptyCapture);
    }
    let device = cpal::default_host()
        .default_input_device()
        .ok_or(Failure::AudioUnavailable)?;
    let supported = device
        .default_input_config()
        .map_err(|_| Failure::AudioUnavailable)?;
    let config = supported.config();
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    if !(1..=8).contains(&channels) || !(8_000..=96_000).contains(&rate) {
        return Err(Failure::AudioUnavailable);
    }

    let (producer, mut consumer) = RingBuffer::new(rate as usize * channels / 4);
    let error = Arc::new(AtomicU8::new(0));
    let stream = match supported.sample_format() {
        SampleFormat::F32 => make_stream::<f32>(&device, config, producer, error.clone()),
        SampleFormat::I16 => make_stream::<i16>(&device, config, producer, error.clone()),
        SampleFormat::U16 => make_stream::<u16>(&device, config, producer, error.clone()),
        SampleFormat::I32 => make_stream::<i32>(&device, config, producer, error.clone()),
        _ => Err(Failure::AudioUnavailable),
    }?;

    context.check()?;
    if finish.is_cancelled() {
        return Err(Failure::EmptyCapture);
    }
    if super::session::current_session() != friday_engine::SessionStatus::Unlocked {
        return Err(Failure::Locked);
    }
    stream.play().map_err(|_| Failure::AudioUnavailable)?;
    let _ = started.send(());
    let mut frames = Downmix::new(rate as usize * 30, channels);
    while !finish.is_cancelled() && tokio::time::Instant::now() < context.deadline() {
        if context.token().is_cancelled() {
            return Err(Failure::Cancelled);
        }
        check_stream(&error)?;
        frames.drain(&mut consumer);
        if frames.full() {
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    // Destroy the stream before draining the tail and before sending audio to the network.
    drop(stream);
    if context.token().is_cancelled() {
        return Err(Failure::Cancelled);
    }
    check_stream(&error)?;
    frames.drain(&mut consumer);
    encode_capture(frames.mono, rate)
}

fn make_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut producer: Producer<f32>,
    error: Arc<AtomicU8>,
) -> Result<Stream, Failure>
where
    T: SizedSample + Copy,
    f32: FromSample<T>,
{
    let overflow = error.clone();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                if overflow.load(Ordering::Relaxed) != 0 {
                    return;
                }
                for sample in data {
                    if producer.push(f32::from_sample(*sample)).is_err() {
                        overflow.store(OVERFLOW, Ordering::Release);
                        break;
                    }
                }
            },
            move |_| error.store(DEVICE_LOST, Ordering::Release),
            None,
        )
        .map_err(|_| Failure::AudioUnavailable)
}

fn check_stream(error: &AtomicU8) -> Result<(), Failure> {
    match error.load(Ordering::Acquire) {
        OVERFLOW => Err(Failure::AudioOverflow),
        DEVICE_LOST => Err(Failure::AudioLost),
        _ => Ok(()),
    }
}

struct Downmix {
    mono: Vec<f32>,
    limit: usize,
    channels: usize,
    sum: f32,
    remaining: usize,
}

impl Downmix {
    fn new(limit: usize, channels: usize) -> Self {
        Self {
            mono: Vec::with_capacity(limit),
            limit,
            channels,
            sum: 0.0,
            remaining: channels,
        }
    }

    fn full(&self) -> bool {
        self.mono.len() == self.limit
    }

    fn drain(&mut self, input: &mut Consumer<f32>) {
        while !self.full() {
            let Ok(sample) = input.pop() else { break };
            self.sum += sample;
            self.remaining -= 1;
            if self.remaining == 0 {
                self.mono.push(self.sum / self.channels as f32);
                self.sum = 0.0;
                self.remaining = self.channels;
            }
        }
    }
}
