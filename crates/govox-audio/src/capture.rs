//! Opening a microphone and turning its callback into a stream of frames.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use govox_core::audio::normalize_to_mono;
use govox_core::domain::AudioFrame;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no input device available")]
    NoDevice,
    #[error("microphone {name:?} not found")]
    DeviceNotFound { name: String },
    #[error("microphone configuration rejected: {0}")]
    UnsupportedConfig(String),
    #[error("microphone stream failed: {0}")]
    Stream(String),
}

impl From<CaptureError> for govox_core::domain::GovoxError {
    fn from(error: CaptureError) -> Self {
        Self::MicrophoneUnavailable(error.to_string())
    }
}

/// An input device, as `govox devices` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub index: usize,
    pub name: String,
    pub channels: u16,
    pub default_sample_rate: u32,
    /// Whether this is the host's default input.
    pub is_default: bool,
}

/// Every input device this host can see.
///
/// A device that fails to report its config is skipped rather than erroring:
/// one broken device must not make `govox devices` useless.
#[must_use]
pub fn list_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };

    devices
        .enumerate()
        .filter_map(|(index, device)| {
            let name = device.name().ok()?;
            let config = device.default_input_config().ok()?;
            Some(DeviceInfo {
                index,
                is_default: name == default_name,
                name,
                channels: config.channels(),
                default_sample_rate: config.sample_rate().0,
            })
        })
        .collect()
}

/// A running capture. Dropping it stops the stream.
///
/// `cpal::Stream` is `!Send`, so it stays on the thread that built it; this
/// handle is what crosses task boundaries.
pub struct MicrophoneCapture {
    frames: mpsc::Receiver<AudioFrame>,
    errors: mpsc::UnboundedReceiver<CaptureError>,
    /// Kept alive to keep the stream open; never read.
    _stream: StreamGuard,
}

/// Parks the `!Send` stream on its own thread and stops it on drop.
struct StreamGuard {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl MicrophoneCapture {
    /// Open `device` (or the default when empty) and start capturing.
    ///
    /// Audio is delivered as `frame_ms` frames of mono `f32` at `sample_rate`,
    /// whatever the device natively offers.
    ///
    /// # Errors
    /// If no input device exists, the named one is absent, or the device
    /// rejects the configuration.
    pub fn start(
        device: &str,
        sample_rate: u32,
        frame_ms: u32,
        queue_frames: usize,
    ) -> Result<Self, CaptureError> {
        let (frame_tx, frames) = mpsc::channel(queue_frames.max(1));
        let (error_tx, errors) = mpsc::unbounded_channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();

        let device = device.to_owned();
        let thread = std::thread::Builder::new()
            .name("govox-capture".to_owned())
            .spawn(move || {
                match build_stream(&device, sample_rate, frame_ms, frame_tx, error_tx) {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        // Park until the handle is dropped. The stream must
                        // outlive this scope or cpal stops it immediately.
                        let _ = stop_rx.recv();
                        drop(stream);
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|e| CaptureError::Stream(e.to_string()))?;

        match ready.recv() {
            Ok(Ok(())) => Ok(Self {
                frames,
                errors,
                _stream: StreamGuard {
                    stop: Some(stop_tx),
                    thread: Some(thread),
                },
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(CaptureError::Stream(
                "capture thread died before reporting".to_owned(),
            )),
        }
    }

    /// The next frame, or `None` once the stream has stopped.
    pub async fn next_frame(&mut self) -> Option<AudioFrame> {
        self.frames.recv().await
    }

    /// A device error reported since the last check, if any.
    pub fn take_error(&mut self) -> Option<CaptureError> {
        self.errors.try_recv().ok()
    }
}

fn build_stream(
    device_name: &str,
    sample_rate: u32,
    frame_ms: u32,
    frames: mpsc::Sender<AudioFrame>,
    errors: mpsc::UnboundedSender<CaptureError>,
) -> Result<cpal::Stream, CaptureError> {
    let host = cpal::default_host();
    let device = if device_name.is_empty() {
        host.default_input_device().ok_or(CaptureError::NoDevice)?
    } else {
        host.input_devices()
            .map_err(|e| CaptureError::Stream(e.to_string()))?
            .find(|d| d.name().is_ok_and(|n| n == device_name))
            .ok_or_else(|| CaptureError::DeviceNotFound {
                name: device_name.to_owned(),
            })?
    };

    let default = device
        .default_input_config()
        .map_err(|e| CaptureError::UnsupportedConfig(e.to_string()))?;
    let source_rate = default.sample_rate().0;
    let channels = default.channels();
    let config: cpal::StreamConfig = default.into();

    // Timestamps are derived from the sample count rather than read from the
    // clock. The callback can be late or coalesced, and a wall-clock reading
    // would put that jitter into the utterance boundaries the VAD produces;
    // the sample count is exact by construction.
    let samples_seen = Arc::new(AtomicU64::new(0));
    let target_len = (sample_rate as usize * frame_ms as usize / 1000).max(1);
    let mut pending: Vec<f32> = Vec::with_capacity(target_len * 2);

    let error_sink = errors.clone();
    let stream = device
        .build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                pending.extend(normalize_to_mono(data, channels, source_rate, sample_rate));

                while pending.len() >= target_len {
                    let chunk: Vec<f32> = pending.drain(..target_len).collect();
                    let seen = samples_seen.fetch_add(target_len as u64, Ordering::Relaxed);
                    let frame = AudioFrame {
                        samples: Arc::from(chunk),
                        sample_rate,
                        timestamp: seen as f64 / f64::from(sample_rate),
                    };
                    // try_send, never blocking send: this is the audio
                    // callback. Dropping a frame under backpressure costs one
                    // 30 ms window; blocking here would underrun the device and
                    // corrupt everything after it.
                    if frames.try_send(frame).is_err() {
                        tracing::warn!("audio frame dropped: consumer is not keeping up");
                    }
                }
            },
            move |err| {
                let _ = error_sink.send(CaptureError::Stream(err.to_string()));
            },
            None,
        )
        .map_err(|e| CaptureError::UnsupportedConfig(e.to_string()))?;

    stream
        .play()
        .map_err(|e| CaptureError::Stream(e.to_string()))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_errors_become_microphone_unavailable() {
        let error: govox_core::domain::GovoxError = CaptureError::NoDevice.into();
        assert!(matches!(
            error,
            govox_core::domain::GovoxError::MicrophoneUnavailable(_)
        ));
    }

    #[test]
    fn a_missing_named_device_says_which_one() {
        let error = CaptureError::DeviceNotFound {
            name: "Blue Yeti".to_owned(),
        };
        assert!(error.to_string().contains("Blue Yeti"));
    }

    /// Requires a real microphone; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs an input device"]
    fn the_default_device_is_listed() {
        let devices = list_devices();
        assert!(!devices.is_empty(), "no input devices found");
        assert!(devices.iter().any(|d| d.is_default));
    }
}
