//! Turning whatever the sound card produces into what the VAD expects.
//!
//! Mono, `f32`, 16 kHz. The device may offer none of those, so downmixing and
//! resampling happen here — pure functions over slices, so they are testable
//! without a sound card and can run inside the capture callback.

/// Downmix interleaved frames to mono by averaging channels.
///
/// A trailing partial frame (fewer samples than `channels`) is dropped rather
/// than averaged against silence: it is a torn read, and inventing a sample for
/// it puts a click in the audio.
#[must_use]
pub fn to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let channels = usize::from(channels).max(1);
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Resample by nearest-neighbour decimation, as `govox-py` does.
///
/// Deliberately not interpolating and deliberately not filtering. This is a
/// point-sampling resampler: it aliases, and on a 48 kHz → 16 kHz path it is
/// simply taking every third sample. That is acceptable because the only
/// consumers are Silero and Whisper, both trained on ordinary speech and both
/// robust to it, and because the alternative — a real polyphase filter — is a
/// dependency and a latency cost for no measured accuracy gain. If WER ever
/// regresses on a non-16 kHz device, this is the first thing to suspect.
#[must_use]
pub fn resample(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate || samples.is_empty() || source_rate == 0 || target_rate == 0 {
        return samples.to_vec();
    }

    let ratio = f64::from(target_rate) / f64::from(source_rate);
    let target_len = ((samples.len() as f64 * ratio).round() as usize).max(1);
    if target_len == samples.len() {
        return samples.to_vec();
    }

    let step = samples.len() as f64 / target_len as f64;
    (0..target_len)
        .map(|index| {
            let source = (index as f64 * step) as usize;
            samples[source.min(samples.len() - 1)]
        })
        .collect()
}

/// Downmix and resample in one step: the whole capture-side conversion.
#[must_use]
pub fn normalize_to_mono(
    interleaved: &[f32],
    channels: u16,
    source_rate: u32,
    target_rate: u32,
) -> Vec<f32> {
    let mono = to_mono(interleaved, channels);
    if source_rate == target_rate {
        return mono;
    }
    resample(&mono, source_rate, target_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_passes_through_untouched() {
        assert_eq!(to_mono(&[0.1, 0.2, 0.3], 1), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn stereo_is_averaged() {
        assert_eq!(to_mono(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
    }

    #[test]
    fn a_torn_trailing_frame_is_dropped() {
        // Three samples over two channels: the lone third would be averaged
        // against a sample that does not exist yet.
        assert_eq!(to_mono(&[1.0, 1.0, 0.7], 2), vec![1.0]);
    }

    #[test]
    fn zero_channels_is_treated_as_mono_rather_than_dividing_by_zero() {
        assert_eq!(to_mono(&[0.25, 0.5], 0), vec![0.25, 0.5]);
    }

    #[test]
    fn a_matching_rate_is_a_no_op() {
        let samples: Vec<f32> = (0..100).map(|i| i as f32).collect();
        assert_eq!(resample(&samples, 16_000, 16_000), samples);
    }

    #[test]
    fn downsampling_three_to_one_takes_every_third_sample() {
        let samples: Vec<f32> = (0..9).map(|i| i as f32).collect();
        assert_eq!(resample(&samples, 48_000, 16_000), vec![0.0, 3.0, 6.0]);
    }

    #[test]
    fn upsampling_repeats_the_nearest_sample() {
        assert_eq!(
            resample(&[0.0, 1.0], 8_000, 16_000),
            vec![0.0, 0.0, 1.0, 1.0]
        );
    }

    #[test]
    fn an_empty_block_stays_empty() {
        assert!(resample(&[], 48_000, 16_000).is_empty());
        assert!(normalize_to_mono(&[], 2, 48_000, 16_000).is_empty());
    }

    #[test]
    fn a_block_shorter_than_the_ratio_still_yields_one_sample() {
        // 2 samples at 48 kHz round to 0.67 → clamped to 1, never to an empty
        // block. An empty frame would divide by zero in frame_duration_ms.
        assert_eq!(resample(&[0.4, 0.9], 48_000, 16_000).len(), 1);
    }

    #[test]
    fn the_index_never_runs_off_the_end() {
        // Rounding up can ask for one more output sample than the step allows.
        for len in 1..200_usize {
            let samples: Vec<f32> = (0..len).map(|i| i as f32).collect();
            let out = resample(&samples, 44_100, 16_000);
            assert!(!out.is_empty(), "len={len} produced nothing");
        }
    }

    #[test]
    fn a_full_capture_block_converts_in_one_call() {
        // 30 ms of 48 kHz stereo = 1440 frames = 2880 interleaved samples,
        // becoming 480 mono samples at 16 kHz — one VAD frame exactly.
        let interleaved: Vec<f32> = (0..2880).map(|i| (i % 7) as f32 / 7.0).collect();
        let out = normalize_to_mono(&interleaved, 2, 48_000, 16_000);
        assert_eq!(out.len(), 480);
    }
}
