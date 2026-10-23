//! These tests invoke the installed Git executable and native clone backend.
//! Set GIT_THIN_REQUIRE_CLONE=1 on APFS/reflink CI to make missing clone support fatal.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Repo {
    _temp: TempDir,
    parent: PathBuf,
    source: PathBuf,
}

fn command(program: &str, cwd: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap_or_else(|error| panic!("could not run {program}: {error}"))
}

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn success(output: Output) -> Output {
    assert!(output.status.success(), "{}", describe(&output));
    output
}

fn git(cwd: &Path, args: &[&str]) -> Output {
    let mut all = vec![
        "-c",
        "user.name=Git Thin Tests",
        "-c",
        "user.email=tests@example.invalid",
    ];
    all.extend_from_slice(args);
    success(command("git", cwd, &all))
}

fn thin(cwd: &Path, args: &[&str]) -> Output {
    command(env!("CARGO_BIN_EXE_git-thin"), cwd, args)
}

fn path(path: &Path) -> &str {
    path.to_str().expect("UTF-8 test temporary path")
}

impl Repo {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let source = parent.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "core.autocrlf", "false"]);
        git(&source, &["config", "user.name", "Git Thin Tests"]);
        git(&source, &["config", "user.email", "tests@example.invalid"]);
        fs::create_dir(source.join("src")).unwrap();
        fs::write(source.join("src/main.txt"), "original source\n").unwrap();
        fs::write(source.join("README.md"), "original readme\n").unwrap();
        fs::write(source.join(".gitignore"), "build/\nignored.txt\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "baseline"]);
        Self {
            _temp: temp,
            parent,
            source,
        }
    }

    fn can_clone(&self) -> bool {
        match git_thin::backend::probe(&self.source, &self.parent) {
            Ok(()) => true,
            Err(error) => {
                let lower = error.to_ascii_lowercase();
                let unsupported =
                    lower.contains("native copy-on-write cloning is unsupported here");
                assert!(
                    unsupported && std::env::var("GIT_THIN_REQUIRE_CLONE").as_deref() != Ok("1"),
                    "native clone probe failed: {error}"
                );
                eprintln!("SKIP native clone integration test: {error}");
                false
            }
        }
    }

    fn add(&self, scope: &str) -> Option<PathBuf> {
        if !self.can_clone() {
            return None;
        }
        let destination = self.parent.join("agent checkout");
        success(thin(
            &self.source,
            &[
                "add",
                path(&destination),
                "--branch",
                "agent/test",
                "--write",
                scope,
            ],
        ));
        Some(destination)
    }

    fn reject_add(&self, diagnostic: &[&str]) {
        let destination = self.parent.join("rejected");
        let output = thin(
            &self.source,
            &[
                "--json",
                "add",
                path(&destination),
                "--branch",
                "agent/rejected",
                "--write",
                "src",
            ],
        );
        assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
        let text = describe(&output).to_ascii_lowercase();
        assert!(
            diagnostic.iter().any(|s| text.contains(s)),
            "missing actionable diagnostic: {text}"
        );
        assert!(!destination.exists(), "failed add created destination");
        let branches = git(&self.source, &["branch", "--list", "agent/rejected"]);
        assert!(branches.stdout.is_empty(), "failed add created a branch");
    }
}

fn assert_check(cwd: &Path, worktree: &Path, exit: i32, paths: &[&str]) {
    let output = thin(cwd, &["check", path(worktree), "--json"]);
    assert_eq!(output.status.code(), Some(exit), "{}", describe(&output));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid JSON: {error}; {}", describe(&output)));
    fn contains(value: &serde_json::Value, expected: &str) -> bool {
        match value {
            serde_json::Value::String(actual) => actual == expected,
            serde_json::Value::Array(items) => items.iter().any(|item| contains(item, expected)),
            serde_json::Value::Object(items) => items.values().any(|item| contains(item, expected)),
            _ => false,
        }
    }
    for expected in paths {
        assert!(
            contains(&value, expected),
            "{expected:?} missing from {value}"
        );
    }
}

#[test]
fn independent_contents_indexes_and_ordinary_git_commits() {
    let repo = Repo::new();
    let Some(worktree) = repo.add("src") else {
        return;
    };
    assert_ne!(
        fs::metadata(repo.source.join("src/main.txt"))
            .unwrap()
            .ino(),
        fs::metadata(worktree.join("src/main.txt")).unwrap().ino()
    );
    assert!(git(&worktree, &["status", "--porcelain"]).stdout.is_empty());
    fs::write(worktree.join("src/main.txt"), "agent change\n").unwrap();
    assert_eq!(
        fs::read_to_string(repo.source.join("src/main.txt")).unwrap(),
        "original source\n"
    );
    git(&worktree, &["add", "src/main.txt"]);
    assert!(!git(&worktree, &["diff", "--cached", "--name-only"])
        .stdout
        .is_empty());
    assert!(git(&repo.source, &["diff", "--cached", "--name-only"])
        .stdout
        .is_empty());
    git(&worktree, &["commit", "-m", "agent edit"]);
    assert_check(&repo.source, &worktree, 0, &[]);
    fs::write(repo.source.join("README.md"), "source edit\n").unwrap();
    assert_eq!(
        fs::read_to_string(worktree.join("README.md")).unwrap(),
        "original readme\n"
    );
    assert!(git(&worktree, &["status", "--porcelain"]).stdout.is_empty());
}

#[test]
fn clones_modes_symlinks_and_unusual_names_without_untracked_files() {
    let repo = Repo::new();
    let names = ["spaces in name", "雪.txt", "line\nbreak.txt"];
    for name in names {
        fs::write(repo.source.join(name), name.as_bytes()).unwrap();
    }
    fs::write(repo.source.join("src/run"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        repo.source.join("src/run"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("main.txt", repo.source.join("src/link")).unwrap();
    git(&repo.source, &["add", "."]);
    git(&repo.source, &["commit", "-m", "special files"]);
    fs::write(repo.source.join("untracked.txt"), "do not clone").unwrap();
    fs::write(repo.source.join("ignored.txt"), "do not clone").unwrap();
    let Some(worktree) = repo.add(".") else {
        return;
    };
    for name in names {
        assert_eq!(fs::read(worktree.join(name)).unwrap(), name.as_bytes());
    }
    assert_eq!(
        fs::metadata(worktree.join("src/run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert_eq!(
        fs::read_link(worktree.join("src/link")).unwrap(),
        Path::new("main.txt")
    );
    assert!(!worktree.join("untracked.txt").exists());
    assert!(!worktree.join("ignored.txt").exists());
    assert_check(&repo.source, &worktree, 0, &[]);
}

#[test]
fn scope_detects_unstaged_staged_and_committed_changes() {
    let repo = Repo::new();
    let Some(worktree) = repo.add("src") else {
        return;
    };
    fs::write(worktree.join("README.md"), "out of scope\n").unwrap();
    assert_check(&repo.source, &worktree, 1, &["README.md"]);
    git(&worktree, &["add", "README.md"]);
    assert_check(&repo.source, &worktree, 1, &["README.md"]);
    git(&worktree, &["commit", "-m", "outside scope"]);
    assert_check(&repo.source, &worktree, 1, &["README.md"]);
    success(thin(
        &repo.source,
        &["scope", "add", path(&worktree), "README.md"],
    ));
    assert_check(&repo.source, &worktree, 0, &[]);
}

#[test]
fn scope_detects_new_files_deletions_and_both_rename_sides() {
    let repo = Repo::new();
    let Some(worktree) = repo.add("src") else {
        return;
    };
    fs::write(worktree.join("outside.txt"), "new file").unwrap();
    assert_check(&repo.source, &worktree, 1, &["outside.txt"]);
    fs::remove_file(worktree.join("outside.txt")).unwrap();
    fs::remove_file(worktree.join("README.md")).unwrap();
    assert_check(&repo.source, &worktree, 1, &["README.md"]);
    git(&worktree, &["restore", "README.md"]);
    git(&worktree, &["mv", "src/main.txt", "outside.txt"]);
    assert_check(&repo.source, &worktree, 1, &["outside.txt"]);
    git(&worktree, &["reset", "--hard", "HEAD"]);
    git(&worktree, &["mv", "README.md", "src/readme.md"]);
    assert_check(&repo.source, &worktree, 1, &["README.md"]);
}

#[test]
fn tracked_ignored_files_are_checked_and_generated_outputs_are_separate() {
    let repo = Repo::new();
    fs::write(repo.source.join("ignored.txt"), "tracked despite ignore\n").unwrap();
    git(&repo.source, &["add", "-f", "ignored.txt"]);
    git(&repo.source, &["commit", "-m", "tracked ignored file"]);
    let Some(worktree) = repo.add("src") else {
        return;
    };
    fs::create_dir(worktree.join("build")).unwrap();
    fs::write(worktree.join("build/generated"), "generated bytes").unwrap();
    assert_check(&repo.source, &worktree, 0, &[]);
    fs::write(worktree.join("ignored.txt"), "tracked change\n").unwrap();
    assert_check(&repo.source, &worktree, 1, &["ignored.txt"]);
}

#[test]
fn exact_file_scope_does_not_include_siblings_and_directory_scope_expands() {
    let repo = Repo::new();
    let Some(worktree) = repo.add("src/main.txt") else {
        return;
    };
    fs::write(worktree.join("src/new.txt"), "new source file").unwrap();
    assert_check(&repo.source, &worktree, 1, &["src/new.txt"]);
    success(thin(
        &repo.source,
        &["scope", "add", path(&worktree), "src"],
    ));
    assert_check(&repo.source, &worktree, 0, &[]);
    fs::create_dir(worktree.join("src-other")).unwrap();
    fs::write(worktree.join("src-other/file"), "not in src").unwrap();
    assert_check(&repo.source, &worktree, 1, &["src-other/file"]);
}

#[test]
fn dry_runs_preserve_destination_branch_and_worktree() {
    let repo = Repo::new();
    if !repo.can_clone() {
        return;
    }
    let destination = repo.parent.join("dry-run");
    success(thin(
        &repo.source,
        &[
            "add",
            path(&destination),
            "--branch",
            "agent/dry",
            "--write",
            "src",
            "--dry-run",
            "--json",
        ],
    ));
    assert!(!destination.exists());
    assert!(git(&repo.source, &["branch", "--list", "agent/dry"])
        .stdout
        .is_empty());
    let worktree = repo.add("src").unwrap();
    success(thin(
        &repo.source,
        &["remove", path(&worktree), "--dry-run"],
    ));
    assert!(worktree.join("src/main.txt").exists());
    assert!(git(&worktree, &["status", "--porcelain"]).stdout.is_empty());
}

#[test]
fn removal_preserves_commits_and_branches_and_refuses_local_work() {
    let repo = Repo::new();
    let Some(worktree) = repo.add("src") else {
        return;
    };
    fs::write(worktree.join("src/main.txt"), "not committed\n").unwrap();
    assert!(!thin(&repo.source, &["remove", path(&worktree)])
        .status
        .success());
    assert!(worktree.exists());
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "preserved commit"]);
    let commit = git(&worktree, &["rev-parse", "HEAD"]).stdout;
    fs::write(worktree.join("untracked.txt"), "important").unwrap();
    assert!(!thin(&repo.source, &["remove", path(&worktree)])
        .status
        .success());
    fs::remove_file(worktree.join("untracked.txt")).unwrap();
    fs::write(worktree.join("ignored.txt"), "important generated output").unwrap();
    assert!(!thin(&repo.source, &["remove", path(&worktree)])
        .status
        .success());
    fs::remove_file(worktree.join("ignored.txt")).unwrap();
    success(thin(&repo.source, &["remove", path(&worktree)]));
    assert!(!worktree.exists());
    assert_eq!(
        git(&repo.source, &["rev-parse", "agent/test"]).stdout,
        commit
    );
}

#[test]
fn unrelated_history_is_rejected_without_replacing_baseline() {
    let repo = Repo::new();
    let Some(worktree) = repo.add(".") else {
        return;
    };
    git(&worktree, &["checkout", "--orphan", "unrelated"]);
    git(&worktree, &["commit", "-m", "unrelated root"]);
    let output = thin(&repo.source, &["check", path(&worktree), "--json"]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    git(&worktree, &["checkout", "agent/test"]);
    assert_check(&repo.source, &worktree, 0, &[]);
}

#[test]
fn rejects_dirty_staged_and_missing_source_files() {
    let repo = Repo::new();
    fs::write(repo.source.join("README.md"), "dirty\n").unwrap();
    repo.reject_add(&["clean", "tracked", "dirty", "unstaged"]);
    git(&repo.source, &["add", "README.md"]);
    repo.reject_add(&["clean", "tracked", "dirty", "staged"]);
    git(&repo.source, &["reset", "--hard", "HEAD"]);
    fs::remove_file(repo.source.join("src/main.txt")).unwrap();
    repo.reject_add(&["clean", "tracked", "missing", "complete", "unstaged"]);
}

#[test]
fn rejects_sparse_checkout() {
    let repo = Repo::new();
    git(&repo.source, &["sparse-checkout", "init", "--cone"]);
    git(&repo.source, &["sparse-checkout", "set", "src"]);
    repo.reject_add(&["sparse"]);
}

#[test]
fn rejects_checkout_filters_and_lfs_attributes() {
    for filter in ["custom", "lfs"] {
        let repo = Repo::new();
        fs::write(
            repo.source.join(".gitattributes"),
            format!("*.txt filter={filter}\n"),
        )
        .unwrap();
        git(&repo.source, &["add", ".gitattributes"]);
        git(&repo.source, &["commit", "-m", "unsupported filter"]);
        repo.reject_add(&["filter", "lfs"]);
    }
}

#[test]
fn rejects_submodule_tree_entries() {
    let repo = Repo::new();
    let head = git(&repo.source, &["rev-parse", "HEAD"]);
    let oid = std::str::from_utf8(&head.stdout).unwrap().trim();
    git(
        &repo.source,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{oid},vendor"),
        ],
    );
    git(&repo.source, &["commit", "-m", "submodule gitlink"]);
    fs::create_dir(repo.source.join("vendor")).unwrap();
    repo.reject_add(&["submodule", "gitlink", "160000"]);
}

#[test]
fn rejects_escaping_tracked_symlinks() {
    let repo = Repo::new();
    symlink("../../outside", repo.source.join("src/escape")).unwrap();
    git(&repo.source, &["add", "src/escape"]);
    git(&repo.source, &["commit", "-m", "escaping symlink"]);
    repo.reject_add(&["symlink", "escape", "outside"]);
}

#[test]
fn preexisting_destination_and_branch_are_never_removed() {
    let repo = Repo::new();
    let destination = repo.parent.join("rejected");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep"), "user content").unwrap();
    let output = thin(
        &repo.source,
        &[
            "add",
            path(&destination),
            "--branch",
            "agent/rejected",
            "--write",
            "src",
        ],
    );
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(destination.join("keep")).unwrap(),
        "user content"
    );
    git(&repo.source, &["branch", "agent/existing"]);
    let target = repo.parent.join("new-destination");
    let output = thin(
        &repo.source,
        &[
            "add",
            path(&target),
            "--branch",
            "agent/existing",
            "--write",
            "src",
        ],
    );
    assert!(!output.status.success());
    assert!(!git(&repo.source, &["branch", "--list", "agent/existing"])
        .stdout
        .is_empty());
    assert!(!target.exists());
}

#[test]
fn list_status_and_recovery_have_machine_readable_output() {
    let repo = Repo::new();
    let output = success(thin(&repo.source, &["--json", "list"]));
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    let output = success(thin(&repo.source, &["--json", "recover", "--dry-run"]));
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    let Some(worktree) = repo.add("src") else {
        return;
    };
    let output = success(thin(&repo.source, &["status", path(&worktree), "--json"]));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.to_string().contains("src"));
    let output = success(thin(&repo.source, &["list", "--json"]));
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("agent/test"));
}

#[cfg(feature = "fault-injection")]
fn injected_add(repo: &Repo, point: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_git-thin"))
        .current_dir(&repo.source)
        .args([
            "--json",
            "add",
            path(&repo.parent.join("fault-worktree")),
            "--branch",
            "agent/fault",
            "--write",
            "src",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_THIN_TEST_FAILPOINT", point)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap()
}

#[cfg(feature = "fault-injection")]
fn assert_only_source_registered(repo: &Repo) {
    let output = git(&repo.source, &["worktree", "list", "--porcelain"]);
    let records = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        records
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "{records}"
    );
    assert!(!repo.parent.join("fault-worktree").exists());
}

#[cfg(feature = "fault-injection")]
#[test]
fn failed_creation_and_simulated_disk_exhaustion_rollback_owned_resources() {
    for point in ["after-registration", "before-verification", "no-space"] {
        let repo = Repo::new();
        if !repo.can_clone() {
            return;
        }
        let original = fs::read(repo.source.join("src/main.txt")).unwrap();
        let output = injected_add(&repo, point);
        assert!(
            !output.status.success(),
            "fault did not fire: {}",
            describe(&output)
        );
        assert_only_source_registered(&repo);
        assert_eq!(
            fs::read(repo.source.join("src/main.txt")).unwrap(),
            original
        );
        assert!(git(&repo.source, &["status", "--porcelain"])
            .stdout
            .is_empty());
    }
}

#[cfg(feature = "fault-injection")]
#[test]
fn interrupted_creation_is_recoverable_and_recovery_is_idempotent() {
    let repo = Repo::new();
    if !repo.can_clone() {
        return;
    }
    let output = injected_add(&repo, "crash-after-registration");
    assert_eq!(output.status.code(), Some(86), "{}", describe(&output));
    let before = git(&repo.source, &["worktree", "list", "--porcelain"]).stdout;
    success(thin(&repo.source, &["recover", "--dry-run", "--json"]));
    assert_eq!(
        git(&repo.source, &["worktree", "list", "--porcelain"]).stdout,
        before
    );
    success(thin(&repo.source, &["recover", "--json"]));
    assert_only_source_registered(&repo);
    success(thin(&repo.source, &["recover", "--json"]));
    assert_only_source_registered(&repo);
    assert!(git(&repo.source, &["status", "--porcelain"])
        .stdout
        .is_empty());
}

#[cfg(feature = "fault-injection")]
#[test]
fn concurrent_source_edit_is_detected_by_content_verification() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let repo = Repo::new();
    if !repo.can_clone() {
        return;
    }
    let ready = repo.parent.join("ready");
    let resume = repo.parent.join("continue");
    let mut child = Command::new(env!("CARGO_BIN_EXE_git-thin"))
        .current_dir(&repo.source)
        .args([
            "--json",
            "add",
            path(&repo.parent.join("fault-worktree")),
            "--branch",
            "agent/fault",
            "--write",
            "src",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_THIN_TEST_FAILPOINT", "pause-before-clone")
        .env("GIT_THIN_TEST_READY_FILE", &ready)
        .env("GIT_THIN_TEST_CONTINUE_FILE", &resume)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready.exists() {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            panic!(
                "creation ended before race handshake: {}",
                describe(&output)
            );
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "creation never reached race handshake: {}",
                describe(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(
        repo.source.join("src/main.txt"),
        "concurrent source change\n",
    )
    .unwrap();
    fs::write(&resume, "continue").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "concurrent change accepted: {}",
        describe(&output)
    );
    let diagnostic = describe(&output).to_ascii_lowercase();
    assert!(
        diagnostic.contains("content")
            || diagnostic.contains("verification")
            || diagnostic.contains("match"),
        "{diagnostic}"
    );
    assert_only_source_registered(&repo);
    assert_eq!(
        fs::read_to_string(repo.source.join("src/main.txt")).unwrap(),
        "concurrent source change\n"
    );
}

#[test]
fn rejects_in_progress_rebase_without_disturbing_it() {
    let repo = Repo::new();
    git(&repo.source, &["checkout", "-b", "feature"]);
    fs::write(repo.source.join("README.md"), "feature change\n").unwrap();
    git(&repo.source, &["commit", "-am", "feature edit"]);
    git(&repo.source, &["checkout", "main"]);
    fs::write(repo.source.join("README.md"), "main change\n").unwrap();
    git(&repo.source, &["commit", "-am", "main edit"]);
    let rebase = command("git", &repo.source, &["rebase", "feature"]);
    assert!(!rebase.status.success(), "expected conflicting rebase");
    repo.reject_add(&["rebase", "operation in progress"]);
    assert!(
        repo.source.join(".git/rebase-merge").exists()
            || repo.source.join(".git/rebase-apply").exists()
    );
    git(&repo.source, &["rebase", "--abort"]);
    assert!(git(&repo.source, &["status", "--porcelain"])
        .stdout
        .is_empty());
}

#[test]
fn rejects_unsafe_scopes_without_creating_resources() {
    let repo = Repo::new();
    for scope in ["../outside", "/absolute", "src/../../outside", "src/*.txt"] {
        let destination = repo.parent.join("invalid-scope");
        let output = thin(
            &repo.source,
            &[
                "add",
                path(&destination),
                "--branch",
                "agent/invalid-scope",
                "--write",
                scope,
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(2),
            "accepted unsafe scope {scope:?}: {}",
            describe(&output)
        );
        assert!(!destination.exists());
        assert!(
            git(&repo.source, &["branch", "--list", "agent/invalid-scope"])
                .stdout
                .is_empty()
        );
    }
}

#[test]
fn unsupported_clone_filesystem_never_falls_back_to_a_checkout() {
    let repo = Repo::new();
    let unsupported = match git_thin::backend::probe(&repo.source, &repo.parent) {
        Ok(()) => return,
        Err(error) if error.contains("native copy-on-write cloning is unsupported here") => error,
        Err(error) => panic!("unexpected probe failure: {error}"),
    };
    let destination = repo.parent.join("unsupported");
    let output = thin(
        &repo.source,
        &[
            "add",
            path(&destination),
            "--branch",
            "agent/unsupported",
            "--write",
            "src",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(2),
        "unsupported backend accepted ({unsupported}): {}",
        describe(&output)
    );
    assert!(!destination.exists());
    assert!(
        git(&repo.source, &["branch", "--list", "agent/unsupported"])
            .stdout
            .is_empty()
    );
}

#[test]
fn cross_volume_creation_is_rejected_before_registration_when_configured() {
    let Some(other_volume) = std::env::var_os("GIT_THIN_CROSS_VOLUME_ROOT") else {
        eprintln!("SKIP cross-volume integration: set GIT_THIN_CROSS_VOLUME_ROOT to a writable directory on a different volume");
        return;
    };
    let repo = Repo::new();
    let temp = tempfile::tempdir_in(other_volume).unwrap();
    let parent = temp.path().canonicalize().unwrap();
    assert_ne!(fs::metadata(&repo.source).unwrap().dev(), fs::metadata(&parent).unwrap().dev(), "GIT_THIN_CROSS_VOLUME_ROOT must be on a different volume from the test temporary directory");
    let destination = parent.join("cross-volume");
    let output = thin(
        &repo.source,
        &[
            "add",
            path(&destination),
            "--branch",
            "agent/cross-volume",
            "--write",
            "src",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    let text = describe(&output).to_ascii_lowercase();
    assert!(
        text.contains("volume") || text.contains("filesystem"),
        "{text}"
    );
    assert!(!destination.exists());
    assert!(
        git(&repo.source, &["branch", "--list", "agent/cross-volume"])
            .stdout
            .is_empty()
    );
}

#[test]
fn scope_reports_unicode_spaces_and_newlines_as_distinct_paths() {
    let repo = Repo::new();
    let Some(worktree) = repo.add("src") else {
        return;
    };
    let names = ["out of scope.txt", "雪.txt", "line\nbreak.txt"];
    for name in names {
        fs::write(worktree.join(name), "outside scope").unwrap();
    }
    assert_check(&repo.source, &worktree, 1, &names);
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "unusual outside paths"]);
    assert_check(&repo.source, &worktree, 1, &names);
    for name in names {
        success(thin(&repo.source, &["scope", "add", path(&worktree), name]));
    }
    assert_check(&repo.source, &worktree, 0, &[]);
}

#[test]
fn non_utf8_source_paths_are_explicitly_rejected() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let repo = Repo::new();
    let name = OsString::from_vec(vec![b'f', 0xff]);
    fs::write(repo.source.join(name), "non UTF-8 filename").unwrap();
    git(&repo.source, &["add", "."]);
    git(&repo.source, &["commit", "-m", "unsupported path encoding"]);
    repo.reject_add(&["non-utf-8", "utf8", "utf-8"]);
}

#[test]
fn removal_preserves_detached_commits_until_a_local_branch_retains_them() {
    let repo = Repo::new();
    let Some(worktree) = repo.add("src") else {
        return;
    };
    git(&worktree, &["checkout", "--detach"]);
    fs::write(worktree.join("src/main.txt"), "detached committed work\n").unwrap();
    git(&worktree, &["commit", "-am", "detached edit"]);
    let commit = git(&worktree, &["rev-parse", "HEAD"]).stdout;
    let output = thin(&repo.source, &["remove", path(&worktree)]);
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    assert!(
        describe(&output).contains("local branch"),
        "{}",
        describe(&output)
    );
    assert!(worktree.exists());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).stdout, commit);
    git(&worktree, &["branch", "rescue-detached"]);
    success(thin(&repo.source, &["remove", path(&worktree)]));
    assert!(!worktree.exists());
    assert_eq!(
        git(&repo.source, &["rev-parse", "rescue-detached"]).stdout,
        commit
    );
}

#[test]
fn unsupported_source_filters_are_rejected_without_execution() {
    let repo = Repo::new();
    fs::write(
        repo.source.join(".gitattributes"),
        "src/main.txt filter=custom\n",
    )
    .unwrap();
    git(&repo.source, &["add", ".gitattributes"]);
    git(
        &repo.source,
        &["commit", "-m", "declare unsupported filter"],
    );
    let marker = repo.parent.join("filter-ran");
    let script = format!("touch '{}'; cat", marker.display());
    git(&repo.source, &["config", "filter.custom.clean", &script]);
    // Force Git to inspect contents instead of trusting the original stat cache.
    fs::write(repo.source.join("src/main.txt"), "changed bytes\n").unwrap();
    repo.reject_add(&["checkout attribute", "filter"]);
    assert!(
        !marker.exists(),
        "preflight executed an unsupported clean filter"
    );
}
