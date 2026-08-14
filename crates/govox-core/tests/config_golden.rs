//! Golden test for the config schema: every key and every default.
//!
//! `corpus/config-defaults.json` is the full default configuration, resolved
//! with an empty `XDG_CONFIG_HOME` and no `GOVOX__` variables so it is a
//! property of the code rather than of whoever ran it.
//!
//! It catches what hand-written assertions cannot: a key silently dropped, or a
//! default quietly changed. You only assert what you remember to assert, and a
//! schema this size (13 sections) outruns memory. A diff here means a key or a
//! default moved — intended or not.
//!
//! Regenerate deliberately, and read the diff:
//!
//! ```console
//! $ GOVOX_BLESS=1 cargo test -p govox-core --test config_golden -- --ignored bless
//! ```
//!
//! History: these values were originally captured from an earlier Python
//! implementation, to prove the schema had been ported without loss. That is
//! provenance now — the file describes govox's own defaults.

use govox_core::config::{Config, Environment};

const GOLDEN: &str = include_str!("../../../corpus/config-defaults.json");

/// Keys present in the schema but deliberately absent from the snapshot.
///
/// Each is an addition made after the snapshot was first recorded, with a
/// matching row in `docs/parity.md`. The list exists so an addition has to be
/// *declared*: tolerating unknown extra keys in general would hide the case
/// this test is really for, which is a key drifting by accident.
///
/// - `recognition.gpu_device` — which GPU to run on. Has no equivalent in the
///   implementation this snapshot came from, which always took whatever the
///   driver enumerated first. On a laptop with switchable graphics that is the
///   integrated GPU, measured here at 2.4x slower.
const RUST_ONLY_KEYS: &[&str] = &["recognition.gpu_device"];

/// Compare two JSON values, collecting every difference as a dotted path.
///
/// Reports *all* differences rather than the first, because a schema drift
/// usually shows up as several keys at once and fixing them one test-run at a
/// time is miserable.
fn diff(a: &serde_json::Value, b: &serde_json::Value, path: &str, out: &mut Vec<String>) {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(left), Value::Object(right)) => {
            let mut keys: Vec<&String> = left.keys().chain(right.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (left.get(key), right.get(key)) {
                    (Some(l), Some(r)) => diff(l, r, &child, out),
                    (Some(_), None) => out.push(format!("{child}: only in rust")),
                    (None, Some(_)) => out.push(format!("{child}: MISSING from rust")),
                    (None, None) => unreachable!(),
                }
            }
        }
        (Value::Number(left), Value::Number(right)) => {
            // 45 and 45.0 are the same configured value; pydantic renders a
            // float field holding a whole number as 45.0, serde as 45.0 too,
            // but an int field differs in type. Compare numerically.
            let (l, r) = (left.as_f64(), right.as_f64());
            if l != r {
                out.push(format!("{path}: actual={left} golden={right}"));
            }
        }
        (left, right) => {
            if left != right {
                out.push(format!("{path}: actual={left} golden={right}"));
            }
        }
    }
}

/// Guard the guard.
///
/// A comparison test that silently compares nothing still passes, and this one
/// is the only thing standing between a mistranscribed default and a silent
/// behaviour change. So: plant known drift and require it to be caught.
#[test]
fn the_diff_detects_drift() {
    let base: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();

    // A changed value.
    let mut changed = base.clone();
    changed["recognition"]["model"] = serde_json::Value::String("tiny".into());
    let mut found = Vec::new();
    diff(&changed, &base, "", &mut found);
    assert_eq!(found.len(), 1, "changed value: {found:?}");
    assert!(found[0].starts_with("recognition.model:"), "{found:?}");

    // A key missing from the Rust side — the drift that matters most, because
    // it is what an upstream addition looks like.
    let mut missing = base.clone();
    missing["recognition"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    let mut found = Vec::new();
    diff(&missing, &base, "", &mut found);
    assert_eq!(found, ["recognition.model: MISSING from rust"], "{found:?}");

    // A key only on the Rust side.
    let mut extra = base.clone();
    extra["recognition"]
        .as_object_mut()
        .unwrap()
        .insert("invented".into(), serde_json::Value::Bool(true));
    let mut found = Vec::new();
    diff(&extra, &base, "", &mut found);
    assert_eq!(found, ["recognition.invented: only in rust"], "{found:?}");

    // And identity really is quiet.
    let mut found = Vec::new();
    diff(&base, &base, "", &mut found);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn defaults_match_the_golden_snapshot() {
    // Empty environment: no XDG_CONFIG_HOME, no HOME, so no user config layer
    // and no overrides — the same conditions the generator ran under.
    let config = Config::load_from(None, &Environment::default())
        .expect("the shipped default.toml must load on its own");

    let rust: serde_json::Value = serde_json::to_value(&config).expect("config serialises to JSON");
    let golden: serde_json::Value =
        serde_json::from_str(GOLDEN).expect("corpus/config-defaults.json is valid JSON");

    let mut differences = Vec::new();
    diff(&rust, &golden, "", &mut differences);

    // Declared additions are removed here rather than skipped inside `diff`,
    // so a key that stops being an addition — because it was folded into the
    // snapshot, or renamed — resurfaces as a failure instead of staying
    // invisible.
    differences.retain(|d| {
        !RUST_ONLY_KEYS
            .iter()
            .any(|key| d == &format!("{key}: only in rust"))
    });

    assert!(
        differences.is_empty(),
        "the config schema moved in {} place(s):\n  {}\n\n\
         If that was intended, re-record with:\n  \
         GOVOX_BLESS=1 cargo test -p govox-core --test config_golden -- --ignored bless",
        differences.len(),
        differences.join("\n  ")
    );
}

#[test]
fn every_declared_addition_is_really_an_addition() {
    // Guard the allowlist. An entry that no longer corresponds to a real
    // Rust-only key is dead weight that would mask the next genuine drift.
    let config = Config::load_from(None, &Environment::default()).expect("defaults load");
    let rust: serde_json::Value = serde_json::to_value(&config).expect("serialises");
    let golden: serde_json::Value = serde_json::from_str(GOLDEN).expect("valid JSON");

    let mut differences = Vec::new();
    diff(&rust, &golden, "", &mut differences);

    for key in RUST_ONLY_KEYS {
        assert!(
            differences.contains(&format!("{key}: only in rust")),
            "{key} is on the allowlist but is not a Rust-only key; remove it"
        );
    }
}

/// Re-record the default-configuration snapshot from the current schema.
///
/// Ignored, and additionally gated on `GOVOX_BLESS=1`, so a checked-in fixture
/// cannot be rewritten as a side effect of `cargo test -- --ignored`.
///
/// `RUST_ONLY_KEYS` is deliberately not touched here. An addition still has to
/// be declared by hand, which is the whole point of that list.
#[test]
#[ignore = "rewrites corpus/config-defaults.json; run deliberately with GOVOX_BLESS=1"]
fn bless_the_config_golden() {
    assert!(
        std::env::var("GOVOX_BLESS").is_ok(),
        "refusing to rewrite the snapshot without GOVOX_BLESS=1"
    );

    let config = Config::load_from(None, &Environment::default())
        .expect("the shipped default.toml must load on its own");
    let value: serde_json::Value = serde_json::to_value(&config).expect("config serialises");

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../corpus/config-defaults.json"
    );
    let mut text = serde_json::to_string_pretty(&value).expect("snapshot serialises");
    text.push('\n');
    std::fs::write(path, text).expect("snapshot is writable");
    println!("wrote {path}");
}
