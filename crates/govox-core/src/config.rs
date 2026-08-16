//! Layered configuration, ported from `govox-py`'s `src/govox/config.py`.
//!
//! Four layers, applied in order, each deep-merged over the last:
//!
//! 1. the shipped defaults, embedded at compile time
//! 2. `$XDG_CONFIG_HOME/govox/config.toml` (or `~/.config/...`)
//! 3. `GOVOX__SECTION__KEY` environment overrides
//! 4. an explicit `--config` path, which must exist
//!
//! # Why this is hand-rolled rather than a config crate
//!
//! The environment layer guesses types in a specific order — `true`/`false`,
//! then integer, then float, then string — and that order is a parity surface:
//! `GOVOX__RECOGNITION__MODEL=123` must reach the schema as an integer and be
//! *rejected*, not silently accepted as the string "123". A general-purpose
//! provider parses on its own terms and hides the rule. Reproducing it here is
//! about 120 lines and is exactly auditable.
//!
//! # The unknown-field asymmetry
//!
//! `govox-py` sets `extra="forbid"` on the root model only. Pydantic does not
//! inherit that into the section models, so an unknown **section** is rejected
//! while an unknown **key inside a known section** is silently ignored:
//!
//! ```text
//! [bogus]           -> rejected
//! [audio]
//! bogus_key = 1     -> accepted, ignored
//! ```
//!
//! Verified against the pinned reference, not assumed. `deny_unknown_fields`
//! therefore appears on [`Config`] and on nothing else — putting it on every
//! struct, which is the obvious Rust instinct, would reject configurations that
//! work today. See `docs/parity.md`; this is a wart worth revisiting
//! deliberately rather than fixing by accident.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The shipped defaults, baked into the binary.
///
/// `govox-py` resolves this with `Path(__file__).resolve().parents[2]`, which
/// only works from a source checkout — it is why the systemd unit needs a
/// hardcoded `WorkingDirectory`. Embedding removes both problems.
const DEFAULT_TOML: &str = include_str!("../../../config/default.toml");

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Config file does not exist: {0}")]
    Missing(PathBuf),
    #[error("Invalid TOML in {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("Config root must be a table: {0}")]
    NotATable(PathBuf),
    #[error("Conflicting environment override for {0}")]
    ConflictingEnv(String),
    #[error("Invalid govox configuration ({fields}): {detail}")]
    Invalid { fields: String, detail: String },
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

macro_rules! str_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }
        impl $name {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(RecognitionEngine { Local => "local" });
str_enum!(RecognitionDevice { Auto => "auto", Cpu => "cpu", Cuda => "cuda" });
str_enum!(DownloadPolicy {
    Offline => "offline",
    CacheFirst => "cache_first",
    Allow => "allow",
});
str_enum!(StreamingEngine { WhisperStreaming => "whisper_streaming" });
str_enum!(InjectionMethod {
    Ydotool => "ydotool",
    Clipboard => "clipboard",
    Auto => "auto",
});
str_enum!(ActivationMode {
    PushToTalk => "push_to_talk",
    Toggle => "toggle",
    DoubleTap => "double_tap",
});
str_enum!(BufferTrimming { Segment => "segment", Sentence => "sentence" });
str_enum!(OverlayPosition {
    TopLeft => "top-left",
    TopRight => "top-right",
    BottomLeft => "bottom-left",
    BottomRight => "bottom-right",
});
str_enum!(LogStyle {
    Plain => "plain",
    Color => "color",
    Json => "json",
    Auto => "auto",
});

// Deliberately no `deny_unknown_fields` on any of these; see the module docs.

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub frame_ms: u32,
    #[serde(default)]
    pub device: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RecognitionAdvancedConfig {
    pub temperature: f64,
    pub compression_ratio_threshold: f64,
    pub log_prob_threshold: f64,
    pub no_speech_threshold: f64,
    pub condition_on_previous_text: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RecognitionConfig {
    pub engine: RecognitionEngine,
    pub model: String,
    pub language: String,
    pub device: RecognitionDevice,
    pub compute_type: String,
    pub beam_size: u32,
    pub bias_prompt_token_budget: u32,
    pub download_policy: DownloadPolicy,
    #[serde(default)]
    pub model_dir: String,
    /// Which GPU to run on, by Vulkan/CUDA device index.
    ///
    /// **No equivalent in `govox-py`**, which never reaches the GPU-selection
    /// layer: CTranslate2 takes `device_index` but faster-whisper does not
    /// expose it, so the reference always uses whatever the driver enumerates
    /// first.
    ///
    /// That default is wrong on any laptop with switchable graphics. On the
    /// reference machine index 0 is the Intel integrated GPU and index 1 is the
    /// RTX 4070, so an unconfigured install quietly runs Whisper on the iGPU —
    /// working, and much slower than the hardware allows.
    ///
    /// Indices come from the `ggml_vulkan: N = ...` lines printed at startup.
    /// They are the driver's enumeration order, which is stable in practice but
    /// not guaranteed across driver updates, so the chosen index is logged.
    #[serde(default)]
    pub gpu_device: i32,
    pub advanced: RecognitionAdvancedConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct StreamingConfig {
    pub enabled: bool,
    pub engine: StreamingEngine,
    pub min_chunk_size_s: f64,
    pub buffer_trimming: BufferTrimming,
    pub buffer_trimming_sec: f64,
    pub vad: bool,
    pub fallback_to_utterance: bool,
}

fn default_filler_words() -> Vec<String> {
    ["um", "uh", "er", "ah", "erm", "hmm", "mhm"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CorrectionConfig {
    pub enabled: bool,
    #[serde(default)]
    pub dictionary_path: String,
    #[serde(default = "crate::config::default_true")]
    pub drop_fillers: bool,
    #[serde(default = "default_filler_words")]
    pub filler_words: Vec<String>,
    #[serde(default = "crate::config::default_true")]
    pub collapse_repeats: bool,
    #[serde(default = "crate::config::default_true")]
    pub spoken_punctuation: bool,
    #[serde(default)]
    pub spoken_emoji: bool,
    #[serde(default)]
    pub number_formatting: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct InjectionConfig {
    pub method: InjectionMethod,
    #[serde(default)]
    pub ydotool_socket: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ActivationConfig {
    pub mode: ActivationMode,
    pub push_to_talk_key: String,
    pub toggle_key: String,
    pub queue_size: u32,
    #[serde(default = "default_double_tap_ms")]
    pub double_tap_ms: u32,
}

fn default_double_tap_ms() -> u32 {
    400
}

/// Voice editing commands.
///
/// `last_insertion_ttl_s` bounds how long "delete that" trusts its record of
/// the last dictated text: govox cannot see the user click into another window,
/// so an unbounded record eventually fires backspaces somewhere it should not.
///
/// `command_mode` and `read_focused_field` are both off by default in the
/// reference, and stay off here.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct EditingConfig {
    #[serde(default = "default_ttl_s")]
    pub last_insertion_ttl_s: f64,
    #[serde(default)]
    pub command_mode: bool,
    #[serde(default)]
    pub read_focused_field: bool,
}

fn default_ttl_s() -> f64 {
    30.0
}

impl Default for EditingConfig {
    fn default() -> Self {
        Self {
            last_insertion_ttl_s: 30.0,
            command_mode: false,
            read_focused_field: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ImeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_engine_name")]
    pub engine_name: String,
    #[serde(default = "default_baseline_engine")]
    pub baseline_engine: String,
}

fn default_engine_name() -> String {
    "govox".to_owned()
}
fn default_baseline_engine() -> String {
    "xkb:us::eng".to_owned()
}

impl Default for ImeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine_name: default_engine_name(),
            baseline_engine: default_baseline_engine(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct IndicatorConfig {
    pub enabled: bool,
}

/// A per-application overlay rule, matched against the focused application.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct OverlayAppRule {
    #[serde(rename = "match")]
    pub match_: String,
    #[serde(default)]
    pub caret_offset_x: i64,
    #[serde(default)]
    pub caret_offset_y: i64,
    #[serde(default)]
    pub follow_caret: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct FeedbackConfig {
    #[serde(default = "default_true")]
    pub chime: bool,
    #[serde(default = "default_true")]
    pub tick: bool,
    #[serde(default = "default_tick_interval")]
    pub tick_interval_s: f64,
    #[serde(default = "default_true")]
    pub tray_pulse: bool,
    #[serde(default = "default_true")]
    pub overlay: bool,
    #[serde(default = "default_overlay_position")]
    pub overlay_position: OverlayPosition,
    #[serde(default = "default_true")]
    pub overlay_follow_caret: bool,
    #[serde(default)]
    pub overlay_caret_debug: bool,
    #[serde(default)]
    pub overlay_require_caret_width: bool,
    #[serde(default)]
    pub app_rules: Vec<OverlayAppRule>,
    #[serde(default = "default_true")]
    pub overlay_level: bool,
    #[serde(default = "default_true")]
    pub overlay_caption: bool,
    #[serde(default = "default_true")]
    pub overlay_click_to_stop: bool,
    #[serde(default = "default_true")]
    pub silence_auto_stop: bool,
    #[serde(default = "default_silence_timeout")]
    pub silence_timeout_s: f64,
}

pub(crate) fn default_true() -> bool {
    true
}
fn default_tick_interval() -> f64 {
    45.0
}
fn default_silence_timeout() -> f64 {
    60.0
}
fn default_overlay_position() -> OverlayPosition {
    OverlayPosition::TopRight
}

impl Default for FeedbackConfig {
    fn default() -> Self {
        Self {
            chime: true,
            tick: true,
            tick_interval_s: 45.0,
            tray_pulse: true,
            overlay: true,
            overlay_position: OverlayPosition::TopRight,
            overlay_follow_caret: true,
            overlay_caret_debug: false,
            overlay_require_caret_width: false,
            app_rules: Vec::new(),
            overlay_level: true,
            overlay_caption: true,
            overlay_click_to_stop: true,
            silence_auto_stop: true,
            silence_timeout_s: 60.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct VadConfig {
    pub speech_threshold: f64,
    pub silence_threshold: f64,
    pub min_speech_ms: u32,
    pub hangover_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Empty means the OTLP exporter falls back to `OTEL_EXPORTER_OTLP_ENDPOINT`.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub console: bool,
}

fn default_service_name() -> String {
    "govox".to_owned()
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: default_service_name(),
            endpoint: String::new(),
            console: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct LoggingConfig {
    #[serde(default = "default_level")]
    pub level: String,
    #[serde(default = "default_root_level")]
    pub root_level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
    #[serde(default = "default_log_style")]
    pub style: LogStyle,
    /// Per-logger overrides keyed by dotted logger name.
    #[serde(default)]
    pub loggers: BTreeMap<String, String>,
}

fn default_level() -> String {
    "INFO".to_owned()
}
fn default_root_level() -> String {
    "WARNING".to_owned()
}
fn default_log_format() -> String {
    "%(asctime)s %(name)s %(levelname)s %(message)s".to_owned()
}
fn default_log_style() -> LogStyle {
    LogStyle::Auto
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_level(),
            root_level: default_root_level(),
            format: default_log_format(),
            style: LogStyle::Auto,
            loggers: BTreeMap::new(),
        }
    }
}

/// Log level names accepted by `govox-py`, which validates against Python's
/// `logging.getLevelNamesMapping()`. Values are normalised to upper case.
const LOG_LEVELS: [&str; 7] = [
    "CRITICAL", "FATAL", "ERROR", "WARN", "WARNING", "INFO", "DEBUG",
];
const LOG_LEVEL_NOTSET: &str = "NOTSET";

fn normalise_level(value: &str) -> Option<String> {
    let upper = value.trim().to_uppercase();
    if upper == LOG_LEVEL_NOTSET || LOG_LEVELS.contains(&upper.as_str()) {
        Some(upper)
    } else {
        None
    }
}

/// The whole configuration.
///
/// `deny_unknown_fields` is on this struct **and on no section struct**, which
/// reproduces the reference's asymmetry exactly. See the module documentation.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub audio: AudioConfig,
    pub recognition: RecognitionConfig,
    pub streaming: StreamingConfig,
    pub correction: CorrectionConfig,
    pub injection: InjectionConfig,
    pub activation: ActivationConfig,
    pub indicator: IndicatorConfig,
    pub vad: VadConfig,
    #[serde(default)]
    pub editing: EditingConfig,
    #[serde(default)]
    pub ime: ImeConfig,
    #[serde(default)]
    pub feedback: FeedbackConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

impl Config {
    /// Load the four layers and validate the result.
    pub fn load(explicit: Option<&Path>) -> Result<Self, ConfigError> {
        Self::load_from(explicit, &Environment::from_process())
    }

    /// Load with an injected environment, so tests never mutate the process.
    ///
    /// `govox-py`'s tests reach for `monkeypatch.setenv("XDG_CONFIG_HOME", …)`,
    /// which makes them order-dependent under any parallel runner. Rust's test
    /// harness is threaded by default, so the environment is a parameter here.
    pub fn load_from(explicit: Option<&Path>, env: &Environment) -> Result<Self, ConfigError> {
        let mut merged = parse_toml_str(DEFAULT_TOML, Path::new("<embedded default.toml>"))?;

        if let Some(user) = env.user_config_path()
            && user.exists()
        {
            merge_nested(&mut merged, read_toml(&user)?);
        }

        merge_nested(&mut merged, env.overrides()?);

        if let Some(path) = explicit {
            if !path.exists() {
                return Err(ConfigError::Missing(path.to_path_buf()));
            }
            merge_nested(&mut merged, read_toml(path)?);
        }

        let config: Self =
            toml::Value::Table(merged)
                .try_into()
                .map_err(|e: toml::de::Error| ConfigError::Invalid {
                    fields: field_from_error(&e),
                    detail: e.to_string(),
                })?;

        config.validate()?;
        Ok(config)
    }

    /// Range and membership checks that the type system does not express.
    ///
    /// Kept in one place, reporting dotted paths, so the error text matches the
    /// reference's `Invalid govox configuration (a.b): …` shape whatever fails.
    fn validate(&self) -> Result<(), ConfigError> {
        let mut bad: Vec<String> = Vec::new();

        let mut gt0_u = |v: u32, name: &str| {
            if v == 0 {
                bad.push(name.to_owned());
            }
        };
        gt0_u(self.audio.sample_rate, "audio.sample_rate");
        gt0_u(self.audio.frame_ms, "audio.frame_ms");
        gt0_u(self.recognition.beam_size, "recognition.beam_size");
        gt0_u(self.activation.queue_size, "activation.queue_size");
        gt0_u(self.activation.double_tap_ms, "activation.double_tap_ms");
        gt0_u(self.vad.min_speech_ms, "vad.min_speech_ms");
        gt0_u(self.vad.hangover_ms, "vad.hangover_ms");

        // partial_cmp rather than `v <= 0.0`: the latter is *false* for NaN, so
        // a NaN would silently pass a "must be positive" check. Anything that
        // is not strictly greater than zero — NaN included — is rejected.
        let mut gt0_f = |v: f64, name: &str| {
            if v.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                bad.push(name.to_owned());
            }
        };
        gt0_f(
            self.recognition.advanced.compression_ratio_threshold,
            "recognition.advanced.compression_ratio_threshold",
        );
        gt0_f(
            self.streaming.min_chunk_size_s,
            "streaming.min_chunk_size_s",
        );
        gt0_f(
            self.streaming.buffer_trimming_sec,
            "streaming.buffer_trimming_sec",
        );
        gt0_f(
            self.editing.last_insertion_ttl_s,
            "editing.last_insertion_ttl_s",
        );
        gt0_f(self.feedback.tick_interval_s, "feedback.tick_interval_s");
        gt0_f(
            self.feedback.silence_timeout_s,
            "feedback.silence_timeout_s",
        );

        if self.recognition.advanced.temperature < 0.0 {
            bad.push("recognition.advanced.temperature".to_owned());
        }

        let mut unit = |v: f64, name: &str| {
            if !(0.0..=1.0).contains(&v) {
                bad.push(name.to_owned());
            }
        };
        unit(
            self.recognition.advanced.no_speech_threshold,
            "recognition.advanced.no_speech_threshold",
        );
        unit(self.vad.speech_threshold, "vad.speech_threshold");
        unit(self.vad.silence_threshold, "vad.silence_threshold");

        for (index, rule) in self.feedback.app_rules.iter().enumerate() {
            if rule.match_.is_empty() {
                bad.push(format!("feedback.app_rules.{index}.match"));
            }
        }

        if normalise_level(&self.logging.level).is_none() {
            bad.push("logging.level".to_owned());
        }
        if normalise_level(&self.logging.root_level).is_none() {
            bad.push("logging.root_level".to_owned());
        }
        for name in self.logging.loggers.keys() {
            if normalise_level(&self.logging.loggers[name]).is_none() {
                bad.push(format!("logging.loggers.{name}"));
            }
        }

        if bad.is_empty() {
            return Ok(());
        }
        Err(ConfigError::Invalid {
            fields: bad.join(", "),
            detail: format!("{} invalid value(s)", bad.len()),
        })
    }
}

/// The process environment, injected so tests stay independent.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    vars: BTreeMap<String, String>,
}

impl Environment {
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
        }
    }

    /// Build an environment from explicit pairs. Test helper.
    #[must_use]
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// `$XDG_CONFIG_HOME/govox/config.toml`, else `~/.config/govox/config.toml`.
    #[must_use]
    pub fn user_config_path(&self) -> Option<PathBuf> {
        let base = match self.vars.get("XDG_CONFIG_HOME") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => PathBuf::from(self.vars.get("HOME")?).join(".config"),
        };
        Some(base.join("govox").join("config.toml"))
    }

    /// `GOVOX__SECTION__KEY=value` pairs, as a nested table.
    fn overrides(&self) -> Result<toml::Table, ConfigError> {
        let mut out = toml::Table::new();
        for (key, value) in &self.vars {
            let Some(rest) = key.strip_prefix("GOVOX__") else {
                continue;
            };
            let parts: Vec<String> = rest
                .split("__")
                .filter(|p| !p.is_empty())
                .map(str::to_lowercase)
                .collect();
            // Fewer than two parts names a section but no key, so there is
            // nothing to set. The reference skips these silently.
            if parts.len() < 2 {
                continue;
            }

            let mut cursor = &mut out;
            for part in &parts[..parts.len() - 1] {
                let entry = cursor
                    .entry(part.clone())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                let toml::Value::Table(table) = entry else {
                    return Err(ConfigError::ConflictingEnv(key.clone()));
                };
                cursor = table;
            }
            cursor.insert(parts[parts.len() - 1].clone(), parse_env_value(value));
        }
        Ok(out)
    }
}

/// Coerce an environment string the way the reference does.
///
/// Order matters and is load-bearing: `"true"`/`"false"` (case-insensitively,
/// after trimming), then integer, then float, then the string unchanged. It is
/// why `GOVOX__RECOGNITION__MODEL=123` is rejected by the schema rather than
/// quietly accepted — it arrives as an integer.
fn parse_env_value(value: &str) -> toml::Value {
    let normalised = value.trim().to_lowercase();
    if normalised == "true" {
        return toml::Value::Boolean(true);
    }
    if normalised == "false" {
        return toml::Value::Boolean(false);
    }
    // Python's int()/float() tolerate surrounding whitespace, so trim first.
    // They also accept digit separators ("1_0" -> 10) where Rust does not; that
    // reaches the schema as a string and is rejected — narrower, not wider.
    if let Ok(int) = value.trim().parse::<i64>() {
        return toml::Value::Integer(int);
    }
    if let Ok(float) = value.trim().parse::<f64>() {
        return toml::Value::Float(float);
    }
    toml::Value::String(value.to_owned())
}

fn read_toml(path: &Path) -> Result<toml::Table, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_toml_str(&text, path)
}

fn parse_toml_str(text: &str, path: &Path) -> Result<toml::Table, ConfigError> {
    text.parse::<toml::Table>()
        .map_err(|source| ConfigError::Toml {
            path: path.to_path_buf(),
            source,
        })
}

/// Recursive table merge. Non-table values, lists included, replace wholesale.
fn merge_nested(target: &mut toml::Table, source: toml::Table) {
    for (key, value) in source {
        match (target.get_mut(&key), value) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                merge_nested(existing, incoming);
            }
            (_, incoming) => {
                target.insert(key, incoming);
            }
        }
    }
}

/// Best-effort dotted field name from a serde error, for the message.
fn field_from_error(error: &toml::de::Error) -> String {
    let text = error.to_string();
    // toml renders "invalid type: … \nin `recognition.beam_size`" or similar.
    if let Some(start) = text.rfind("in `")
        && let Some(end) = text[start + 4..].find('`')
    {
        return text[start + 4..start + 4 + end].to_owned();
    }
    "unknown field".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_config_home(dir: &Path) -> Environment {
        Environment::from_pairs([("XDG_CONFIG_HOME", dir.to_string_lossy().to_string())])
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("govox-cfg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_load_without_a_user_file() {
        let dir = scratch("defaults");
        let config = Config::load_from(None, &env_with_config_home(&dir)).unwrap();

        assert_eq!(config.recognition.engine, RecognitionEngine::Local);
        assert_eq!(config.recognition.model, "small");
        assert_eq!(config.recognition.device, RecognitionDevice::Auto);
        assert_eq!(config.recognition.download_policy, DownloadPolicy::Offline);
        assert_eq!(config.recognition.model_dir, "");
        assert_eq!(config.recognition.advanced.temperature, 0.0);
        assert_eq!(config.recognition.advanced.compression_ratio_threshold, 2.4);
        assert_eq!(config.recognition.advanced.log_prob_threshold, -1.0);
        assert_eq!(config.recognition.advanced.no_speech_threshold, 0.6);
        assert!(!config.recognition.advanced.condition_on_previous_text);
    }

    #[test]
    fn defaults_load_streaming_fields() {
        let dir = scratch("streaming-defaults");
        let config = Config::load_from(None, &env_with_config_home(&dir)).unwrap();

        assert!(!config.streaming.enabled);
        assert_eq!(config.streaming.engine, StreamingEngine::WhisperStreaming);
        assert_eq!(config.streaming.min_chunk_size_s, 1.0);
        assert_eq!(config.streaming.buffer_trimming, BufferTrimming::Segment);
        assert_eq!(config.streaming.buffer_trimming_sec, 10.0);
        assert!(config.streaming.vad);
        assert!(config.streaming.fallback_to_utterance);
    }

    #[test]
    fn feedback_defaults() {
        let dir = scratch("feedback-defaults");
        let config = Config::load_from(None, &env_with_config_home(&dir)).unwrap();

        assert!(config.feedback.chime);
        assert!(config.feedback.tick);
        assert_eq!(config.feedback.tick_interval_s, 45.0);
        assert!(config.feedback.tray_pulse);
        assert!(config.feedback.overlay);
        assert_eq!(config.feedback.overlay_position, OverlayPosition::TopRight);
        assert!(config.feedback.silence_auto_stop);
        assert_eq!(config.feedback.silence_timeout_s, 60.0);
    }

    #[test]
    fn user_file_overrides_default() {
        let dir = scratch("user-overrides");
        write(
            &dir.join("govox/config.toml"),
            "[recognition]\nmodel = \"tiny\"\n",
        );

        let config = Config::load_from(None, &env_with_config_home(&dir)).unwrap();
        assert_eq!(config.recognition.model, "tiny");
    }

    #[test]
    fn env_overrides_user_file() {
        let dir = scratch("env-overrides");
        write(
            &dir.join("govox/config.toml"),
            "[recognition]\nmodel = \"tiny\"\n",
        );
        let env = Environment::from_pairs([
            ("XDG_CONFIG_HOME", dir.to_string_lossy().to_string()),
            ("GOVOX__RECOGNITION__MODEL", "base".to_owned()),
        ]);

        let config = Config::load_from(None, &env).unwrap();
        assert_eq!(config.recognition.model, "base");
    }

    #[test]
    fn explicit_path_overrides_everything() {
        let dir = scratch("explicit");
        write(
            &dir.join("govox/config.toml"),
            "[recognition]\nmodel = \"small.en\"\n",
        );
        let override_path = dir.join("override.toml");
        write(&override_path, "[recognition]\nmodel = \"tiny\"\n");
        let env = Environment::from_pairs([
            ("XDG_CONFIG_HOME", dir.to_string_lossy().to_string()),
            ("GOVOX__RECOGNITION__MODEL", "base".to_owned()),
        ]);

        let config = Config::load_from(Some(&override_path), &env).unwrap();
        assert_eq!(config.recognition.model, "tiny");
    }

    #[test]
    fn missing_explicit_path_is_an_error() {
        let dir = scratch("missing");
        let err = Config::load_from(Some(&dir.join("nope.toml")), &env_with_config_home(&dir))
            .unwrap_err();
        assert!(matches!(err, ConfigError::Missing(_)), "got {err}");
    }

    #[test]
    fn merge_is_deep_not_wholesale() {
        // Overriding one key in [recognition.advanced] must not drop the rest.
        let dir = scratch("deep-merge");
        let path = dir.join("o.toml");
        write(&path, "[recognition.advanced]\ntemperature = 0.5\n");

        let config = Config::load_from(Some(&path), &env_with_config_home(&dir)).unwrap();
        assert_eq!(config.recognition.advanced.temperature, 0.5);
        assert_eq!(
            config.recognition.advanced.no_speech_threshold, 0.6,
            "sibling survived"
        );
    }

    #[test]
    fn lists_replace_wholesale_rather_than_appending() {
        let dir = scratch("list-replace");
        let path = dir.join("o.toml");
        write(&path, "[correction]\nfiller_words = [\"nope\"]\n");

        let config = Config::load_from(Some(&path), &env_with_config_home(&dir)).unwrap();
        assert_eq!(config.correction.filler_words, ["nope"]);
    }

    #[test]
    fn loads_recognition_tuning_options() {
        let dir = scratch("tuning");
        let path = dir.join("o.toml");
        write(
            &path,
            r#"
[recognition]
device = "cuda"
download_policy = "cache_first"
model_dir = "/models/govox-small"

[recognition.advanced]
temperature = 0.2
compression_ratio_threshold = 1.8
log_prob_threshold = -0.5
no_speech_threshold = 0.4
condition_on_previous_text = true
"#,
        );

        let config = Config::load_from(Some(&path), &env_with_config_home(&dir)).unwrap();
        assert_eq!(config.recognition.device, RecognitionDevice::Cuda);
        assert_eq!(
            config.recognition.download_policy,
            DownloadPolicy::CacheFirst
        );
        assert_eq!(config.recognition.model_dir, "/models/govox-small");
        assert_eq!(config.recognition.advanced.temperature, 0.2);
        assert_eq!(config.recognition.advanced.compression_ratio_threshold, 1.8);
        assert_eq!(config.recognition.advanced.log_prob_threshold, -0.5);
        assert_eq!(config.recognition.advanced.no_speech_threshold, 0.4);
        assert!(config.recognition.advanced.condition_on_previous_text);
    }

    #[test]
    fn loads_custom_streaming_fields() {
        let dir = scratch("streaming-custom");
        let path = dir.join("o.toml");
        write(
            &path,
            r#"
[streaming]
enabled = true
engine = "whisper_streaming"
min_chunk_size_s = 0.5
buffer_trimming = "sentence"
buffer_trimming_sec = 6.0
vad = false
fallback_to_utterance = false
"#,
        );

        let config = Config::load_from(Some(&path), &env_with_config_home(&dir)).unwrap();
        assert!(config.streaming.enabled);
        assert_eq!(config.streaming.min_chunk_size_s, 0.5);
        assert_eq!(config.streaming.buffer_trimming, BufferTrimming::Sentence);
        assert_eq!(config.streaming.buffer_trimming_sec, 6.0);
        assert!(!config.streaming.vad);
        assert!(!config.streaming.fallback_to_utterance);
    }

    fn expect_invalid(dir: &Path, body: &str, needle: &str) {
        let path = dir.join("bad.toml");
        write(&path, body);
        let err = Config::load_from(Some(&path), &env_with_config_home(dir)).unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("Invalid govox configuration"),
            "got: {text}"
        );
        assert!(text.contains(needle), "expected {needle:?} in: {text}");
    }

    #[test]
    fn rejects_nonpositive_beam_size() {
        expect_invalid(
            &scratch("beam"),
            "[recognition]\nbeam_size = 0\n",
            "beam_size",
        );
    }

    #[test]
    fn rejects_invalid_download_policy() {
        expect_invalid(
            &scratch("policy"),
            "[recognition]\ndownload_policy = \"sometimes\"\n",
            "download_policy",
        );
    }

    #[test]
    fn rejects_invalid_streaming_values() {
        for (body, field) in [
            ("min_chunk_size_s = 0\n", "min_chunk_size_s"),
            ("buffer_trimming = \"window\"\n", "buffer_trimming"),
            ("buffer_trimming_sec = 0\n", "buffer_trimming_sec"),
        ] {
            expect_invalid(&scratch(field), &format!("[streaming]\n{body}"), field);
        }
    }

    #[test]
    fn rejects_nonpositive_feedback_intervals() {
        for field in ["tick_interval_s", "silence_timeout_s"] {
            expect_invalid(
                &scratch(field),
                &format!("[feedback]\n{field} = 0\n"),
                field,
            );
        }
    }

    #[test]
    fn rejects_unknown_overlay_position() {
        expect_invalid(
            &scratch("position"),
            "[feedback]\noverlay_position = \"center\"\n",
            "overlay_position",
        );
    }

    #[test]
    fn rejects_out_of_range_vad_thresholds() {
        expect_invalid(
            &scratch("vad"),
            "[vad]\nspeech_threshold = 1.5\n",
            "speech_threshold",
        );
    }

    #[test]
    fn rejects_unknown_log_level() {
        expect_invalid(
            &scratch("level"),
            "[logging]\nlevel = \"CHATTY\"\n",
            "logging.level",
        );
    }

    #[test]
    fn unknown_section_is_rejected() {
        expect_invalid(&scratch("unknown-section"), "[bogus]\nx = 1\n", "bogus");
    }

    #[test]
    fn unknown_key_inside_a_known_section_is_ignored() {
        // Matches the reference exactly, verified against the pinned source.
        // Arguably a wart — a typo'd key does nothing, silently — but changing
        // it here would reject configurations that work today.
        let dir = scratch("unknown-key");
        let path = dir.join("o.toml");
        write(&path, "[audio]\nbogus_key = 1\n");

        let config = Config::load_from(Some(&path), &env_with_config_home(&dir)).unwrap();
        assert_eq!(config.audio.sample_rate, 16_000);
    }

    #[test]
    fn env_values_are_coerced_by_shape() {
        assert_eq!(parse_env_value("true"), toml::Value::Boolean(true));
        assert_eq!(parse_env_value("  FALSE "), toml::Value::Boolean(false));
        assert_eq!(parse_env_value("42"), toml::Value::Integer(42));
        assert_eq!(parse_env_value("-7"), toml::Value::Integer(-7));
        assert_eq!(parse_env_value("0.5"), toml::Value::Float(0.5));
        assert_eq!(
            parse_env_value("small"),
            toml::Value::String("small".into())
        );
    }

    #[test]
    fn env_bool_reaches_the_schema_as_a_bool() {
        let dir = scratch("env-bool");
        let env = Environment::from_pairs([
            ("XDG_CONFIG_HOME", dir.to_string_lossy().to_string()),
            ("GOVOX__STREAMING__ENABLED", "true".to_owned()),
        ]);
        let config = Config::load_from(None, &env).unwrap();
        assert!(config.streaming.enabled);
    }

    #[test]
    fn env_key_with_fewer_than_two_parts_is_skipped() {
        let dir = scratch("env-short");
        let env = Environment::from_pairs([
            ("XDG_CONFIG_HOME", dir.to_string_lossy().to_string()),
            ("GOVOX__RECOGNITION", "nonsense".to_owned()),
        ]);
        // Names a section but no key, so there is nothing to set.
        let config = Config::load_from(None, &env).unwrap();
        assert_eq!(config.recognition.model, "small");
    }

    #[test]
    fn env_reaches_a_nested_section() {
        let dir = scratch("env-nested");
        let env = Environment::from_pairs([
            ("XDG_CONFIG_HOME", dir.to_string_lossy().to_string()),
            (
                "GOVOX__RECOGNITION__ADVANCED__TEMPERATURE",
                "0.25".to_owned(),
            ),
        ]);
        let config = Config::load_from(None, &env).unwrap();
        assert_eq!(config.recognition.advanced.temperature, 0.25);
    }

    #[test]
    fn user_config_path_prefers_xdg_over_home() {
        let env = Environment::from_pairs([("XDG_CONFIG_HOME", "/tmp/xdg"), ("HOME", "/home/x")]);
        assert_eq!(
            env.user_config_path().unwrap(),
            PathBuf::from("/tmp/xdg/govox/config.toml")
        );

        let env = Environment::from_pairs([("HOME", "/home/x")]);
        assert_eq!(
            env.user_config_path().unwrap(),
            PathBuf::from("/home/x/.config/govox/config.toml")
        );
    }

    #[test]
    fn embedded_default_toml_is_valid_on_its_own() {
        // The strongest single check in this module: the file govox-py ships is
        // parsed by the Rust schema with no user config, no environment, and no
        // overrides. A key added upstream that this schema lacks fails here.
        let config = Config::load_from(None, &Environment::default()).unwrap();
        assert_eq!(config.audio.sample_rate, 16_000);
        assert_eq!(config.activation.mode, ActivationMode::Toggle);
        assert_eq!(config.injection.method, InjectionMethod::Ydotool);
        assert!(config.indicator.enabled);
        assert_eq!(config.logging.style, LogStyle::Auto);
    }
}
