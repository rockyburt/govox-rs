//! Whisper recognition, on a thread of its own.
//!
//! `WhisperState` is `Send` but not `Sync`, and transcription is a multi-second
//! CPU/GPU burn. Wrapping it in a `Mutex` and calling `spawn_blocking` would put
//! shared mutable state back in the middle of the daemon — the exact thing this
//! port is trying to be rid of.
//!
//! Instead the model lives on one dedicated `std::thread` that owns it
//! outright, fed by an mpsc channel with oneshot replies. Callers get an
//! `async fn transcribe`, the model is never shared, and the queue is where
//! backpressure becomes visible rather than a lock.

use std::sync::Arc;

use govox_core::config::{RecognitionConfig, RecognitionDevice};
use govox_core::domain::{AudioBuffer, GovoxError, PersonalDictionary};
use govox_core::streaming::TimedWord;
use tokio::sync::{mpsc, oneshot};
use whisper_rs::{
    DtwMode, DtwParameters, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
    WhisperState,
};

use crate::model::{self, ModelError};
use crate::text::{bias_prompt, postprocess_text, whisper_language};

/// DTW needs scratch memory proportional to the audio window; 128 MB is what
/// whisper.cpp's own example uses and is ample for a 30 s window.
const DTW_MEM_SIZE: usize = 128 * 1024 * 1024;

/// Which GPU backend, if any, this binary was compiled with.
///
/// whisper.cpp picks its accelerator with a cargo feature, whereas
/// `[recognition] device` is a *runtime* key in `govox-py`. A build that
/// quietly ignored `device = "cuda"` and ran on the CPU would look like a
/// working daemon that is an order of magnitude too slow, so the mismatch is
/// checked at startup and reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cpu,
    Vulkan,
    Cuda,
}

impl Backend {
    /// What this binary can actually do.
    #[must_use]
    pub const fn compiled() -> Self {
        if cfg!(feature = "cuda") {
            Self::Cuda
        } else if cfg!(feature = "vulkan") {
            Self::Vulkan
        } else {
            Self::Cpu
        }
    }

    #[must_use]
    pub const fn is_gpu(self) -> bool {
        !matches!(self, Self::Cpu)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Vulkan => "vulkan",
            Self::Cuda => "cuda",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error(transparent)]
    Model(#[from] ModelError),

    #[error(
        "[recognition] device = \"cuda\" needs a GPU-enabled build, but this \
         binary was compiled for the CPU. Rebuild with `--features vulkan` (or \
         `--features cuda`), or set device = \"cpu\" to accept CPU speed."
    )]
    GpuNotCompiledIn,

    #[error("whisper failed to load the model: {0}")]
    Load(String),

    #[error("whisper failed to transcribe: {0}")]
    Transcribe(String),

    #[error("the recognition thread has stopped")]
    Stopped,
}

impl From<AsrError> for GovoxError {
    fn from(error: AsrError) -> Self {
        Self::RecognitionFailed(error.to_string())
    }
}

/// Decide whether to ask whisper.cpp for the GPU, given config and build.
///
/// `cuda` on a Vulkan build is honoured rather than refused: the user asked for
/// *the GPU*, and a Vulkan build is running on the GPU. Refusing would break a
/// working config to make a point about backend names. `cuda` on a CPU build is
/// a hard error, because there the user's intent cannot be met at all.
///
/// # Errors
/// If GPU was requested and no GPU backend is compiled in.
pub fn resolve_gpu(device: RecognitionDevice, backend: Backend) -> Result<bool, AsrError> {
    match device {
        RecognitionDevice::Cpu => Ok(false),
        RecognitionDevice::Auto => Ok(backend.is_gpu()),
        RecognitionDevice::Cuda => {
            if !backend.is_gpu() {
                return Err(AsrError::GpuNotCompiledIn);
            }
            if backend != Backend::Cuda {
                tracing::warn!(
                    compiled = backend.name(),
                    "[recognition] device = \"cuda\" honoured by the {} backend; \
                     this build is GPU-accelerated but not via CUDA",
                    backend.name()
                );
            }
            Ok(true)
        }
    }
}

/// A request to the recognition thread.
enum Request {
    Transcribe {
        audio: Arc<[f32]>,
        reply: oneshot::Sender<Result<String, AsrError>>,
    },
    /// Like `Transcribe`, but returning per-word spans for streaming.
    TranscribeWords {
        audio: Arc<[f32]>,
        reply: oneshot::Sender<Result<Vec<TimedWord>, AsrError>>,
    },
    /// Load the model now, so the user's first utterance does not pay for it.
    WarmUp {
        reply: oneshot::Sender<Result<(), AsrError>>,
    },
}

/// A cheap, cloneable handle to the recognition thread.
#[derive(Clone)]
pub struct WhisperHandle {
    requests: mpsc::Sender<Request>,
}

impl WhisperHandle {
    /// Transcribe one utterance.
    ///
    /// # Errors
    /// If the model fails to load or decode, or the thread has stopped.
    pub async fn transcribe(&self, audio: &AudioBuffer) -> Result<String, AsrError> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Transcribe {
                audio: Arc::clone(&audio.samples),
                reply,
            })
            .await
            .map_err(|_| AsrError::Stopped)?;
        answer.await.map_err(|_| AsrError::Stopped)?
    }

    /// Transcribe with per-word timestamps, for streaming.
    ///
    /// # Errors
    /// If the model fails to load or decode, or the thread has stopped.
    pub async fn transcribe_words(&self, audio: &[f32]) -> Result<Vec<TimedWord>, AsrError> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::TranscribeWords {
                audio: Arc::from(audio.to_vec()),
                reply,
            })
            .await
            .map_err(|_| AsrError::Stopped)?;
        answer.await.map_err(|_| AsrError::Stopped)?
    }

    /// Load and warm the model so the first real utterance is not slow.
    ///
    /// # Errors
    /// If the model cannot be loaded.
    pub async fn warm_up(&self) -> Result<(), AsrError> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::WarmUp { reply })
            .await
            .map_err(|_| AsrError::Stopped)?;
        answer.await.map_err(|_| AsrError::Stopped)?
    }
}

/// The streaming seam, satisfied by delegating to the inherent methods.
///
/// The inherent versions keep returning [`AsrError`] so that callers holding a
/// concrete handle still get the specific variant; the trait widens to
/// [`GovoxError`] because it is what `govox-core` can name.
impl govox_core::domain::WordRecognizer for WhisperHandle {
    async fn transcribe_words(&self, audio: &[f32]) -> Result<Vec<TimedWord>, GovoxError> {
        WhisperHandle::transcribe_words(self, audio)
            .await
            .map_err(Into::into)
    }

    async fn warm_up(&self) -> Result<(), GovoxError> {
        WhisperHandle::warm_up(self).await.map_err(Into::into)
    }
}

/// Owns the recognition thread; dropping it stops the thread.
pub struct WhisperRecognizer {
    /// `Option` so [`Drop`] can release it, closing the channel. See the
    /// `Drop` impl — this is load-bearing, not tidiness.
    handle: Option<WhisperHandle>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WhisperRecognizer {
    /// Start the recognition thread.
    ///
    /// The model is *not* loaded yet — call [`WhisperHandle::warm_up`] for
    /// that. Startup stays fast and a missing model surfaces where the daemon
    /// can report it.
    ///
    /// # Errors
    /// If the configured device cannot be honoured by this build.
    pub fn start(
        config: &RecognitionConfig,
        dictionary: &PersonalDictionary,
        queue_depth: usize,
    ) -> Result<Self, AsrError> {
        let backend = Backend::compiled();
        let use_gpu = resolve_gpu(config.device, backend)?;
        tracing::info!(
            model = %config.model,
            backend = backend.name(),
            use_gpu,
            // Logged because the index is the driver's enumeration order: if it
            // ever shifts, the only symptom is that everything gets slower.
            gpu_device = config.gpu_device,
            "recognition configured"
        );

        let (tx, rx) = mpsc::channel(queue_depth.max(1));
        let worker = Worker {
            config: config.clone(),
            prompt: bias_prompt(&dictionary.bias_terms, config.bias_prompt_token_budget),
            use_gpu,
            loaded: None,
        };

        let thread = std::thread::Builder::new()
            .name("govox-asr".to_owned())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || worker.run(rx))
            .map_err(|e| AsrError::Load(e.to_string()))?;

        Ok(Self {
            handle: Some(WhisperHandle { requests: tx }),
            thread: Some(thread),
        })
    }

    /// A handle callers can clone and hold.
    ///
    /// # Panics
    /// Never in practice: the handle is only taken during [`Drop`].
    #[must_use]
    pub fn handle(&self) -> WhisperHandle {
        self.handle.clone().expect("handle is only taken on drop")
    }
}

impl Drop for WhisperRecognizer {
    fn drop(&mut self) {
        // Release our sender so the channel can close and the worker loop can
        // end. Taking it out of the `Option` is the whole point: while this
        // struct holds a `WhisperHandle`, a sender is alive.
        drop(self.handle.take());

        // Deliberately *not* joining the thread. The first version did, to
        // release the GPU context before process exit, and it deadlocks:
        // `handle()` hands out clones, the pipeline holds one for its whole
        // run, so the channel stays open, `blocking_recv` never returns and
        // the join waits forever. The worker exits once the last handle goes
        // and the process reclaims the context regardless — a late GPU
        // teardown beats a daemon that will not quit.
        self.thread.take();
    }
}

/// The state that lives on the recognition thread and is never shared.
struct Worker {
    config: RecognitionConfig,
    prompt: String,
    use_gpu: bool,
    loaded: Option<Loaded>,
}

struct Loaded {
    /// One state, reused across decodes.
    ///
    /// `create_state` allocates the KV caches; at a 0.25 s chunk size the
    /// streaming path was doing that several times a second. The state holds
    /// its own `Arc` on the context, so it can be stored beside it.
    ///
    /// **Declared before `context` on purpose.** Struct fields drop in
    /// declaration order, and freeing the context's GPU backend before the
    /// state that is still holding buffers on it segfaults on teardown.
    state: WhisperState,
    /// Kept alive alongside the state, and needed to build a fresh one if the
    /// reused state ever has to be replaced.
    #[allow(dead_code, reason = "owns the loaded model; the state is what decodes")]
    context: WhisperContext,
}

impl Worker {
    fn run(mut self, mut requests: mpsc::Receiver<Request>) {
        while let Some(request) = requests.blocking_recv() {
            match request {
                Request::WarmUp { reply } => {
                    let _ = reply.send(self.ensure_loaded().map(|_| ()));
                }
                Request::Transcribe { audio, reply } => {
                    let _ = reply.send(self.transcribe(&audio));
                }
                Request::TranscribeWords { audio, reply } => {
                    let _ = reply.send(self.transcribe_words(&audio));
                }
            }
        }

        // Leak the model rather than free it.
        //
        // `WhisperRecognizer::drop` deliberately does not join this thread (see
        // the comment there: joining deadlocks, because a handle held by the
        // pipeline keeps the channel open). So this runs concurrently with the
        // process exiting, and freeing GPU buffers underneath a main thread
        // that is already unmapping segfaults inside the Vulkan driver:
        //
        //   whisper_free_state -> ggml_backend_sched_free -> vk::Device::freeMemory
        //   -> libnvidia-glcore, while thread 1 sits in munmap
        //
        // The loop only ends when the recognizer is going away, which in this
        // codebase means the process is going away too, and the kernel reclaims
        // both the mapping and the device allocations on exit. That was already
        // the effective behaviour — the comment on `Drop` says the process
        // reclaims the context regardless — this just stops us racing to do it
        // by hand. Costs nothing at runtime and makes shutdown faster.
        //
        // Freeing properly needs a shutdown that waits for this thread, which
        // needs the handle lifetime problem solved first.
        std::mem::forget(self.loaded.take());

        tracing::debug!("recognition thread stopped");
    }

    fn ensure_loaded(&mut self) -> Result<&mut Loaded, AsrError> {
        if self.loaded.is_none() {
            let resolved = model::resolve(&self.config)?;
            tracing::info!(
                model = %resolved.name,
                path = %resolved.path.display(),
                "loading whisper model"
            );
            let started = std::time::Instant::now();

            let mut params = WhisperContextParameters::default();
            params.use_gpu(self.use_gpu);
            // Which GPU. 0 is the driver's first device, which on a laptop with
            // switchable graphics is usually the integrated one — so leaving
            // this unset can silently pick the slow card.
            params.gpu_device(self.config.gpu_device);
            // DTW must be configured here, at context construction — unlike
            // faster-whisper, where word timestamps are a per-call argument.
            // Getting it wrong is silent: timestamps come back as zeros.
            params.dtw_parameters(DtwParameters {
                mode: DtwMode::ModelPreset {
                    model_preset: resolved.dtw_preset,
                },
                dtw_mem_size: DTW_MEM_SIZE,
            });

            let path = resolved.path.to_string_lossy().into_owned();
            let context = WhisperContext::new_with_params(&path, params)
                .map_err(|e| AsrError::Load(e.to_string()))?;

            tracing::info!(
                model = %resolved.name,
                elapsed_s = started.elapsed().as_secs_f64(),
                "whisper model loaded"
            );
            let state = context
                .create_state()
                .map_err(|e| AsrError::Load(e.to_string()))?;
            self.loaded = Some(Loaded { context, state });
        }
        Ok(self.loaded.as_mut().expect("just loaded"))
    }

    fn transcribe(&mut self, audio: &[f32]) -> Result<String, AsrError> {
        // Whisper pads to 30 s internally, but an empty buffer has nothing to
        // pad and decodes to hallucinated text. Cheaper to refuse here.
        if audio.is_empty() {
            return Ok(String::new());
        }

        let no_speech_threshold = self.config.advanced.no_speech_threshold;

        // Load first and drop the mutable borrow, so the params (which borrow
        // the prompt) and the context can be held at the same time.
        self.ensure_loaded()?;
        let params = Self::full_params_for(&self.config, &self.prompt);
        // Disjoint field borrows: `params` holds `config` and `prompt`, this
        // holds `loaded`.
        let state = &mut self.loaded.as_mut().expect("ensure_loaded succeeded").state;
        state
            .full(params, audio)
            .map_err(|e| AsrError::Transcribe(e.to_string()))?;

        let mut text = String::new();
        for segment in state.as_iter() {
            // whisper.cpp accepts `no_speech_thold` but documents it as not
            // implemented, so it has to be applied here or it does nothing.
            // Dropping the segment keeps a cough from being typed as words.
            if f64::from(segment.no_speech_probability()) > no_speech_threshold {
                tracing::debug!(
                    probability = segment.no_speech_probability(),
                    "dropping a segment scored as non-speech"
                );
                continue;
            }
            if let Ok(chunk) = segment.to_str_lossy() {
                text.push_str(&chunk);
            }
        }

        Ok(postprocess_text(&text, false))
    }

    /// Decode with one segment per word, and return their spans.
    ///
    /// whisper.cpp has no word-timestamp flag. The idiom is
    /// `token_timestamps` + `max_len(1)` + `split_on_word`, which makes it emit
    /// one *segment* per word — so the spans come off the segments rather than
    /// off a word list as they do in faster-whisper.
    fn transcribe_words(&mut self, audio: &[f32]) -> Result<Vec<TimedWord>, AsrError> {
        if audio.is_empty() {
            return Ok(Vec::new());
        }
        let no_speech_threshold = self.config.advanced.no_speech_threshold;

        self.ensure_loaded()?;
        let mut params = Self::full_params_for(&self.config, &self.prompt);
        params.set_token_timestamps(true);
        params.set_max_len(1);
        params.set_split_on_word(true);

        let state = &mut self.loaded.as_mut().expect("ensure_loaded succeeded").state;
        state
            .full(params, audio)
            .map_err(|e| AsrError::Transcribe(e.to_string()))?;

        let mut words = Vec::new();
        for segment in state.as_iter() {
            if f64::from(segment.no_speech_probability()) > no_speech_threshold {
                continue;
            }
            let Ok(text) = segment.to_str_lossy() else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            // whisper.cpp reports timestamps in centiseconds.
            words.push(TimedWord::new(
                segment.start_timestamp() as f64 / 100.0,
                segment.end_timestamp() as f64 / 100.0,
                text.into_owned(),
            ));
        }
        Ok(words)
    }

    fn full_params_for<'a>(config: &'a RecognitionConfig, prompt: &'a str) -> FullParams<'a, 'a> {
        let advanced = &config.advanced;

        // beam_size <= 1 means greedy, matching faster-whisper's reading of
        // the same key.
        let mut params = if config.beam_size > 1 {
            FullParams::new(SamplingStrategy::BeamSearch {
                beam_size: config.beam_size as i32,
                patience: -1.0,
            })
        } else {
            FullParams::new(SamplingStrategy::Greedy { best_of: 1 })
        };

        params.set_language(whisper_language(&config.language));
        params.set_temperature(advanced.temperature as f32);
        // `temperature_inc` is deliberately left at whisper.cpp's 0.2. Turning
        // the fallback off was measured on the streaming corpus and is a bad
        // trade: raw WER 0.247 -> 0.283 and term recall 20/27 -> 16/27, to save
        // about 5% of a decode. See docs/guides/accuracy-eval.md.
        params.set_logprob_thold(advanced.log_prob_threshold as f32);
        // Approximate, not equivalent: whisper.cpp's entropy threshold is not
        // faster-whisper's compression ratio. Recorded in docs/parity.md.
        params.set_entropy_thold(advanced.compression_ratio_threshold as f32);
        // Inverted: whisper.cpp asks whether to DROP context.
        params.set_no_context(!advanced.condition_on_previous_text);

        // `n_threads` is left at whisper.cpp's default of min(4, cores). Raising
        // it was measured on the streaming corpus and does nothing here: 8
        // threads decode in 0.235s against the default's 0.235s, and 16 threads
        // are slower at 0.243s. The decode is GPU-bound, so extra host threads
        // only contend. On a CPU build the answer would differ.

        if !prompt.is_empty() {
            params.set_initial_prompt(prompt);
        }

        // Nothing goes to stdout: the daemon owns its own logging, and
        // whisper.cpp's prints would corrupt the overlay's line protocol.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        params
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_is_always_honoured() {
        for backend in [Backend::Cpu, Backend::Vulkan, Backend::Cuda] {
            assert!(!resolve_gpu(RecognitionDevice::Cpu, backend).unwrap());
        }
    }

    #[test]
    fn auto_follows_the_compiled_backend() {
        assert!(!resolve_gpu(RecognitionDevice::Auto, Backend::Cpu).unwrap());
        assert!(resolve_gpu(RecognitionDevice::Auto, Backend::Vulkan).unwrap());
        assert!(resolve_gpu(RecognitionDevice::Auto, Backend::Cuda).unwrap());
    }

    #[test]
    fn cuda_on_a_cpu_build_fails_loudly_rather_than_falling_back() {
        // The whole point of the check. A silent CPU fallback is a daemon that
        // looks fine and is ten times too slow.
        let error = resolve_gpu(RecognitionDevice::Cuda, Backend::Cpu)
            .expect_err("a CPU build cannot honour device = cuda");
        assert!(matches!(error, AsrError::GpuNotCompiledIn));

        let message = error.to_string();
        assert!(
            message.contains("--features vulkan"),
            "must say how to fix it"
        );
        assert!(
            message.contains("device = \"cpu\""),
            "must offer the alternative"
        );
    }

    #[test]
    fn cuda_on_a_vulkan_build_is_honoured() {
        // The reference config sets device = "cuda" and the shipped build is
        // Vulkan. Refusing would break a working config to make a point about
        // backend names; the user asked for the GPU and gets the GPU.
        assert!(resolve_gpu(RecognitionDevice::Cuda, Backend::Vulkan).unwrap());
    }

    /// Dropping the recogniser must not block, even with a handle outstanding.
    ///
    /// The first version joined the worker thread in `Drop`. That deadlocked:
    /// `handle()` hands out clones of the sender, and the pipeline holds one
    /// for its entire run, so the channel never closed and the join waited
    /// forever. Every test that built a recogniser hung on teardown — and so
    /// would the daemon on Ctrl-C.
    ///
    /// No model is loaded here, so this runs in the ordinary suite. It fails
    /// by hanging rather than by asserting, which is why it is worth having a
    /// watchdog thread rather than trusting the harness timeout.
    #[test]
    fn dropping_the_recognizer_does_not_block_on_an_outstanding_handle() {
        use std::sync::mpsc;

        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let mut config = govox_core::config::Config::load_from(
                None,
                &govox_core::config::Environment::default(),
            )
            .expect("defaults are valid")
            .recognition;
            // A model that will never be loaded: `start` does not touch disk.
            config.model = "tiny.en".to_owned();

            let recognizer = WhisperRecognizer::start(&config, &PersonalDictionary::default(), 2)
                .expect("starting does not need a model file");

            // The situation that deadlocked: a live clone at drop time.
            let outstanding = recognizer.handle();
            drop(recognizer);
            drop(outstanding);

            let _ = done.send(());
        });

        finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("dropping the recognizer blocked; the Drop impl has regressed");
    }

    #[test]
    fn the_compiled_backend_matches_the_enabled_feature() {
        let backend = Backend::compiled();
        if cfg!(feature = "cuda") {
            assert_eq!(backend, Backend::Cuda);
        } else if cfg!(feature = "vulkan") {
            assert_eq!(backend, Backend::Vulkan);
        } else {
            assert_eq!(backend, Backend::Cpu);
            assert!(!backend.is_gpu());
        }
    }
}
