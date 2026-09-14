//! The D-Bus control interface: start, stop or toggle dictation from another
//! program.
//!
//! Without it the only way in was the activation gesture, and a program cannot
//! perform that gesture reliably. Synthesizing a double tap through `ydotool`
//! reaches the evdev reader, but the tap window is measured on the event loop,
//! which a streaming decode stalls; a stop sent mid-dictation is the request
//! most likely to arrive too late and be read as the first tap of a new pair.
//! A method call is queued instead, so it is late at worst and never lost.
//!
//! ```text
//! busctl --user call io.github.rockyburt.Govox /io/github/rockyburt/Govox \
//!     io.github.rockyburt.Govox.Dictation Toggle
//! ```
//!
//! Each method replies with whether govox is listening once the request has
//! been handled, which is the one thing a caller cannot otherwise find out.
//!
//! # Who can call it
//!
//! Anything on the user's session bus — the same processes that can already
//! run `ydotool` or read the microphone themselves. The tray and overlay show a
//! session however it started, so a remote start is never a silent one.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use govox_core::activation::ControlRequest;
use tokio::sync::{mpsc, oneshot};

/// The well-known name, derived from where the project lives.
pub const BUS_NAME: &str = "io.github.rockyburt.Govox";
pub const OBJECT_PATH: &str = "/io/github/rockyburt/Govox";
pub const INTERFACE: &str = "io.github.rockyburt.Govox.Dictation";

/// One request, and where the event loop answers it.
#[derive(Debug)]
pub struct ControlMessage {
    pub request: ControlRequest,
    /// Whether govox is listening after the request was handled.
    ///
    /// Dropped unanswered only if the loop is gone, which the caller sees as
    /// an error rather than a guess.
    pub reply: oneshot::Sender<bool>,
}

/// The object served at [`OBJECT_PATH`].
pub struct Dictation {
    requests: mpsc::Sender<ControlMessage>,
    /// The event loop's last word on whether a session is running.
    ///
    /// Read by the `Listening` property without a round trip through the loop,
    /// so asking is answered even while a decode has it busy.
    listening: Arc<AtomicBool>,
}

impl Dictation {
    #[must_use]
    pub const fn new(requests: mpsc::Sender<ControlMessage>, listening: Arc<AtomicBool>) -> Self {
        Self {
            requests,
            listening,
        }
    }

    async fn ask(&self, request: ControlRequest) -> zbus::fdo::Result<bool> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(ControlMessage { request, reply })
            .await
            .map_err(|_| shutting_down())?;
        answer.await.map_err(|_| shutting_down())
    }
}

fn shutting_down() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed("govox is shutting down".to_owned())
}

#[zbus::interface(name = "io.github.rockyburt.Govox.Dictation")]
impl Dictation {
    /// Start dictating. Refused, returning `false`, in push-to-talk mode.
    async fn start(&self) -> zbus::fdo::Result<bool> {
        self.ask(ControlRequest::Start).await
    }

    /// Stop dictating and commit what was said.
    async fn stop(&self) -> zbus::fdo::Result<bool> {
        self.ask(ControlRequest::Stop).await
    }

    /// Start when idle, stop when listening.
    async fn toggle(&self) -> zbus::fdo::Result<bool> {
        self.ask(ControlRequest::Toggle).await
    }

    /// Whether a session is running.
    ///
    /// No change signal: a key, a silence timeout or the overlay can each end
    /// a session, and a signal that fired for only some of them would be a
    /// quieter way to be wrong than polling.
    #[zbus(property(emits_changed_signal = "false"))]
    fn listening(&self) -> bool {
        self.listening.load(Ordering::Relaxed)
    }
}

/// Claim [`BUS_NAME`] on the session bus and serve [`Dictation`].
///
/// The returned connection *is* the registration: dropping it releases the
/// name. Fails when another govox already holds it — the builder never queues
/// for a name — which leaves that daemon, not this one, answering calls.
///
/// # Errors
///
/// No session bus, or the name is taken.
pub async fn serve(dictation: Dictation) -> zbus::Result<zbus::Connection> {
    zbus::connection::Builder::session()?
        .serve_at(OBJECT_PATH, dictation)?
        .name(BUS_NAME)?
        .build()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand in for the event loop: answer each request with a fixed state.
    fn answering(state: bool) -> (Dictation, tokio::task::JoinHandle<Vec<ControlRequest>>) {
        let (requests, mut incoming) = mpsc::channel::<ControlMessage>(4);
        let seen = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(message) = incoming.recv().await {
                seen.push(message.request);
                let _ = message.reply.send(state);
            }
            seen
        });
        (
            Dictation::new(requests, Arc::new(AtomicBool::new(false))),
            seen,
        )
    }

    #[tokio::test]
    async fn each_method_sends_its_request_and_returns_the_answer() {
        let (dictation, seen) = answering(true);
        assert!(dictation.start().await.unwrap());
        assert!(dictation.stop().await.unwrap());
        assert!(dictation.toggle().await.unwrap());
        drop(dictation);
        assert_eq!(
            seen.await.unwrap(),
            [
                ControlRequest::Start,
                ControlRequest::Stop,
                ControlRequest::Toggle
            ]
        );
    }

    #[tokio::test]
    async fn a_gone_event_loop_is_an_error_not_a_guess() {
        let (requests, incoming) = mpsc::channel::<ControlMessage>(4);
        drop(incoming);
        let dictation = Dictation::new(requests, Arc::new(AtomicBool::new(false)));
        assert!(dictation.toggle().await.is_err());
    }

    #[tokio::test]
    async fn an_unanswered_request_is_an_error_not_a_guess() {
        let (requests, mut incoming) = mpsc::channel::<ControlMessage>(4);
        tokio::spawn(async move {
            // Received, then dropped without a reply: the loop shutting down
            // mid-request.
            let _ = incoming.recv().await;
        });
        let dictation = Dictation::new(requests, Arc::new(AtomicBool::new(false)));
        assert!(dictation.stop().await.is_err());
    }

    #[test]
    fn the_listening_property_reads_the_shared_flag() {
        let (requests, _incoming) = mpsc::channel::<ControlMessage>(1);
        let flag = Arc::new(AtomicBool::new(false));
        let dictation = Dictation::new(requests, Arc::clone(&flag));
        assert!(!dictation.listening());
        flag.store(true, Ordering::Relaxed);
        assert!(dictation.listening());
    }

    /// Against a real session bus: claim the name, call `Toggle` through a
    /// proxy, read the property. Ignored because it needs a desktop session,
    /// and fails while a govox daemon is running and already holds the name.
    #[tokio::test]
    #[ignore = "needs a session bus with the govox name free"]
    async fn live_session_bus_round_trip() {
        let (requests, mut incoming) = mpsc::channel::<ControlMessage>(4);
        let listening = Arc::new(AtomicBool::new(false));
        tokio::spawn(async move {
            while let Some(message) = incoming.recv().await {
                let _ = message.reply.send(true);
            }
        });
        let _server = serve(Dictation::new(requests, Arc::clone(&listening)))
            .await
            .expect("claim the name");

        let client = zbus::Connection::session().await.unwrap();
        let proxy = zbus::Proxy::new(&client, BUS_NAME, OBJECT_PATH, INTERFACE)
            .await
            .unwrap();
        let answer: bool = proxy.call("Toggle", &()).await.unwrap();
        assert!(answer);
        listening.store(true, Ordering::Relaxed);
        let property: bool = proxy.get_property("Listening").await.unwrap();
        assert!(property);
    }
}
