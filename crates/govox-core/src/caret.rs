//! Deciding where the overlay card should sit, given a reported caret.
//!
//! Three small decisions, kept pure and away from the plumbing that makes the
//! D-Bus calls: which per-application override applies, whether the reported
//! rectangle can be believed, and where the corrected caret is.
//!
//! The bias throughout is towards giving up. A card resting in a known corner
//! is a smaller failure than a card placed confidently over the wrong part of
//! the user's screen.

use crate::config::OverlayAppRule;
use crate::domain::CaretRect;

/// The narrowest caret rectangle whose *position* is worth believing.
///
/// A client reporting a zero-width rectangle has told us where it thinks the
/// caret is without having measured it, and the measured cases were wrong:
/// AT-SPI returns `(0, 0, 0, 0)` extents for its text fields, so there is
/// nothing to cross-check against either. A heuristic, not a guarantee.
pub const TRUSTED_CARET_MIN_WIDTH: i32 = 2;

/// Does `pattern` match `text`, treating `*` and `?` as wildcards?
///
/// `*` stands for any run of characters including none, `?` for exactly one.
/// Both sides are compared as `char`s rather than bytes, because a window
/// title carries whatever the application put there — this was developed
/// against one containing `◐` and an em dash.
///
/// There is no escape syntax, so a literal `*` cannot be matched. Titles
/// containing one are rare enough that an escape character would cost more
/// confusion than it saves.
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0_usize, 0_usize);
    // Where to resume if the current `*` turns out to have consumed too
    // little: step it forward one character and try again. This is what keeps
    // matching linear in practice rather than exponential.
    let mut star: Option<usize> = None;
    let mut retry = 0_usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = t;
        } else if let Some(s) = star {
            p = s + 1;
            retry += 1;
            t = retry;
        } else {
            return false;
        }
    }
    // Trailing `*`s still match the empty remainder.
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// The first rule whose `match` applies to `label`, case-insensitively.
///
/// Two shapes, chosen by whether the pattern contains a wildcard:
///
/// - **No wildcard** — a substring test, so `"google chrome"` matches whatever
///   document happens to be open in it. This is the original behaviour, and
///   every config written before wildcards existed keeps working unchanged.
/// - **`*` or `?` present** — a glob matched against the *whole* label, so
///   `"*chrome*"` is the explicit spelling of the above, and `"* / *gmail*"`
///   can single out one document within an application.
///
/// Anchoring the glob is what makes it worth having. An unanchored one could
/// only ever widen a substring match, never narrow it, so there would be no
/// way to say "this application, but only this document".
///
/// `None` for an unknown window: a rule that cannot be shown to apply must not
/// be applied, or a caret correction meant for one application silently
/// displaces the card in every other.
#[must_use]
pub fn match_app_rule<'a>(
    label: Option<&str>,
    rules: &'a [OverlayAppRule],
) -> Option<&'a OverlayAppRule> {
    let label = label?;
    if label.is_empty() {
        return None;
    }
    let lowered = label.to_lowercase();
    let chars: Vec<char> = lowered.chars().collect();
    rules.iter().find(|rule| {
        let pattern = rule.match_.to_lowercase();
        if pattern.contains('*') || pattern.contains('?') {
            glob_match(&pattern.chars().collect::<Vec<char>>(), &chars)
        } else {
            lowered.contains(&pattern)
        }
    })
}

/// Can this rectangle be believed about *where* the caret is?
///
/// `None` is not a caret at all and is false by definition.
#[must_use]
pub fn caret_is_trustworthy(rect: Option<CaretRect>) -> bool {
    rect.is_some_and(|(_, _, width, _)| width >= TRUSTED_CARET_MIN_WIDTH)
}

/// Shift a reported caret by the correction configured for its application.
///
/// Some clients report a caret in the wrong coordinate space — consistently
/// wrong, which is what makes a hand-measured offset the right repair.
#[must_use]
pub fn apply_caret_offset(
    rect: Option<CaretRect>,
    rule: Option<&OverlayAppRule>,
) -> Option<CaretRect> {
    let rect = rect?;
    let Some(rule) = rule else { return Some(rect) };
    if rule.caret_offset_x == 0 && rule.caret_offset_y == 0 {
        return Some(rect);
    }
    let (x, y, width, height) = rect;
    Some((
        x.saturating_add(i32::try_from(rule.caret_offset_x).unwrap_or(0)),
        y.saturating_add(i32::try_from(rule.caret_offset_y).unwrap_or(0)),
        width,
        height,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, dx: i64, dy: i64) -> OverlayAppRule {
        OverlayAppRule {
            match_: pattern.to_owned(),
            caret_offset_x: dx,
            caret_offset_y: dy,
            follow_caret: None,
        }
    }

    #[test]
    fn a_rule_matches_a_substring_of_the_window_label() {
        let rules = vec![rule("google chrome", 167, 263)];
        let matched = match_app_rule(Some("Inbox — Google Chrome"), &rules);
        assert_eq!(matched.map(|r| r.caret_offset_x), Some(167));
    }

    #[test]
    fn an_unnamed_window_matches_nothing() {
        // The correction is calibrated for one application. Applying it to a
        // window we could not name would displace the card everywhere else.
        let rules = vec![rule("google chrome", 167, 263)];
        assert!(match_app_rule(None, &rules).is_none());
        assert!(match_app_rule(Some(""), &rules).is_none());
        assert!(match_app_rule(Some("Terminal"), &rules).is_none());
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let rules = vec![rule("chrome", 1, 1), rule("google chrome", 2, 2)];
        assert_eq!(
            match_app_rule(Some("Google Chrome"), &rules).map(|r| r.caret_offset_x),
            Some(1)
        );
    }

    /// The real label, as `AtspiTextModel::active_window` reports it.
    const LABEL: &str = "Google Chrome / Inbox (12) — you@example.com — Gmail";

    #[test]
    fn a_plain_pattern_is_still_a_substring_match() {
        // Every rule written before wildcards existed keeps working. This is
        // the shape in the config on this machine.
        let rules = vec![rule("google chrome", 167, 263)];
        assert!(match_app_rule(Some(LABEL), &rules).is_some());
    }

    #[test]
    fn a_wildcard_pattern_matches_the_whole_label() {
        let rules = vec![rule("*chrome*", 1, 1)];
        assert!(match_app_rule(Some(LABEL), &rules).is_some());
        // Anchored, so a bare prefix does not match a label starting further
        // left — that is what makes narrowing possible at all.
        let rules = vec![rule("chrome*", 1, 1)];
        assert!(match_app_rule(Some(LABEL), &rules).is_none());
    }

    #[test]
    fn a_wildcard_can_narrow_to_one_document() {
        // The case a substring test cannot express: this application, but only
        // while this document is open.
        let gmail = vec![rule("google chrome / *gmail*", 1, 1)];
        assert!(match_app_rule(Some(LABEL), &gmail).is_some());
        assert!(match_app_rule(Some("Google Chrome / Hacker News"), &gmail).is_none());
    }

    #[test]
    fn a_question_mark_matches_exactly_one_character() {
        let rules = vec![rule("?oogle chrome*", 1, 1)];
        assert!(match_app_rule(Some(LABEL), &rules).is_some());
        let rules = vec![rule("??oogle chrome*", 1, 1)];
        assert!(match_app_rule(Some(LABEL), &rules).is_none());
    }

    #[test]
    fn wildcards_count_characters_not_bytes() {
        // Titles carry whatever the application put there; this one is the
        // real terminal title from the machine this was developed on.
        let label = "ptyxis / personal | ◐ port-govox-py-to-rust — zellij";
        assert!(match_app_rule(Some(label), &[rule("ptyxis / *◐*", 1, 1)]).is_some());
        // One `?` for one multi-byte character, not for one of its bytes.
        assert!(match_app_rule(Some("app / ◐x"), &[rule("app / ?x", 1, 1)]).is_some());
        assert!(match_app_rule(Some("app / ◐x"), &[rule("app / ??x", 1, 1)]).is_none());
    }

    #[test]
    fn a_lone_star_matches_any_named_window() {
        assert!(match_app_rule(Some(LABEL), &[rule("*", 9, 9)]).is_some());
        // But still not an unnamed one: a rule that cannot be shown to apply
        // is not applied.
        assert!(match_app_rule(None, &[rule("*", 9, 9)]).is_none());
    }

    #[test]
    fn a_caret_with_no_width_is_not_trusted() {
        // The signature of a client that reported a position it never measured.
        assert!(!caret_is_trustworthy(Some((100, 200, 0, 26))));
        assert!(!caret_is_trustworthy(Some((100, 200, 1, 26))));
        assert!(caret_is_trustworthy(Some((100, 200, 2, 26))));
        assert!(!caret_is_trustworthy(None));
    }

    #[test]
    fn an_offset_shifts_the_position_and_leaves_the_size() {
        // The size is what `caret_is_trustworthy` reads, so a correction that
        // changed it would launder an untrustworthy caret into a trusted one.
        assert_eq!(
            apply_caret_offset(Some((10, 20, 2, 26)), Some(&rule("chrome", 167, 263))),
            Some((177, 283, 2, 26))
        );
    }

    #[test]
    fn no_rule_and_no_offset_leave_the_caret_alone() {
        assert_eq!(
            apply_caret_offset(Some((10, 20, 2, 26)), None),
            Some((10, 20, 2, 26))
        );
        assert_eq!(
            apply_caret_offset(Some((10, 20, 2, 26)), Some(&rule("chrome", 0, 0))),
            Some((10, 20, 2, 26))
        );
        assert_eq!(apply_caret_offset(None, None), None);
    }
}
