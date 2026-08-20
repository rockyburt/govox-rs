//! Spoken numbers → digits, with currency and percent attached. Off by default.
//!
//! Ported from `correction/numbers.py`. A hand-written tokenizer and scanner
//! rather than a regex, because the run of number words has to be folded
//! (additive for words, multiplicative for multipliers) before anything can be
//! decided about it.

use std::sync::LazyLock;

use fancy_regex::Regex;

use super::punctuation::replace_all;

/// Shared with the editing grammar, which imports from here rather than keeping
/// a second table. The command grammar needs only one..twenty, but a number
/// *parser* needs the whole vocabulary.
pub const NUMBER_WORDS: &[(&str, i64)] = &[
    ("zero", 0),
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("thirteen", 13),
    ("fourteen", 14),
    ("fifteen", 15),
    ("sixteen", 16),
    ("seventeen", 17),
    ("eighteen", 18),
    ("nineteen", 19),
    ("twenty", 20),
    ("thirty", 30),
    ("forty", 40),
    ("fifty", 50),
    ("sixty", 60),
    ("seventy", 70),
    ("eighty", 80),
    ("ninety", 90),
];

pub const MULTIPLIERS: &[(&str, i64)] =
    &[("hundred", 100), ("thousand", 1000), ("million", 1_000_000)];

/// Symbols that attach to the number before them.
pub const CURRENCY: &[(&str, &str)] = &[
    ("dollar", "$"),
    ("dollars", "$"),
    ("euro", "€"),
    ("euros", "€"),
    ("pound", "£"),
    ("pounds", "£"),
];

pub const PERCENT_WORDS: &[&str] = &["percent"];

const TRAILING_PUNCTUATION: &[char] = &['.', ',', ';', ':', '!', '?'];

#[must_use]
pub fn number_word(token: &str) -> Option<i64> {
    NUMBER_WORDS
        .iter()
        .find(|(w, _)| *w == token)
        .map(|(_, v)| *v)
}

fn multiplier(token: &str) -> Option<i64> {
    MULTIPLIERS
        .iter()
        .find(|(w, _)| *w == token)
        .map(|(_, v)| *v)
}

fn currency(token: &str) -> Option<&'static str> {
    CURRENCY.iter().find(|(w, _)| *w == token).map(|(_, s)| *s)
}

fn is_number_token(token: &str) -> bool {
    number_word(token).is_some() || multiplier(token).is_some() || token == "and"
}

/// The word without trailing punctuation, which Whisper attaches freely.
fn strip(word: &str) -> &str {
    word.trim_matches(|c| TRAILING_PUNCTUATION.contains(&c))
}

/// Reproduces Python's `word[len(_strip(word)):]` exactly.
///
/// Note the quirk being preserved: `strip` removes from *both* ends, but the
/// slice is taken from the front, so `".5."` yields `"5."` rather than `"."`.
/// It is only ever consulted for words that end in punctuation.
fn trailing(word: &str) -> String {
    let stripped_len = strip(word).chars().count();
    if word.ends_with(TRAILING_PUNCTUATION) {
        word.chars().skip(stripped_len).collect()
    } else {
        String::new()
    }
}

/// Fold a run of number words into one integer, or `None` if it is not one.
///
/// "and" is only permitted between parts ("three hundred and twelve"); a run
/// that is nothing but "and" is not a number.
// clippy wants the final `else { return None }` folded into `multiplier(token)?`.
// Declined: the arms classify a token as number word, multiplier or unknown, and
// the third abandoning the whole run is the rule worth seeing, not an operator.
#[allow(clippy::question_mark)]
fn compose(tokens: &[String]) -> Option<i64> {
    let mut total: i64 = 0;
    let mut current: i64 = 0;
    let mut seen_number = false;
    for token in tokens {
        if token == "and" {
            continue;
        }
        if let Some(value) = number_word(token) {
            current += value;
            seen_number = true;
        } else if let Some(mult) = multiplier(token) {
            // "three hundred" multiplies what is pending; "hundred" alone is 100.
            current = if current == 0 { 1 } else { current } * mult;
            if mult >= 1000 {
                total += current;
                current = 0;
            }
            seen_number = true;
        } else {
            return None;
        }
    }
    if seen_number {
        Some(total + current)
    } else {
        None
    }
}

fn unit_at(words: &[&str], index: usize) -> Option<String> {
    let word = words.get(index)?;
    let token = strip(word).to_lowercase();
    if currency(&token).is_some() || PERCENT_WORDS.contains(&token.as_str()) {
        Some(token)
    } else {
        None
    }
}

fn render(number: &str, words: &[&str], index: usize) -> String {
    let Some(unit) = unit_at(words, index) else {
        return number.to_owned();
    };
    let tail = trailing(words[index]);
    match currency(&unit) {
        Some(symbol) => format!("{symbol}{number}{tail}"),
        None => format!("{number}%{tail}"),
    }
}

fn consume_unit(words: &[&str], index: usize) -> usize {
    if unit_at(words, index).is_some() {
        index + 1
    } else {
        index
    }
}

/// Rewrite spoken numbers, with currency and percent attached.
#[must_use]
pub fn apply_number_formatting(text: &str) -> String {
    // Python splits on a single space and keeps empties; `split(' ')` matches.
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;

    while index < words.len() {
        let start = index;
        // "numeral seven" is how you ask for 7 where Rule 2 below would keep
        // the word. It only counts immediately before a number, so "roman
        // numeral" and a lone "numeral" are ordinary words.
        let forced = strip(words[index]).to_lowercase() == "numeral"
            && index + 1 < words.len()
            && is_number_token(&strip(words[index + 1]).to_lowercase());
        if forced {
            index += 1;
        }
        let mut run: Vec<String> = Vec::new();
        while index < words.len() {
            let token = strip(words[index]).to_lowercase();
            if !is_number_token(&token) {
                break;
            }
            run.push(token);
            index += 1;
        }

        if run.is_empty() {
            out.push(words[index].to_owned());
            index += 1;
            continue;
        }

        let value = compose(&run);
        // A run of nothing but multipliers is an idiom, not a quantity: "one in
        // a million" and "thanks a million" must not sprout digits. A real
        // number names its cardinal — "three hundred", "two thousand".
        let has_cardinal = run.iter().any(|t| number_word(t).is_some());
        let Some(value) = value.filter(|_| has_cardinal) else {
            out.extend(words[start..index].iter().map(|w| (*w).to_owned()));
            continue;
        };

        // "twenty five point three" -> 25.3
        if index + 1 < words.len() && strip(words[index]).to_lowercase() == "point" {
            let mut digits = String::new();
            let mut cursor = index + 1;
            while cursor < words.len() {
                let token = strip(words[cursor]).to_lowercase();
                match number_word(&token) {
                    Some(digit) => {
                        digits.push_str(&digit.to_string());
                        cursor += 1;
                    }
                    None => break,
                }
            }
            if !digits.is_empty() {
                let rendered = format!("{value}.{digits}");
                index = cursor;
                out.push(render(&rendered, &words, index));
                index = consume_unit(&words, index);
                continue;
            }
        }

        let rendered = value.to_string();
        let multi_word = run.len() > 1;
        let unit_follows = unit_at(&words, index).is_some();

        // Rule 2: a bare small number only converts next to a unit, so "one
        // idea" stays words while "one dollar" becomes "$1".
        if !multi_word && !unit_follows && value < 100 && !forced {
            out.extend(words[start..index].iter().map(|w| (*w).to_owned()));
            continue;
        }

        out.push(render(&rendered, &words, index));
        index = consume_unit(&words, index);
    }

    out.join(" ")
}

static DIGIT_UNIT: LazyLock<Regex> = LazyLock::new(|| {
    let mut units: Vec<&str> = CURRENCY.iter().map(|(w, _)| *w).collect();
    for word in PERCENT_WORDS {
        units.push(word);
    }
    // The reference builds this from a set, so duplicates collapse; then sorts
    // longest-first.
    units.sort_unstable();
    units.dedup();
    units.sort_by_key(|w| std::cmp::Reverse(w.len()));
    Regex::new(&format!(
        r"(?i)\b(?P<number>\d[\d,]*(?:\.\d+)?)\s+(?P<unit>{})\b",
        units.join("|")
    ))
    .expect("digit-unit pattern compiles")
});

/// Digits Whisper already emitted still deserve their unit: "25 dollars" → "$25".
#[must_use]
pub fn attach_units_to_digits(text: &str) -> String {
    replace_all(&DIGIT_UNIT, text, |caps| {
        let number = caps.name("number").expect("number group").as_str();
        let unit = caps
            .name("unit")
            .expect("unit group")
            .as_str()
            .to_lowercase();
        match currency(&unit) {
            Some(symbol) => format!("{symbol}{number}"),
            None => format!("{number}%"),
        }
    })
}

#[cfg(test)]
mod numeral_tests {
    use super::apply_number_formatting as f;

    #[test]
    fn numeral_forces_a_bare_small_number_to_digits() {
        assert_eq!(f("numeral seven"), "7");
        assert_eq!(f("i want numeral three"), "i want 3");
        assert_eq!(f("numeral one idea"), "1 idea");
    }

    #[test]
    fn without_it_rule_two_still_keeps_the_word() {
        assert_eq!(f("i have one idea"), "i have one idea");
    }

    #[test]
    fn numeral_is_an_ordinary_word_away_from_a_number() {
        assert_eq!(f("roman numeral"), "roman numeral");
        assert_eq!(f("the numeral"), "the numeral");
        assert_eq!(f("numeral please"), "numeral please");
    }

    #[test]
    fn numbers_that_already_converted_are_unaffected() {
        assert_eq!(f("twenty five"), "25");
        assert_eq!(f("numeral twenty five"), "25");
    }
}
