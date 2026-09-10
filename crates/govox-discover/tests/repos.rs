//! Discovery against a filesystem the test owns.
//!
//! The trees here are real but scratch, and no test reads anything belonging to
//! the machine it runs on. Nothing shells out, so none of this needs `git`
//! installed — which is the same property that keeps a machine without `git`
//! from becoming a daemon that will not start.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use govox_core::discovery::{DiscoverySpec, ProviderName, TermProvider};
use govox_discover::machine::{SshHostsProvider, SystemdUnitsProvider};
use govox_discover::repos::{BranchProvider, RepoProvider};
use govox_discover::roots::expand_roots;

/// A directory of our own, in the style `govox-daemon`'s watch tests use: no
/// `tempfile` dev-dependency for something this small.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("govox-discover-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// A checkout with a `.git` directory, a branch, and optionally an origin.
fn repo(root: &Path, name: &str, branch: &str, origin: Option<&str>) -> PathBuf {
    let repo = root.join(name);
    let git = repo.join(".git");
    fs::create_dir_all(git.join("refs").join("heads")).unwrap();
    fs::write(git.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();

    // `feature/x` nests: a directory and a file, exactly as git writes it.
    let ref_path = git.join("refs").join("heads").join(branch);
    fs::create_dir_all(ref_path.parent().unwrap()).unwrap();
    fs::write(ref_path, "bc6e6dbf1f3d4e5a6b7c8d9e0f1a2b3c4d5e6f70\n").unwrap();

    if let Some(url) = origin {
        fs::write(
            git.join("config"),
            format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n"),
        )
        .unwrap();
    }
    repo
}

/// Stamp `<repo>/.git/HEAD`, which is what the recency ordering reads.
fn touch(repo: &Path, ago: Duration) {
    let head = fs::File::options()
        .write(true)
        .open(repo.join(".git").join("HEAD"))
        .unwrap();
    head.set_modified(SystemTime::now() - ago).unwrap();
}

fn terms(provider: &dyn TermProvider, spec: &DiscoverySpec) -> Vec<String> {
    provider.candidates(spec).terms
}

#[test]
fn a_repo_root_that_does_not_exist_is_skipped_rather_than_fatal() {
    // The ordinary case on a machine that has not been set up yet. A missing
    // directory is not the user's instruction, so it cannot stop the daemon.
    let home = scratch("missing-root");
    let roots = expand_roots(
        &[
            "~/dev/*/repos".to_string(),
            "/definitely/not/here".to_string(),
        ],
        Some(&home),
    );
    assert!(roots.is_empty());

    let provider = RepoProvider { roots };
    assert!(terms(&provider, &DiscoverySpec::default()).is_empty());
}

#[test]
fn a_glob_in_a_repo_root_expands_to_every_context_that_exists() {
    let home = scratch("glob");
    for context in ["personal", "rentals"] {
        fs::create_dir_all(home.join("dev").join(context).join("repos")).unwrap();
    }
    // A sibling with no `repos` inside must not become a root.
    fs::create_dir_all(home.join("dev").join("scratch")).unwrap();

    let roots = expand_roots(&["~/dev/*/repos".to_string()], Some(&home));
    assert_eq!(
        roots,
        vec![
            home.join("dev").join("personal").join("repos"),
            home.join("dev").join("rentals").join("repos"),
        ],
        "sorted, so two runs over an unchanged disk spend the budget the same way"
    );
}

#[test]
fn a_repo_with_no_origin_still_contributes_its_directory_name() {
    // One of the real checkouts here has no remote at all. The directory name
    // is what gets said out loud regardless of whether it was ever pushed.
    let root = scratch("no-origin");
    repo(&root, "Rentals-LLM-Instructions", "main", None);
    repo(
        &root,
        "govox-rs",
        "develop",
        Some("git@github.com:rockyburt/govox-rs.git"),
    );

    let provider = RepoProvider {
        roots: vec![root.clone()],
    };
    let found = terms(&provider, &DiscoverySpec::default());
    assert!(found.contains(&"Rentals-LLM-Instructions".to_string()));
    assert!(found.contains(&"govox-rs".to_string()));
    assert!(
        found.contains(&"rockyburt".to_string()),
        "the org comes from origin: {found:?}"
    );
}

#[test]
fn a_worktree_whose_dot_git_is_a_file_is_followed_to_its_real_git_dir() {
    // A worktree is exactly the case where the branch is worth biasing —
    // somebody made a whole directory for it.
    let root = scratch("worktree");
    let main = repo(&root, "govox-rs", "develop", None);

    let real_git = main
        .join(".git")
        .join("worktrees")
        .join("dictionary-discovery");
    fs::create_dir_all(real_git.join("refs").join("heads")).unwrap();
    fs::write(real_git.join("HEAD"), "ref: refs/heads/discovery-rollout\n").unwrap();

    let linked = root.join("govox-rs-spike");
    fs::create_dir_all(&linked).unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", real_git.display()),
    )
    .unwrap();

    let provider = BranchProvider {
        roots: vec![root.clone()],
    };
    let found = terms(&provider, &DiscoverySpec::default());
    assert!(
        found.contains(&"discovery".to_string()) && found.contains(&"rollout".to_string()),
        "the worktree's own branch, split into words: {found:?}"
    );
}

#[test]
fn repos_are_ordered_by_recency_so_the_cap_keeps_the_ones_in_use() {
    // The ordering *is* the budget policy: when there is not room for every
    // repository, today's should win over one cloned two years ago.
    let root = scratch("recency");
    let stale = repo(&root, "stale-repo", "main", None);
    let active = repo(&root, "active-repo", "main", None);
    touch(&stale, Duration::from_secs(60 * 60 * 24 * 400));
    touch(&active, Duration::from_secs(60));

    let spec = DiscoverySpec {
        max_repos: 1,
        ..DiscoverySpec::default()
    };
    let provider = RepoProvider {
        roots: vec![root.clone()],
    };
    assert_eq!(terms(&provider, &spec), vec!["active-repo"]);
}

#[test]
fn a_branch_contributes_its_words_and_the_scaffolding_is_dropped() {
    let root = scratch("branch-words");
    repo(&root, "RentalsCa", "feature/rentals-dashboard", None);

    let provider = BranchProvider {
        roots: vec![root.clone()],
    };
    let found = terms(&provider, &DiscoverySpec::default());
    assert!(found.contains(&"rentals".to_string()));
    assert!(found.contains(&"dashboard".to_string()));
    assert!(
        !found.iter().any(|term| term == "feature"),
        "nobody dictates the scaffolding: {found:?}"
    );
}

#[test]
fn an_unreadable_ssh_config_yields_no_terms_and_no_error() {
    let home = scratch("ssh");
    let provider = SshHostsProvider {
        path: home.join(".ssh").join("config"),
    };
    assert!(terms(&provider, &DiscoverySpec::default()).is_empty());

    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::write(
        home.join(".ssh").join("config"),
        "Host *\n  ServerAliveInterval 60\n\nHost thinkpad\n  User rocky\n",
    )
    .unwrap();
    assert_eq!(
        terms(&provider, &DiscoverySpec::default()),
        vec!["thinkpad"]
    );
}

#[test]
fn a_systemd_unit_is_named_by_its_file_and_never_by_systemctl() {
    let home = scratch("units");
    let units = home.join(".config").join("systemd").join("user");
    fs::create_dir_all(&units).unwrap();
    for file in [
        "govox-rs-dev.service",
        "govox-mic-volume-lock.service",
        "getty@.service",
        "notes.txt",
    ] {
        fs::write(units.join(file), "").unwrap();
    }

    let provider = SystemdUnitsProvider { dir: units };
    let found = terms(&provider, &DiscoverySpec::default());
    assert!(found.contains(&"govox-rs-dev".to_string()));
    assert!(found.contains(&"govox-mic-volume-lock".to_string()));
    assert!(
        found.contains(&"getty".to_string()),
        "a template's `@` is systemd's grammar, not part of the name: {found:?}"
    );
    assert!(!found.iter().any(|term| term == "notes"), "not a unit");
}

#[test]
fn the_watch_set_names_head_and_packed_refs_but_never_the_index() {
    // `.git/index` is rewritten by every `git status`. Watching it would turn
    // an idle editor into a reload loop.
    let root = scratch("watch-set");
    repo(&root, "govox-rs", "develop", None);

    let provider = BranchProvider {
        roots: vec![root.clone()],
    };
    let watch = provider.watch_paths(&DiscoverySpec::default());
    assert!(watch.files.iter().any(|path| path.ends_with("HEAD")));
    assert!(watch.files.iter().any(|path| path.ends_with("packed-refs")));
    assert!(!watch.files.iter().any(|path| path.ends_with("index")));

    let roots_watch = RepoProvider {
        roots: vec![root.clone()],
    }
    .watch_paths(&DiscoverySpec::default());
    assert_eq!(
        roots_watch.dirs,
        vec![root],
        "the root itself, so a clone is noticed"
    );
}

#[test]
fn a_provider_the_spec_switches_off_is_not_run() {
    let home = scratch("switched-off");
    let spec = DiscoverySpec {
        providers: vec![ProviderName::Hostname],
        ..DiscoverySpec::default()
    };
    let found = govox_discover::discover(&spec, Some(&home), &[]);
    assert!(
        found
            .candidates
            .iter()
            .all(|answer| answer.provider == Some(ProviderName::Hostname)),
        "only what was asked for ran: {:?}",
        found.candidates
    );
}

// --- the two listed providers ------------------------------------------------

#[test]
fn a_plain_directory_contributes_its_name_without_needing_a_git() {
    // The whole point of `dirs`: a project folder that was never a checkout,
    // which `repos` skips entirely for want of a `.git`.
    let root = scratch("dirs");
    fs::create_dir_all(root.join("personal-playground")).unwrap();
    fs::create_dir_all(root.join(".hidden-machinery")).unwrap();
    fs::write(root.join("notes.txt"), "").unwrap();

    let provider = govox_discover::listed::DirProvider {
        roots: vec![root.clone()],
    };
    assert_eq!(
        terms(&provider, &DiscoverySpec::default()),
        vec!["personal-playground"],
        "a dotfile directory is machinery and a file is not a directory"
    );
}

#[test]
fn directories_are_capped_because_nothing_vouches_for_them() {
    // A checkout has a `.git` proving someone works there. A directory has
    // nothing, so the cap is the only thing standing between a broad root and
    // the whole budget.
    let root = scratch("dirs-cap");
    for name in ["alpha-project", "beta-project", "gamma-project"] {
        fs::create_dir_all(root.join(name)).unwrap();
    }
    let spec = DiscoverySpec {
        max_dirs: 1,
        ..DiscoverySpec::default()
    };
    let provider = govox_discover::listed::DirProvider {
        roots: vec![root.clone()],
    };
    assert_eq!(terms(&provider, &spec).len(), 1);
}

#[test]
fn only_executables_are_commands_and_a_version_pin_collapses() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = scratch("commands");
    let exe = |name: &str| {
        let path = root.join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    exe("zellij");
    exe("kubectl");
    exe("kubectl-1.37.9");
    exe("penwell-gui");
    // Not executable, so not a tool: a README and an editor's backup.
    fs::write(root.join("README"), "").unwrap();
    fs::write(root.join("claude~"), "").unwrap();

    let provider = govox_discover::listed::CommandProvider {
        roots: vec![root.clone()],
    };
    let mut found = terms(&provider, &DiscoverySpec::default());
    found.sort();
    assert_eq!(
        found,
        vec!["kubectl", "penwell-gui", "zellij"],
        "one kubectl, not two, and nothing that cannot be run"
    );
}

#[test]
fn a_bin_root_that_does_not_exist_yields_no_commands_and_no_error() {
    let root = scratch("commands-missing");
    let provider = govox_discover::listed::CommandProvider {
        roots: vec![root.join("nowhere")],
    };
    assert!(terms(&provider, &DiscoverySpec::default()).is_empty());
    assert_eq!(
        provider.watch_paths(&DiscoverySpec::default()).dirs.len(),
        1,
        "watched anyway: installing the first tool should be noticed"
    );
}

#[test]
fn a_term_file_is_read_and_a_missing_one_is_not_an_error() {
    let dir = scratch("term-files");
    let listed = dir.join("clients.txt");
    fs::write(&listed, "# clients\nNuvei\nJobber Twillingate\n").unwrap();

    let provider = govox_discover::listed::FileProvider {
        paths: vec![listed.clone(), dir.join("absent.txt")],
    };
    assert_eq!(
        terms(&provider, &DiscoverySpec::default()),
        vec!["Nuvei", "Jobber", "Twillingate"]
    );
    assert_eq!(
        provider.watch_paths(&DiscoverySpec::default()).files.len(),
        2,
        "a file that does not exist yet is exactly the one to watch for"
    );
}

#[test]
fn a_glob_finds_every_term_file_and_nothing_that_is_not_one() {
    let dir = scratch("term-glob");
    let terms_dir = dir.join("terms");
    fs::create_dir_all(&terms_dir).unwrap();
    fs::write(terms_dir.join("clients.txt"), "Nuvei\n").unwrap();
    fs::write(terms_dir.join("places.txt"), "Twillingate\n").unwrap();
    fs::write(terms_dir.join("README.md"), "Ignored\n").unwrap();

    let pattern = format!("{}/terms/*.txt", dir.display());
    let paths = govox_discover::roots::expand_files(&[pattern], None);
    assert_eq!(paths.len(), 2, "files only, and only .txt: {paths:?}");

    let found = terms(
        &govox_discover::listed::FileProvider { paths },
        &DiscoverySpec::default(),
    );
    assert!(found.contains(&"Nuvei".to_string()));
    assert!(found.contains(&"Twillingate".to_string()));
    assert!(!found.contains(&"Ignored".to_string()));
}
