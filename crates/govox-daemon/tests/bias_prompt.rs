//! The bias prompt, planned with the real tokenizer against this machine's
//! configuration — the check that would have caught the prompt overrun.
//!
//! `#[ignore]`d because it needs a model file on disk, like the recognition
//! tests. It reads the machine's own config and dictionary, as the accuracy
//! eval does, because the failure only exists at machine scale: a hand-sized
//! test list never comes near whisper.cpp's limit.
//!
//! ```text
//! cargo test -p govox-daemon --test bias_prompt -- --ignored --nocapture
//! ```

use govox_asr::whisper::WhisperRecognizer;
use govox_asr::{MAX_PROMPT_TOKENS, bias_prompt};
use govox_core::config::Config;
use govox_core::discovery::word_cost;

#[tokio::test]
#[ignore = "needs a model file on disk"]
async fn the_planned_prompt_fits_and_keeps_every_hand_written_term() {
    let config = Config::load(None).expect("the machine's configuration loads");
    let hand = govox_daemon::load_dictionary(&config).expect("the dictionary loads");

    let recognizer =
        WhisperRecognizer::start(&config.recognition, &hand, 4).expect("the recogniser starts");
    let handle = recognizer.handle();
    handle.warm_up().await.expect("the model loads");
    let count = |text: &str| handle.count_tokens(text);
    let frame = count(govox_asr::PROMPT_FRAME).expect("countable");
    eprintln!(
        "framing sentence: {frame} tokens, so bias_prompt_token_budget can go to {}",
        MAX_PROMPT_TOKENS - frame
    );

    let loaded = govox_daemon::load_dictionary_with_discovery(&config, &count)
        .expect("the dictionary plans");
    let prompt = bias_prompt(
        &loaded.dictionary.bias_terms,
        config.recognition.bias_prompt_token_budget,
    );
    let tokens = count(&prompt).expect("the model is loaded, so it can count");

    // What the word-counted planner used to send, counted by the same model.
    let found = govox_discover::discover(
        loaded
            .dictionary
            .discover
            .as_ref()
            .expect("discovery is configured here"),
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .as_deref(),
        &[],
    );
    let old_plan = govox_core::discovery::plan_bias(
        &hand,
        &found.candidates,
        config.recognition.bias_prompt_token_budget,
        &word_cost,
    );
    let old_prompt = bias_prompt(&old_plan.terms, config.recognition.bias_prompt_token_budget);
    let old_tokens = count(&old_prompt).expect("countable");

    eprintln!(
        "word-counted plan: {} terms, {old_tokens} tokens (whisper.cpp reads {MAX_PROMPT_TOKENS})",
        old_plan.terms.len()
    );
    eprintln!(
        "token-counted plan: {} terms, {tokens} tokens, {} discovered terms dropped",
        loaded.dictionary.bias_terms.len(),
        loaded.plan.dropped.len()
    );

    assert!(
        tokens <= MAX_PROMPT_TOKENS,
        "the prompt is {tokens} tokens; whisper.cpp keeps only the last {MAX_PROMPT_TOKENS} \
         and drops the front, where the hand-written terms are"
    );
    for term in &hand.bias_terms {
        assert!(
            loaded.dictionary.bias_terms.contains(term),
            "hand-written term {term:?} was planned out of the prompt"
        );
        assert!(
            prompt.contains(term.as_str()),
            "{term:?} is not in the prompt"
        );
    }
}
