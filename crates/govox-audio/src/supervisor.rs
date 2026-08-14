//! Keeping capture alive across device drops.
//!
//! A microphone disappears for ordinary reasons — suspend/resume, a USB
//! re-enumeration, PipeWire restarting — and none of them should end the
//! daemon. The supervisor reopens the device with exponential backoff and hands
//! the caller an uninterrupted stream of frames.

use std::time::Duration;

use govox_core::domain::AudioFrame;
use tokio_util::sync::CancellationToken;

use crate::capture::{CaptureError, MicrophoneCapture};

/// Exponential backoff with a ceiling.
///
/// Pure, so the schedule is asserted directly rather than inferred from
/// sleeps in a test.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub base: Duration,
    pub max: Duration,
    /// `None` retries forever, which is the daemon's default: a microphone
    /// that is gone at 09:00 is usually back by 09:01, and a daemon that gave
    /// up is one the user has to notice and restart by hand.
    pub max_attempts: Option<u32>,
    attempt: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(500),
            max: Duration::from_secs(5),
            max_attempts: None,
            attempt: 0,
        }
    }
}

impl Backoff {
    #[must_use]
    pub fn new(base: Duration, max: Duration, max_attempts: Option<u32>) -> Self {
        Self {
            base,
            max,
            max_attempts,
            attempt: 0,
        }
    }

    /// How long to wait before the next attempt, or `None` to give up.
    pub fn next_delay(&mut self) -> Option<Duration> {
        self.attempt += 1;
        if self.max_attempts.is_some_and(|limit| self.attempt > limit) {
            return None;
        }
        // Saturating rather than wrapping: `2^(attempt-1)` overflows a u32 at
        // attempt 33, and a wrapping result would hand back a *short* delay —
        // turning a backoff into a hot loop against a device that is never
        // coming back.
        let scaled = self
            .base
            .saturating_mul(2_u32.saturating_pow(self.attempt - 1));
        Some(scaled.min(self.max))
    }

    /// A frame arrived, so the device is healthy.
    ///
    /// Clears the counter so a later, unrelated drop starts its backoff — and
    /// any bounded attempt budget — fresh rather than cumulatively.
    pub fn note_healthy(&mut self) {
        self.attempt = 0;
    }

    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempt
    }
}

/// Told when the microphone drops and when it comes back.
pub trait CaptureObserver: Send {
    fn on_lost(&self, error: &CaptureError, retry_in: Duration);
    fn on_recovered(&self) {}
}

impl CaptureObserver for () {
    fn on_lost(&self, _error: &CaptureError, _retry_in: Duration) {}
}

/// Reopens the microphone as needed and forwards frames.
pub struct CaptureSupervisor {
    device: String,
    sample_rate: u32,
    frame_ms: u32,
    queue_frames: usize,
    backoff: Backoff,
}

impl CaptureSupervisor {
    #[must_use]
    pub fn new(
        device: impl Into<String>,
        sample_rate: u32,
        frame_ms: u32,
        queue_frames: usize,
        backoff: Backoff,
    ) -> Self {
        Self {
            device: device.into(),
            sample_rate,
            frame_ms,
            queue_frames,
            backoff,
        }
    }

    /// Capture until `cancel` fires or the attempt budget is exhausted.
    ///
    /// `on_frame` runs on the caller's task, not in the audio callback.
    ///
    /// # Errors
    /// Returns the last device error if reconnection gives up.
    pub async fn run<F, O>(
        mut self,
        cancel: &CancellationToken,
        observer: O,
        mut on_frame: F,
    ) -> Result<(), CaptureError>
    where
        F: FnMut(AudioFrame) + Send,
        O: CaptureObserver,
    {
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }

            // A non-zero attempt count *is* "we are recovering from a loss":
            // it is set by every failure path and cleared by the first healthy
            // frame, so a separate flag would only be a second copy of it.
            let recovering = self.backoff.attempts() > 0;

            match MicrophoneCapture::start(
                &self.device,
                self.sample_rate,
                self.frame_ms,
                self.queue_frames,
            ) {
                Ok(mut capture) => {
                    if recovering {
                        observer.on_recovered();
                    }
                    match drive(&mut capture, cancel, &mut self.backoff, &mut on_frame).await {
                        // The stream ended cleanly: a device EOF, not a
                        // failure. Nothing to reconnect to.
                        None => return Ok(()),
                        Some(error) => self.wait_or_give_up(&error, cancel, &observer).await?,
                    }
                }
                Err(error) => self.wait_or_give_up(&error, cancel, &observer).await?,
            }
        }
    }

    async fn wait_or_give_up<O: CaptureObserver>(
        &mut self,
        error: &CaptureError,
        cancel: &CancellationToken,
        observer: &O,
    ) -> Result<(), CaptureError> {
        let Some(delay) = self.backoff.next_delay() else {
            tracing::error!(
                attempts = self.backoff.attempts(),
                "microphone unavailable; giving up"
            );
            return Err(CaptureError::Stream(error.to_string()));
        };

        tracing::warn!(
            %error,
            retry_in_s = delay.as_secs_f64(),
            attempt = self.backoff.attempts(),
            "microphone unavailable; reconnecting"
        );
        observer.on_lost(error, delay);

        // Interruptible: a clean shutdown must not sleep out the full backoff.
        tokio::select! {
            () = cancel.cancelled() => {}
            () = tokio::time::sleep(delay) => {}
        }
        Ok(())
    }
}

/// Pump one stream to exhaustion. `Some(error)` means the device dropped.
async fn drive<F>(
    capture: &mut MicrophoneCapture,
    cancel: &CancellationToken,
    backoff: &mut Backoff,
    on_frame: &mut F,
) -> Option<CaptureError>
where
    F: FnMut(AudioFrame),
{
    loop {
        tokio::select! {
            () = cancel.cancelled() => return None,
            frame = capture.next_frame() => match frame {
                Some(frame) => {
                    backoff.note_healthy();
                    on_frame(frame);
                }
                None => {
                    // The channel closed. A device error, if one was reported,
                    // explains why; otherwise this is a clean end of stream.
                    return capture.take_error();
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backoff() -> Backoff {
        Backoff::new(Duration::from_millis(500), Duration::from_secs(5), None)
    }

    #[test]
    fn delays_double_up_to_the_ceiling() {
        let mut b = backoff();
        let delays: Vec<u64> = (0..8)
            .map(|_| b.next_delay().expect("unbounded").as_millis() as u64)
            .collect();
        assert_eq!(delays, [500, 1000, 2000, 4000, 5000, 5000, 5000, 5000]);
    }

    #[test]
    fn a_healthy_frame_resets_the_schedule() {
        let mut b = backoff();
        b.next_delay();
        b.next_delay();
        assert_eq!(b.attempts(), 2);

        b.note_healthy();
        assert_eq!(b.attempts(), 0);
        // The next drop starts from the base again, not from where it left off.
        assert_eq!(b.next_delay(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn a_bounded_budget_eventually_gives_up() {
        let mut b = Backoff::new(Duration::from_millis(10), Duration::from_secs(1), Some(3));
        assert!(b.next_delay().is_some());
        assert!(b.next_delay().is_some());
        assert!(b.next_delay().is_some());
        assert_eq!(b.next_delay(), None, "the fourth attempt is over budget");
    }

    #[test]
    fn a_long_outage_never_shortens_the_delay() {
        // The shift in `2^(attempt-1)` overflows a u32 at attempt 33. Wrapping
        // there would hand back a tiny delay and turn the backoff into a hot
        // retry loop against a device that is not coming back.
        let mut b = backoff();
        for _ in 0..4 {
            b.next_delay(); // ramp up to the ceiling.
        }
        // Well past attempt 33, where the shift overflows.
        for attempt in 0..200 {
            assert_eq!(
                b.next_delay(),
                Some(Duration::from_secs(5)),
                "delay shortened at attempt {}",
                attempt + 5
            );
        }
    }

    #[test]
    fn a_zero_base_still_terminates() {
        let mut b = Backoff::new(Duration::ZERO, Duration::from_secs(5), Some(2));
        assert_eq!(b.next_delay(), Some(Duration::ZERO));
        assert_eq!(b.next_delay(), Some(Duration::ZERO));
        assert_eq!(b.next_delay(), None);
    }
}
