use audioadapter_buffers::direct::InterleavedSlice;
use friday_engine::{Failure, MAX_SAMPLES, PcmClip, SAMPLE_RATE};
use rubato::{Fft, FixedSync, Resampler};

pub(crate) fn encode_capture(mut mono: Vec<f32>, input_rate: u32) -> Result<PcmClip, Failure> {
    if !(8_000..=96_000).contains(&input_rate) || mono.len() > input_rate as usize * 30 {
        return Err(Failure::AudioUnavailable);
    }
    if mono.len() < input_rate as usize / 10
        || !mono
            .iter()
            .any(|sample| sample.is_finite() && sample.abs() > 0.002)
    {
        return Err(Failure::EmptyCapture);
    }
    for sample in &mut mono {
        *sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
    if input_rate != SAMPLE_RATE {
        let input =
            InterleavedSlice::new(&mono, 1, mono.len()).map_err(|_| Failure::AudioUnavailable)?;
        let mut resampler = Fft::<f32>::new(
            input_rate as usize,
            SAMPLE_RATE as usize,
            1024,
            1,
            FixedSync::Input,
        )
        .map_err(|_| Failure::AudioUnavailable)?;
        mono = resampler
            .process_all(&input, mono.len(), None)
            .map_err(|_| Failure::AudioUnavailable)?
            .take_data();
    }
    let samples = mono
        .into_iter()
        .take(MAX_SAMPLES)
        .map(|sample| (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16)
        .collect();
    PcmClip::new(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_wasapi_rate_to_the_stt_format() {
        let rate = 48_000;
        let wave: Vec<_> = (0..rate)
            .map(|frame| (std::f32::consts::TAU * 440.0 * frame as f32 / rate as f32).sin() * 0.5)
            .collect();
        let result = encode_capture(wave, rate).unwrap().into_samples();
        assert_eq!(result.len(), SAMPLE_RATE as usize);
        assert!(result.iter().any(|sample| sample.abs() > 10_000));
    }

    #[test]
    fn rejects_empty_silent_and_out_of_policy_input() {
        assert!(matches!(
            encode_capture(vec![0.0; 48_000], 48_000),
            Err(Failure::EmptyCapture)
        ));
        assert!(matches!(
            encode_capture(vec![1.0; 4], 48_000),
            Err(Failure::EmptyCapture)
        ));
        assert!(matches!(
            encode_capture(vec![1.0], 1),
            Err(Failure::AudioUnavailable)
        ));
    }
}
