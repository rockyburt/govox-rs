//! Spoken punctuation: "hello comma world period" → "Hello, world."
//!
//! Ported from `correction/punctuation.py`. Not a command in the grammar sense:
//! editing commands match a whole utterance, punctuation words appear inline,
//! so this is a token-level rewrite.
//!
//! Whisper already punctuates from prosody, so this layer only handles the case
//! where the user says the punctuation *word* and expects the mark — which is
//! also why both the mark before and the mark after the spoken word are
//! absorbed. Otherwise "hello period" ("Hello. Period.") would render "Hello..".
//!
//! The pattern needs a **negative lookahead**, which the `regex` crate cannot
//! do, so this module uses `fancy-regex`. Backtracking is irrelevant here: the
//! input is one utterance.

use std::sync::LazyLock;

use fancy_regex::{Captures, Regex};

/// Which side of the mark absorbs the surrounding whitespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// Closes up against the *preceding* word: "hello comma" → "hello,".
    Left,
    /// Belongs to the word *after* it: "open quote hello" → `"hello`.
    Right,
    /// Closes up on both sides: "well hyphen known" → "well-known".
    Tight,
    /// A line break. Like `Tight` it takes no space, but unlike every other
    /// mark it *keeps* preceding auto-punctuation: "Hello. New line world" is
    /// "Hello.\nworld". The sentence really did end; the break is not replacing
    /// its full stop.
    Break,
    /// Stands alone with a space on each side: "Tom ampersand Jerry" →
    /// "Tom & Jerry".
    ///
    /// The symbols people say by name divide into two shapes, and getting them
    /// the same way round is wrong for one of them. A symbol inside an
    /// identifier closes up (`Tight`) — "rocky at sign gmail" is one token. A
    /// symbol standing in for a word does not: "&" read aloud as "and" is a
    /// word in the sentence and is spaced like one.
    Spaced,
}

/// Spoken phrase → (mark, attachment).
///
/// **Order is significant and must match the reference's dict order**: the
/// pattern's alternation is built in this order and alternation is ordered.
/// Prefix-overlapping phrases ("open paren" / "open parenthesis") are safe
/// because of the trailing `\b`, but the two break phrases rely on ordering.
pub const SPOKEN_PUNCTUATION: &[(&str, &str, Attach)] = &[
    ("exclamation mark", "!", Attach::Left),
    ("exclamation point", "!", Attach::Left),
    ("question mark", "?", Attach::Left),
    ("full stop", ".", Attach::Left),
    ("semicolon", ";", Attach::Left),
    ("ellipsis", "…", Attach::Left),
    ("period", ".", Attach::Left),
    ("comma", ",", Attach::Left),
    ("colon", ":", Attach::Left),
    ("hyphen", "-", Attach::Tight),
    // A synonym for "hyphen", not an em dash. Nobody dictating a command-line
    // flag or a hyphenated word says "hyphen hyphen" and means `——`, and the
    // two words are used interchangeably in speech — so the mark people almost
    // always want when they say "dash" is the ASCII one. This deliberately
    // gives up dictating `—`, which had no other spelling; a word that produces
    // the wrong mark nine times out of ten is worse than one that is missing.
    ("dash", "-", Attach::Tight),
    // Deliberately no bare "quote": it is an everyday verb and noun and the
    // determiner guard cannot see far enough back to tell them apart, so the
    // opener must be "open quote". "unquote" is safe bare — not a word alone.
    ("open quote", "\"", Attach::Right),
    ("close quote", "\"", Attach::Left),
    ("unquote", "\"", Attach::Left),
    ("open parenthesis", "(", Attach::Right),
    ("open paren", "(", Attach::Right),
    ("close parenthesis", ")", Attach::Left),
    ("close paren", ")", Attach::Left),
    ("open bracket", "[", Attach::Right),
    ("close bracket", "]", Attach::Left),
    ("open brace", "{", Attach::Right),
    ("close brace", "}", Attach::Left),
    // Symbol names, so an address or a path can be dictated at all.
    //
    // Almost every one is suffixed "sign" or is a word with no other everyday
    // use, because the bare word is usually ordinary English: "at", "plus",
    // "equals", "less than", "star" and "pound" are all sentences waiting to
    // happen. None of those is accepted bare.
    //
    // "dot" is the deliberate exception, and it earns it: "dot com" is simply
    // how an address is said, and without it the address case this section
    // exists for still does not work. Its false-positive cost ("using dot
    // product") is real but strictly smaller than that of "period", which has
    // shipped since the beginning — "the Victorian period" is a likelier
    // sentence than any bare "dot". The determiner guard covers "a dot" and
    // "the dot", and `\b` means the plural "dots" never matches.
    ("at sign", "@", Attach::Tight),
    ("dot", ".", Attach::Tight),
    ("dollar sign", "$", Attach::Right),
    ("percent sign", "%", Attach::Left),
    ("hashtag", "#", Attach::Right),
    ("number sign", "#", Attach::Spaced),
    ("pound sign", "#", Attach::Spaced),
    ("hash sign", "#", Attach::Spaced),
    ("plus sign", "+", Attach::Spaced),
    ("equals sign", "=", Attach::Spaced),
    ("less than sign", "<", Attach::Spaced),
    ("greater than sign", ">", Attach::Spaced),
    ("vertical bar", "|", Attach::Spaced),
    ("ampersand", "&", Attach::Spaced),
    ("asterisk", "*", Attach::Spaced),
    ("tilde", "~", Attach::Spaced),
    ("underscore", "_", Attach::Tight),
    // "no space" is a mark whose mark is nothing. `Tight` already means "close
    // up on both sides", so joining two words is what an empty tight mark does
    // — no new stage, no new state, and it inherits the determiner suppression
    // that stops "the no space rule" from being eaten.
    ("no space", "", Attach::Tight),
    // "forward slash" must precede "slash": alternation is ordered, and the
    // bare phrase would otherwise match first and leave "forward" behind as a
    // prefix word. "backslash" needs no such care — `\b` cannot split a word.
    ("forward slash", "/", Attach::Tight),
    ("backslash", "\\", Attach::Tight),
    ("slash", "/", Attach::Tight),
    ("new paragraph", "\n\n", Attach::Break),
    ("new line", "\n", Attach::Break),
];

/// When one of these immediately precedes the word, it is a noun ("add a comma
/// here"), not a spoken mark.
///
/// Demonstratives are deliberately absent: "what is this question mark" means
/// "What is this?", so guarding on "this" would suppress the common case.
pub const DETERMINERS: &[&str] = &["a", "an", "the", "my", "your", "its", "another"];

/// A newline ends a sentence for casing purposes as surely as a full stop.
pub const TERMINATORS: &[char] = &['.', '!', '?', '…', '\n'];

const MARKS: &str = ".!?…,;:";

pub(crate) fn is_determiner(word: &str) -> bool {
    let lowered = word.to_lowercase();
    DETERMINERS.contains(&lowered.as_str())
}

fn lookup(phrase: &str) -> Option<(&'static str, Attach)> {
    let lowered = phrase.to_lowercase();
    SPOKEN_PUNCTUATION
        .iter()
        .find(|(name, _, _)| *name == lowered)
        .map(|(_, mark, attach)| (*mark, *attach))
}

static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // "open"/"close" must not be swallowed as an ordinary prefix word when they
    // open a paired-mark phrase, or "close quote" parses as prefix="close",
    // phrase="quote" — dropping the prefix and losing the no-space attachment.
    // Derived from the table, so a future "open …" phrase is guarded too.
    let paired: Vec<String> = SPOKEN_PUNCTUATION
        .iter()
        .filter(|(p, _, _)| p.starts_with("open ") || p.starts_with("close "))
        .map(|(p, _, _)| fancy_regex::escape(p.split_once(' ').unwrap().1).into_owned())
        .collect();
    // frozenset in the reference, so duplicates collapse; do the same.
    let mut unique: Vec<String> = Vec::new();
    for word in paired {
        if !unique.contains(&word) {
            unique.push(word);
        }
    }
    let guard = format!(r"(?:open|close)\s+(?:{})\b", unique.join("|"));

    let phrases: Vec<String> = SPOKEN_PUNCTUATION
        .iter()
        .map(|(p, _, _)| fancy_regex::escape(p).into_owned())
        .collect();

    let marks = fancy_regex::escape(MARKS);
    let source = format!(
        r"(?i)(?:(?P<lead>[{marks}])\s*|(?!{guard})(?P<prefix>\w+)\s+)?\b(?P<phrase>{})\b(?P<tail>\s*[{marks}])?(?P<suffix>\s+)?",
        phrases.join("|"),
    );
    Regex::new(&source).expect("punctuation pattern compiles")
});

/// Replace spoken punctuation words with their marks.
#[must_use]
pub fn apply_spoken_punctuation(text: &str) -> String {
    replace_all(&PATTERN, text, |caps| {
        let prefix = caps.name("prefix").map(|m| m.as_str());
        let phrase = caps
            .name("phrase")
            .expect("phrase group always matches")
            .as_str();
        let suffix = caps.name("suffix").map_or("", |m| m.as_str());
        let lead_mark = caps.name("lead").map(|m| m.as_str());

        let (mark, attach) = lookup(phrase).expect("matched phrase is in the table");

        if prefix.is_some_and(is_determiner) {
            // A noun, not a spoken mark. Return the match untouched.
            return caps.get(0).expect("whole match").as_str().to_owned();
        }

        // `lead` and `tail` are Whisper's auto-punctuation around the spoken
        // word; dropping them is what stops ".." and ". ." forming.
        let (lead, trail) = match attach {
            Attach::Right => {
                // The mark belongs to the word after it: keep the space that
                // separated the prefix, drop the trailing one. When
                // auto-punctuation was absorbed instead ("he said. Open quote
                // hello") the mark goes but its space stays, or the opener
                // glues to the previous word.
                let lead = if let Some(prefix) = prefix {
                    format!("{prefix} ")
                } else if lead_mark.is_some() {
                    " ".to_owned()
                } else {
                    String::new()
                };
                (lead, "")
            }
            Attach::Break => {
                // Keep whatever preceded, auto-punctuation included: the
                // sentence before a break genuinely ended.
                let lead = prefix.map_or_else(|| lead_mark.unwrap_or("").to_owned(), str::to_owned);
                (lead, "")
            }
            Attach::Spaced => {
                // A space on each side. The pattern consumed the prefix's own
                // space, so it has to be re-emitted, exactly as `Right` does.
                let lead = if let Some(prefix) = prefix {
                    format!("{prefix} ")
                } else if lead_mark.is_some() {
                    " ".to_owned()
                } else {
                    String::new()
                };
                // At the end of the utterance there is no trailing whitespace to
                // keep; emitting one anyway is harmless, since `normalize_spacing`
                // strips it, and saves a second branch here.
                (lead, if suffix.is_empty() { " " } else { suffix })
            }
            Attach::Tight => (prefix.unwrap_or("").to_owned(), ""),
            Attach::Left => (prefix.unwrap_or("").to_owned(), suffix),
        };
        format!("{lead}{mark}{trail}")
    })
}

/// `Regex::replace_all` with a closure, for `fancy-regex`.
///
/// fancy-regex has no closure-taking `replace_all`, so this walks the matches.
pub(crate) fn replace_all<F>(pattern: &Regex, text: &str, mut render: F) -> String
where
    F: FnMut(&Captures<'_, str>) -> String,
{
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in pattern.captures_iter(text).flatten() {
        let whole = caps.get(0).expect("whole match");
        // Zero-width matches would loop forever and cannot rewrite anything.
        if whole.start() == whole.end() {
            continue;
        }
        out.push_str(&text[last..whole.start()]);
        out.push_str(&render(&caps));
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Capitalize the first letter of each sentence after `.`, `!`, `?`, `…`, `\n`.
///
/// Without this, "hello period world period" renders "Hello. world." — the
/// spoken full stop creates a boundary nothing else capitalizes.
///
/// **A terminator only starts a sentence when something separates it from the
/// next word.** By the time this runs, the full stop that `("dot", ".")`
/// produced is the same character as the one that ends a sentence, and no
/// amount of looking at it will say which it was — so the separator is what
/// distinguishes them, exactly as it does in writing. Without that condition
/// "main dot rs" became `main.Rs`, "JSON dot parse" became `JSON.Parse`, and
/// "rocky at sign gmail dot com" became `gmail.Com`: every dictated filename,
/// method call and domain acquired a capital in the middle.
///
/// The separator may be the terminator itself, which is what keeps `\n`
/// working — a newline both ends the sentence and separates it from the next.
#[must_use]
pub fn capitalize_after_terminators(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // A terminator has been seen and no word has followed it yet.
    let mut pending = false;
    // ...and something separated it from whatever comes next.
    let mut separated = false;
    for char in text.chars() {
        if pending && char.is_alphabetic() {
            if separated {
                // to_uppercase can yield several chars (ß → SS), matching Python.
                out.extend(char.to_uppercase());
            } else {
                out.push(char);
            }
            pending = false;
            separated = false;
            continue;
        }
        if TERMINATORS.contains(&char) {
            pending = true;
            // `\n` is both the terminator and the separator. A `.` is not.
            separated = char.is_whitespace();
        } else if pending && char.is_whitespace() {
            separated = true;
        }
        out.push(char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::apply_spoken_punctuation as punct;

    #[test]
    fn an_address_can_be_dictated() {
        assert_eq!(punct("rocky at sign gmail dot com"), "rocky@gmail.com");
    }

    #[test]
    fn a_path_can_be_dictated() {
        assert_eq!(punct("usr forward slash local slash bin"), "usr/local/bin");
    }

    /// `\b` cannot split a word, so the bare "slash" entry must not reach
    /// inside "backslash" however the alternation is ordered.
    #[test]
    fn backslash_survives_the_bare_slash_entry() {
        assert_eq!(punct("c backslash temp"), "c\\temp");
    }

    #[test]
    fn prose_symbols_keep_a_space_on_each_side() {
        assert_eq!(punct("Tom ampersand Jerry"), "Tom & Jerry");
        assert_eq!(punct("one plus sign two"), "one + two");
    }

    #[test]
    fn a_hashtag_leads_the_word_it_labels() {
        assert_eq!(punct("hashtag rust"), "#rust");
    }

    #[test]
    fn a_percent_sign_closes_up_behind_its_number() {
        assert_eq!(punct("fifty percent sign"), "fifty%");
    }

    #[test]
    fn braces_pair_like_the_other_brackets() {
        assert_eq!(punct("open brace x close brace"), "{x}");
    }

    #[test]
    fn a_determiner_still_blocks_a_symbol_name() {
        assert_eq!(punct("add a slash here"), "add a slash here");
        assert_eq!(punct("the dot product"), "the dot product");
    }

    /// The plural is a different word, and `\b` keeps it that way. This is the
    /// guard that makes the bare "dot" entry defensible.
    #[test]
    fn the_plural_of_dot_is_not_a_mark() {
        assert_eq!(punct("connect the dots"), "connect the dots");
    }

    #[test]
    fn no_space_joins_the_words_on_either_side() {
        // A mark whose mark is nothing: `Tight` supplies the behaviour.
        assert_eq!(punct("hello no space world"), "helloworld");
        assert_eq!(punct("camel no space case no space name"), "camelcasename");
    }

    #[test]
    fn no_space_after_a_determiner_is_prose() {
        assert_eq!(punct("the no space rule"), "the no space rule");
    }

    // --- capitalisation after a terminator ---------------------------------

    use super::capitalize_after_terminators as caps;

    #[test]
    fn a_sentence_boundary_still_capitalises() {
        // The reason the function exists: a spoken full stop creates a boundary
        // nothing else would capitalise.
        assert_eq!(caps("hello. world."), "hello. World.");
        assert_eq!(caps("is it? yes!"), "is it? Yes!");
        assert_eq!(caps("done… next"), "done… Next");
    }

    #[test]
    fn a_newline_is_its_own_separator() {
        // `\n` both ends the sentence and separates it from what follows, so it
        // must capitalise with no space in between.
        assert_eq!(caps("first line\nsecond line"), "first line\nSecond line");
    }

    #[test]
    fn a_dotted_identifier_keeps_its_lower_case() {
        // The defect this fixes. By the time this runs, the full stop from a
        // spoken "dot" is the same character as one ending a sentence — the
        // separator is what tells them apart, exactly as in writing.
        assert_eq!(caps("open main.rs"), "open main.rs");
        assert_eq!(caps("call JSON.parse"), "call JSON.parse");
        assert_eq!(caps("rocky@gmail.com"), "rocky@gmail.com");
        assert_eq!(caps("see rentals.ca today"), "see rentals.ca today");
    }

    #[test]
    fn an_initialism_is_left_alone() {
        assert_eq!(caps("the U.S.A. today"), "the U.S.A. Today");
    }

    #[test]
    fn several_spaces_still_separate() {
        assert_eq!(caps("done.  next"), "done.  Next");
    }

    #[test]
    fn a_quote_after_the_space_does_not_lose_the_capital() {
        // Non-alphabetic characters between the separator and the word must not
        // disarm it, or an opening quote would swallow the capital.
        assert_eq!(caps("he said. \"yes\""), "he said. \"Yes\"");
    }

    #[test]
    fn a_dash_is_a_hyphen() {
        // Synonyms, because they are used interchangeably in speech and the
        // ASCII mark is the one people mean when dictating a flag or a
        // hyphenated word.
        assert_eq!(punct("all dash targets"), "all-targets");
        assert_eq!(punct("all hyphen targets"), "all-targets");
        assert_eq!(
            punct("cargo clippy dash dash all dash targets"),
            "cargo clippy--all-targets"
        );
    }
}
