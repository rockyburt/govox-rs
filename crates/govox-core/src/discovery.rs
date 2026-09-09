//! Bias terms discovered from the machine, rather than typed out by hand.
//!
//! A hand-written `[dictionary] bias` list is wrong the moment anything moves:
//! a repository cloned, a branch created, a machine renamed. The words worth
//! biasing are already on the disk — so govox enumerates them instead.
//!
//! The shape is borrowed from bash completion. A [`TermProvider`] is a
//! completion function: it answers "what is out there" and nothing else.
//! Ordering, deduplication and the budget are decided here, by the caller, so
//! a provider is only ever a report about the machine and never a policy about
//! the prompt.
//!
//! This module is the pure half. Everything here is a function of text already
//! in hand, so it is unit-tested against fixture strings with no filesystem and
//! no `git` binary; `govox-discover` does the reading.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::domain::PersonalDictionary;

/// Repositories considered, newest first, when none is configured.
///
/// Large enough to cover every checkout on a working machine, small enough
/// that the watch set stays a rounding error against the inotify limit.
pub const DEFAULT_MAX_REPOS: usize = 64;

/// Branches taken from each repository, most recently touched first.
pub const DEFAULT_MAX_BRANCHES_PER_REPO: usize = 8;

/// A named source of candidate bias terms.
///
/// The declaration order is the priority order: when the budget runs out it is
/// the later variants that lose their terms, so the list reads from "the thing
/// being worked on" down to "the machine it is being worked on".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderName {
    /// Repository directory names, and the org and name from `origin`.
    Repos,
    /// Words taken from the branches of those repositories.
    Branches,
    /// This machine's own name.
    Hostname,
    /// The hosts named in `~/.ssh/config`.
    SshHosts,
    /// The user's systemd unit names.
    SystemdUnits,
    /// Capture device names, supplied by whoever owns the audio backend.
    AudioDevices,
}

impl ProviderName {
    /// Every provider, in priority order.
    pub const ALL: [Self; 6] = [
        Self::Repos,
        Self::Branches,
        Self::Hostname,
        Self::SshHosts,
        Self::SystemdUnits,
        Self::AudioDevices,
    ];

    /// The name written in `[dictionary.discover] providers`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Repos => "repos",
            Self::Branches => "branches",
            Self::Hostname => "hostname",
            Self::SshHosts => "ssh_hosts",
            Self::SystemdUnits => "systemd_units",
            Self::AudioDevices => "audio_devices",
        }
    }

    /// Parse one configured name. `None` is a name that does not exist, which
    /// the dictionary parser refuses rather than ignores.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.as_str() == name)
    }

    /// Every known name, for the error message that lists them.
    #[must_use]
    pub fn known() -> String {
        Self::ALL
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// What `[dictionary.discover]` asks for. Pure data; no IO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverySpec {
    /// Which providers to run. Order here does not matter: the priority order
    /// is [`ProviderName::ALL`], so writing them backwards cannot silently
    /// reorder the budget.
    pub providers: Vec<ProviderName>,
    /// Directories whose children are repositories. Unexpanded: a leading `~`
    /// and any `*` are resolved by the caller, which is the half that is
    /// allowed to touch the filesystem.
    pub repo_roots: Vec<String>,
    pub max_repos: usize,
    pub max_branches_per_repo: usize,
    /// A hard ceiling on discovered terms, or `0` for "whatever the bias
    /// budget allows".
    pub max_terms: usize,
}

impl Default for DiscoverySpec {
    fn default() -> Self {
        Self {
            providers: ProviderName::ALL.to_vec(),
            repo_roots: Vec::new(),
            max_repos: DEFAULT_MAX_REPOS,
            max_branches_per_repo: DEFAULT_MAX_BRANCHES_PER_REPO,
            max_terms: 0,
        }
    }
}

impl DiscoverySpec {
    /// Is this provider switched on?
    #[must_use]
    pub fn runs(&self, provider: ProviderName) -> bool {
        self.providers.contains(&provider)
    }
}

/// One provider's answer: already normalised, in that provider's own order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidates {
    pub provider: Option<ProviderName>,
    pub terms: Vec<String>,
}

impl Candidates {
    #[must_use]
    pub fn new(provider: ProviderName, terms: Vec<String>) -> Self {
        Self {
            provider: Some(provider),
            terms,
        }
    }
}

/// Paths whose change should invalidate a provider's answer.
///
/// `files` are watched by name — the watcher watches their parent directory
/// and filters by exact path, so a sibling that churns is ignored. `dirs` are
/// watched for entries appearing and disappearing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchSet {
    pub files: Vec<PathBuf>,
    pub dirs: Vec<PathBuf>,
}

impl WatchSet {
    /// Fold another provider's paths in.
    pub fn absorb(&mut self, other: Self) {
        self.files.extend(other.files);
        self.dirs.extend(other.dirs);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.dirs.is_empty()
    }
}

/// A named source of candidate bias terms enumerated from the machine.
///
/// Modelled on a bash completion function: it enumerates, it never decides.
/// Selection, ordering, deduplication and the budget belong to the caller, in
/// this crate, so a provider's answer is only ever "what did the machine say".
pub trait TermProvider {
    fn name(&self) -> ProviderName;

    /// Enumerate.
    ///
    /// **Cannot fail.** A root that does not exist, a config that cannot be
    /// read, a repository mid-rebase — none of those are the user's
    /// instruction, and none of them change what gets typed except by
    /// omission. The absent `Result` is that rule made unrepresentable: a
    /// provider has no way to turn a quiet machine into a daemon that will not
    /// start.
    fn candidates(&self, spec: &DiscoverySpec) -> Candidates;

    /// Paths whose change should invalidate this provider's answer. A provider
    /// that reads nothing needs no watch.
    fn watch_paths(&self, _spec: &DiscoverySpec) -> WatchSet {
        WatchSet::default()
    }
}

/// The longest term worth biasing. Past this it is an identifier, not a word.
const MAX_TERM_CHARS: usize = 40;

/// The shortest. Two characters buy nothing and collide with everything.
const MIN_TERM_CHARS: usize = 3;

/// Is this a commit sha wearing a word's clothes?
///
/// All-hex *and* carrying a digit: `deadbeef` stays a word, `bc6e6db` does not.
fn looks_like_a_sha(term: &str) -> bool {
    term.len() >= 7
        && term.chars().all(|c| c.is_ascii_hexdigit())
        && term.chars().any(|c| c.is_ascii_digit())
}

/// Accept one raw candidate, or reject it with a reason that is the same for
/// every provider.
///
/// The whitespace rule is the load-bearing one. `bias_prompt` splits terms on
/// whitespace and spends one budget unit per word, so `"Blue Microphones USB
/// Audio"` costs four units to buy one useful term. Providers split their own
/// multi-word labels and offer the pieces instead.
///
/// Case, `-`, `.` and `_` survive untouched: `Rentals-API` and `Rentals.ca`
/// are each one word and one budget unit, which is the precedent the
/// hand-written examples already set.
#[must_use]
pub fn normalize_candidate(raw: &str) -> Option<String> {
    let term = raw.trim();
    if term.chars().any(char::is_whitespace) {
        return None;
    }
    let length = term.chars().count();
    if !(MIN_TERM_CHARS..=MAX_TERM_CHARS).contains(&length) {
        return None;
    }
    // An identifier is only worth biasing if it is pronounceable, so it has to
    // contain a letter. This drops version numbers and separator runs.
    if !term.chars().any(char::is_alphabetic) {
        return None;
    }
    if !term
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '.' | '_'))
    {
        return None;
    }
    if looks_like_a_sha(term) {
        return None;
    }
    Some(term.to_string())
}

/// Branch-name words nobody dictates.
const BRANCH_STOPLIST: &[&str] = &[
    "feature", "feat", "fix", "bugfix", "hotfix", "chore", "wip", "main", "master", "dev",
    "develop", "release", "refactor", "docs", "doc", "test", "tests", "spike", "patch", "branch",
    "tmp", "temp", "the", "and", "for", "new", "old",
];

fn is_ticket_prefix(segment: &str) -> bool {
    let length = segment.chars().count();
    (2..=10).contains(&length) && segment.chars().all(|c| c.is_ascii_alphabetic())
}

/// The words inside a branch name, rather than the branch name itself.
///
/// Nobody dictates `feature/rentals-dashboard`; they say "the rentals
/// dashboard branch". Biasing the slug spends budget on a token Whisper will
/// never emit, so the slug is split, the scaffolding (`feature`, `fix`,
/// `main`) is dropped, ticket ids go with it, and what survives is the words
/// actually spoken out loud.
#[must_use]
pub fn branch_terms(branch: &str) -> Vec<String> {
    let segments: Vec<&str> = branch
        .split(['/', '-', '_', '.'])
        .filter(|segment| !segment.is_empty())
        .collect();

    let mut terms = Vec::new();
    let mut index = 0;
    while index < segments.len() {
        let segment = segments[index];
        // `PROJ-1841` arrives split in two. Neither half is a word: the number
        // is noise and the prefix is a tracker's name, not the subject.
        if is_ticket_prefix(segment)
            && segments
                .get(index + 1)
                .is_some_and(|next| next.chars().all(|c| c.is_ascii_digit()))
        {
            index += 2;
            continue;
        }
        index += 1;
        if BRANCH_STOPLIST.contains(&segment.to_lowercase().as_str()) {
            continue;
        }
        if let Some(term) = normalize_candidate(segment)
            && !terms.contains(&term)
        {
            terms.push(term);
        }
    }
    terms
}

/// Words an audio backend puts in every device label, which name no device.
const DEVICE_STOPLIST: &[&str] = &[
    "usb",
    "audio",
    "device",
    "devices",
    "default",
    "sysdefault",
    "card",
    "dev",
    "mono",
    "stereo",
    "analog",
    "digital",
    "input",
    "output",
    "front",
    "rear",
    "pcm",
    "alsa",
    "pulse",
    "pipewire",
    "jack",
    "hdmi",
    "built-in",
    "internal",
    "generic",
    "controller",
];

/// The words inside a capture device's id or label.
///
/// A backend reports `"Blue Microphones, USB Audio"` and
/// `"hw:CARD=Microphones,DEV=0"` for the same microphone. Neither is a term:
/// the first costs four budget units to buy one useful word, and the second is
/// punctuation. Both are split, the backend's own furniture is dropped, and
/// what is left — `Microphones`, `govox_blue` — is what someone would say.
#[must_use]
pub fn device_terms(label: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for piece in label.split([' ', ',', ':', '=', '/', '(', ')', '[', ']', '"']) {
        if DEVICE_STOPLIST.contains(&piece.to_lowercase().as_str()) {
            continue;
        }
        if let Some(term) = normalize_candidate(piece)
            && !terms.contains(&term)
        {
            terms.push(term);
        }
    }
    terms
}

/// A remote as it is written in `.git/config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRemote {
    pub org: Option<String>,
    pub repo: String,
}

/// The org and repository name of `origin`, read out of `.git/config`.
///
/// Both spellings git writes are handled — `git@github.com:Org/Repo.git` and
/// `https://github.com/Org/Repo.git` — because which one a checkout carries is
/// an accident of how it was cloned, not a statement about the project.
#[must_use]
pub fn parse_git_config_origin(text: &str) -> Option<GitRemote> {
    let mut in_origin = false;
    let mut url = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.replace(char::is_whitespace, "") == "[remote\"origin\"]";
            continue;
        }
        if !in_origin {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            url = Some(value.trim().to_string());
            break;
        }
    }
    parse_remote_url(&url?)
}

/// Split a remote URL into its last two path segments.
///
/// Only the tail matters. The host is not vocabulary — nobody dictates
/// "github.com" — and the scheme is an accident of the clone.
fn parse_remote_url(url: &str) -> Option<GitRemote> {
    let scp_like = !url.contains("://");
    let path = if scp_like {
        // `git@github.com:Org/Repo.git` — everything after the colon.
        url.split_once(':').map_or(url, |(_, path)| path)
    } else {
        // `https://github.com/Org/Repo.git` — everything after the host.
        let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
        after_scheme
            .split_once('/')
            .map_or(after_scheme, |(_, path)| path)
    };

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.rsplit('/');
    let repo = normalize_candidate(segments.next()?)?;
    let org = segments.next().and_then(normalize_candidate);
    Some(GitRemote { org, repo })
}

/// The checked-out branch, from `.git/HEAD`.
///
/// A detached HEAD holds a sha rather than a ref, and a sha is not a word.
#[must_use]
pub fn parse_head(text: &str) -> Option<String> {
    text.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .filter(|branch| !branch.is_empty())
}

/// Local branch names from `.git/packed-refs`.
///
/// Comments and the `^` peeled-tag lines are skipped, and only `refs/heads/`
/// is read: a remote-tracking branch is somebody else's vocabulary.
#[must_use]
pub fn parse_packed_refs(text: &str) -> Vec<String> {
    let mut branches = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        if let Some((_, reference)) = line.split_once(' ')
            && let Some(branch) = reference.trim().strip_prefix("refs/heads/")
            && !branch.is_empty()
        {
            branches.push(branch.to_string());
        }
    }
    branches
}

/// The hosts named in an ssh config.
///
/// Every alias on a `Host` line counts, because any of them is what gets
/// spoken. Patterns are dropped: `*` is not a name, and a name with a wildcard
/// in it was never going to be dictated either. `Include` is deliberately not
/// followed — one bounded read, no cycles to guard against.
#[must_use]
pub fn parse_ssh_config_hosts(text: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((keyword, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        for alias in rest.split_whitespace() {
            if alias.contains('*') || alias.contains('?') || alias.starts_with('!') {
                continue;
            }
            if let Some(term) = normalize_candidate(alias)
                && !hosts.contains(&term)
            {
                hosts.push(term);
            }
        }
    }
    hosts
}

/// What one term costs against `bias_prompt_token_budget`.
///
/// The same rule `bias_prompt` applies: whitespace-separated words. Kept here
/// rather than imported so `govox-core` can plan a budget it does not own the
/// spending of.
#[must_use]
pub fn word_cost(term: &str) -> usize {
    term.split_whitespace().count()
}

/// The core bias list, and what did not fit in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BiasPlan {
    /// The core list, in priority order, that fits.
    pub terms: Vec<String>,
    /// Discovered terms that did not, in the order they were dropped.
    pub dropped: Vec<String>,
    /// What `terms` costs.
    pub words: usize,
    /// Words held back so the largest group still fits beside them.
    pub reserved: usize,
}

impl BiasPlan {
    #[must_use]
    pub fn overflowed(&self) -> bool {
        !self.dropped.is_empty()
    }
}

/// Build the core bias list from the hand-written one and what was discovered.
///
/// Three rules, in order:
///
/// 1. **Hand-written terms are never dropped.** They are an instruction; a
///    discovered term is an observation, and an observation does not get to
///    evict an instruction. If the hand-written list alone overruns the budget
///    that is today's behaviour, unchanged.
/// 2. **Room is reserved for the largest `[[dictionary.bias_group]]`**, so
///    focusing the window a group is scoped to cannot silently evict the core
///    that was discovered for every window.
/// 3. **Discovered terms fill what is left**, in provider priority order,
///    deduplicated case-insensitively against everything already listed.
///
/// What does not fit is *returned* rather than quietly truncated. `bias_prompt`
/// truncates in list order and says nothing, which was fine while the list was
/// hand-sized; a machine-sized list needs someone to be able to say which
/// words were lost, so the planner decides and the daemon reports.
#[must_use]
pub fn plan_bias(hand: &PersonalDictionary, found: &[Candidates], budget: u32) -> BiasPlan {
    let mut terms = hand.bias_terms.clone();
    let mut words: usize = terms.iter().map(|term| word_cost(term)).sum();

    let reserved = hand
        .bias_groups
        .iter()
        .map(|group| {
            group
                .terms
                .iter()
                .map(|term| word_cost(term))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);

    let mut seen: HashSet<String> = terms.iter().map(|term| term.to_lowercase()).collect();
    let ceiling = (budget as usize).saturating_sub(reserved);
    let max_terms = hand
        .discover
        .as_ref()
        .map_or(0, |discover| discover.max_terms);

    let mut dropped = Vec::new();
    let mut discovered = 0usize;
    for provider in ProviderName::ALL {
        for answer in found
            .iter()
            .filter(|answer| answer.provider == Some(provider))
        {
            for term in &answer.terms {
                if !seen.insert(term.to_lowercase()) {
                    continue;
                }
                let cost = word_cost(term);
                let capped = max_terms != 0 && discovered >= max_terms;
                if capped || words + cost > ceiling {
                    dropped.push(term.clone());
                    continue;
                }
                terms.push(term.clone());
                words += cost;
                discovered += 1;
            }
        }
    }

    BiasPlan {
        terms,
        dropped,
        words,
        reserved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::BiasGroup;

    fn dictionary(bias: &[&str]) -> PersonalDictionary {
        PersonalDictionary {
            bias_terms: bias.iter().map(|term| (*term).to_string()).collect(),
            ..PersonalDictionary::default()
        }
    }

    fn found(provider: ProviderName, terms: &[&str]) -> Candidates {
        Candidates::new(
            provider,
            terms.iter().map(|term| (*term).to_string()).collect(),
        )
    }

    #[test]
    fn an_ssh_config_host_line_yields_every_alias_but_not_the_wildcard() {
        let config = "\
Host *
  ServerAliveInterval 60

# a comment
Host rockyburt-desktop desktop
  HostName 192.168.1.20

host thinkpad
  User rocky

Host build-*
  User builder
";
        assert_eq!(
            parse_ssh_config_hosts(config),
            vec!["rockyburt-desktop", "desktop", "thinkpad"],
            "`Host *` and `build-*` are patterns, not names anyone dictates"
        );
    }

    #[test]
    fn a_git_config_yields_the_org_and_repo_from_either_remote_form() {
        let ssh = "\
[core]
	bare = false
[remote \"origin\"]
	url = git@github.com:rockyburt/govox-rs.git
	fetch = +refs/heads/*:refs/remotes/origin/*
";
        let https = "\
[remote \"origin\"]
	url = https://github.com/torontorentalsuser/Rentals-API.git
";
        assert_eq!(
            parse_git_config_origin(ssh),
            Some(GitRemote {
                org: Some("rockyburt".to_string()),
                repo: "govox-rs".to_string(),
            })
        );
        assert_eq!(
            parse_git_config_origin(https),
            Some(GitRemote {
                org: Some("torontorentalsuser".to_string()),
                repo: "Rentals-API".to_string(),
            })
        );
    }

    #[test]
    fn a_remote_named_anything_but_origin_is_not_the_project() {
        // A fork's `upstream` names somebody else's org. Biasing it would put
        // a stranger's vocabulary in every window.
        let config = "\
[remote \"upstream\"]
	url = git@github.com:someone-else/govox-rs.git
";
        assert_eq!(parse_git_config_origin(config), None);
    }

    #[test]
    fn packed_refs_and_loose_refs_agree_on_the_branch_name() {
        let packed = "\
# pack-refs with: peeled fully-peeled sorted
bc6e6dbf1f3d4e5a6b7c8d9e0f1a2b3c4d5e6f70 refs/heads/develop
7336ddb0a1b2c3d4e5f60718293a4b5c6d7e8f90 refs/heads/feature/rentals-dashboard
9512216a1b2c3d4e5f60718293a4b5c6d7e8f901 refs/remotes/origin/main
1234567a1b2c3d4e5f60718293a4b5c6d7e8f902 refs/tags/v0.2.0
^abcdef01234567890abcdef01234567890abcdef
";
        assert_eq!(
            parse_packed_refs(packed),
            vec!["develop", "feature/rentals-dashboard"],
            "only local heads; a remote-tracking branch is somebody else's"
        );
        assert_eq!(
            parse_head("ref: refs/heads/feature/rentals-dashboard\n"),
            Some("feature/rentals-dashboard".to_string())
        );
    }

    #[test]
    fn a_detached_head_names_no_branch() {
        assert_eq!(
            parse_head("bc6e6dbf1f3d4e5a6b7c8d9e0f1a2b3c4d5e6f70\n"),
            None
        );
    }

    #[test]
    fn a_branch_slug_is_biased_as_words_not_as_the_slug() {
        // Nobody says "feature slash rentals dash dashboard".
        assert_eq!(
            branch_terms("feature/rentals-dashboard"),
            vec!["rentals", "dashboard"]
        );
        assert_eq!(
            branch_terms("fix/PROJ-1841-caret-offset"),
            vec!["caret", "offset"]
        );
        assert_eq!(branch_terms("main"), Vec::<String>::new());
    }

    #[test]
    fn a_commit_sha_and_a_bare_number_are_not_worth_biasing() {
        assert_eq!(normalize_candidate("bc6e6db"), None, "a short sha");
        assert_eq!(normalize_candidate("1841"), None, "a ticket number");
        assert_eq!(normalize_candidate("0.2.0"), None, "a version");
        assert_eq!(normalize_candidate("no"), None, "too short to be worth it");
        // All-hex but no digit is an ordinary word, and words are the point.
        assert_eq!(
            normalize_candidate("deadbeef"),
            Some("deadbeef".to_string())
        );
        assert_eq!(
            normalize_candidate("Rentals-API"),
            Some("Rentals-API".to_string()),
            "case and hyphens survive: one word, one budget unit"
        );
        assert_eq!(
            normalize_candidate("Blue Microphones"),
            None,
            "a multi-word label costs several budget units to buy one term"
        );
    }

    #[test]
    fn a_device_label_is_split_and_the_backends_own_furniture_dropped() {
        // The same microphone, spelled three ways by the same machine.
        assert_eq!(
            device_terms("Blue Microphones, USB Audio"),
            vec!["Blue", "Microphones"]
        );
        assert_eq!(
            device_terms("hw:CARD=Microphones,DEV=0"),
            vec!["Microphones"]
        );
        assert_eq!(device_terms("govox_blue"), vec!["govox_blue"]);
        assert!(
            device_terms("Default ALSA Output (currently PipeWire Media Server)")
                .iter()
                .all(|term| term != "ALSA" && term != "PipeWire"),
            "the backend names itself in every label; none of it is a device"
        );
    }

    #[test]
    fn the_hand_written_core_is_never_dropped_to_make_room_for_a_discovered_term() {
        let hand = dictionary(&["Rentals.ca", "ydotool"]);
        let plan = plan_bias(&hand, &[found(ProviderName::Repos, &["govox-rs"])], 2);
        assert_eq!(plan.terms, vec!["Rentals.ca", "ydotool"]);
        assert_eq!(
            plan.dropped,
            vec!["govox-rs"],
            "an observation does not evict an instruction"
        );
    }

    #[test]
    fn discovered_terms_are_dropped_in_reverse_priority_order_and_reported() {
        let hand = dictionary(&[]);
        let plan = plan_bias(
            &hand,
            &[
                found(ProviderName::SystemdUnits, &["govox-mic-volume-lock"]),
                found(ProviderName::Repos, &["govox-rs", "rockyburt"]),
                found(ProviderName::Branches, &["dashboard"]),
            ],
            3,
        );
        assert_eq!(
            plan.terms,
            vec!["govox-rs", "rockyburt", "dashboard"],
            "repos, then branches, then the machine — whatever order they arrive in"
        );
        assert_eq!(plan.dropped, vec!["govox-mic-volume-lock"]);
        assert!(plan.overflowed());
    }

    #[test]
    fn the_plan_reserves_room_for_the_largest_bias_group() {
        // Without the reserve, focusing the window this group is scoped to
        // would push the discovered core off the end of the prompt.
        let hand = PersonalDictionary {
            bias_groups: vec![
                BiasGroup {
                    while_using: "*RentalsCa*".to_string(),
                    terms: vec!["Rentsync".to_string(), "Jobber".to_string()],
                },
                BiasGroup {
                    while_using: "*govox*".to_string(),
                    terms: vec!["ydotool".to_string()],
                },
            ],
            ..PersonalDictionary::default()
        };
        let plan = plan_bias(
            &hand,
            &[found(ProviderName::Repos, &["govox-rs", "RentalsCa"])],
            3,
        );
        assert_eq!(plan.reserved, 2, "the larger of the two groups");
        assert_eq!(plan.terms, vec!["govox-rs"]);
        assert_eq!(plan.dropped, vec!["RentalsCa"]);
    }

    #[test]
    fn a_discovered_term_already_written_by_hand_is_not_biased_twice() {
        let hand = dictionary(&["govox-rs"]);
        let plan = plan_bias(
            &hand,
            &[found(ProviderName::Repos, &["GOVOX-RS", "rockyburt"])],
            180,
        );
        assert_eq!(
            plan.terms,
            vec!["govox-rs", "rockyburt"],
            "the hand-written spelling wins, and the duplicate costs nothing"
        );
        assert!(plan.dropped.is_empty(), "a duplicate is not a casualty");
    }

    #[test]
    fn max_terms_caps_discovery_without_touching_the_hand_written_list() {
        let hand = PersonalDictionary {
            bias_terms: vec!["Rentals.ca".to_string()],
            discover: Some(DiscoverySpec {
                max_terms: 1,
                ..DiscoverySpec::default()
            }),
            ..PersonalDictionary::default()
        };
        let plan = plan_bias(
            &hand,
            &[found(ProviderName::Repos, &["govox-rs", "rockyburt"])],
            180,
        );
        assert_eq!(plan.terms, vec!["Rentals.ca", "govox-rs"]);
        assert_eq!(plan.dropped, vec!["rockyburt"]);
    }

    #[test]
    fn a_provider_name_round_trips_and_an_unknown_one_does_not_parse() {
        for provider in ProviderName::ALL {
            assert_eq!(ProviderName::parse(provider.as_str()), Some(provider));
        }
        assert_eq!(ProviderName::parse("nope"), None);
        assert!(ProviderName::known().contains("ssh_hosts"));
    }
}
