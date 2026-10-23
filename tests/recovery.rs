//! Lifecycle tests that run on any filesystem supported by ordinary Git.
//!
//! Fixtures use `git worktree add` and explicit journal records. They validate
//! removal and crash recovery, NOT native cloning or physical storage sharing.
#![cfg(unix)]

use git_thin::lifecycle::Metadata;
use git_thin::scope::ScopeEntry;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    source: PathBuf,
    active_path: PathBuf,
    metadata_file: PathBuf,
    metadata: Metadata,
}

fn run(program: &str, directory: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap()
}

fn diagnostic(output: &Output) -> String {
    format!(
        "exit {:?}; stdout: {}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn success(output: Output) -> Output {
    assert!(output.status.success(), "{}", diagnostic(&output));
    output
}

fn git(directory: &Path, args: &[&str]) -> String {
    String::from_utf8(success(run("git", directory, args)).stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

impl Fixture {
    fn new(state: &str, no_checkout: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let source = parent.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "user.name", "Git Thin Tests"]);
        git(&source, &["config", "user.email", "tests@example.invalid"]);
        git(&source, &["config", "core.autocrlf", "false"]);
        fs::create_dir(source.join("src")).unwrap();
        fs::write(source.join("src/main.txt"), "original\n").unwrap();
        fs::write(source.join("README.md"), "readme\n").unwrap();
        fs::write(source.join(".gitignore"), "build/\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "baseline"]);
        let commit = git(&source, &["rev-parse", "HEAD"]);
        let id = uuid::Uuid::new_v4().to_string();
        let staging_path = parent.join(format!(".git-thin-{id}"));
        let destination = parent.join("managed");
        let active_path = if state == "ready" || state == "removing" {
            destination.clone()
        } else {
            staging_path.clone()
        };
        let mut args = vec!["worktree", "add"];
        if no_checkout {
            args.push("--no-checkout");
        }
        args.extend(["-b", "agent/fixture", text(&active_path), &commit]);
        git(&source, &args);
        let identity = fs::symlink_metadata(&active_path).unwrap();
        let metadata = Metadata {
            schema_version: 1,
            id,
            creation_commit: commit,
            branch: "agent/fixture".into(),
            backend: "test-fixture-ordinary-git-checkout".into(),
            scope: vec![ScopeEntry {
                path: ".".into(),
                directory: true,
            }],
            state: state.into(),
            path: destination,
            staging_path,
            device: identity.dev(),
            inode: identity.ino(),
            clone_ms: 0,
            verification_ms: 0,
        };
        let common = git(
            &source,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        );
        let journal = PathBuf::from(common).join("git-thin");
        fs::create_dir(&journal).unwrap();
        let metadata_file = journal.join(format!("{}.json", metadata.id));
        let fixture = Self {
            _temp: temp,
            source,
            active_path,
            metadata_file,
            metadata,
        };
        fixture.save();
        fixture
    }

    fn save(&self) {
        fs::write(
            &self.metadata_file,
            serde_json::to_vec_pretty(&self.metadata).unwrap(),
        )
        .unwrap();
    }

    fn thin(&self, args: &[&str]) -> Output {
        run(env!("CARGO_BIN_EXE_git-thin"), &self.source, args)
    }

    fn record(&self) -> Metadata {
        serde_json::from_slice(&fs::read(&self.metadata_file).unwrap()).unwrap()
    }

    fn assert_branch_preserved(&self, expected: &str) {
        assert_eq!(git(&self.source, &["rev-parse", "agent/fixture"]), expected);
    }

    fn assert_only_source_registered(&self) {
        let records = git(&self.source, &["worktree", "list", "--porcelain"]);
        assert_eq!(
            records
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1,
            "{records}"
        );
    }
}

#[test]
fn removing_clean_managed_worktree_preserves_commits_and_branch() {
    let fixture = Fixture::new("ready", false);
    fs::write(fixture.active_path.join("src/main.txt"), "saved work\n").unwrap();
    git(&fixture.active_path, &["commit", "-am", "saved work"]);
    let commit = git(&fixture.active_path, &["rev-parse", "HEAD"]);
    success(fixture.thin(&["remove", text(&fixture.active_path), "--dry-run"]));
    assert!(fixture.active_path.exists());
    assert!(fixture.metadata_file.exists());
    success(fixture.thin(&["remove", text(&fixture.active_path)]));
    assert!(!fixture.active_path.exists());
    assert!(!fixture.metadata_file.exists());
    fixture.assert_branch_preserved(&commit);
    fixture.assert_only_source_registered();
}

#[test]
fn removal_refuses_untracked_ignored_and_uncommitted_work() {
    let fixture = Fixture::new("ready", false);
    for relative in ["untracked.txt", "build/generated.txt", "src/main.txt"] {
        let path = fixture.active_path.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "valuable local work\n").unwrap();
        let output = fixture.thin(&["remove", text(&fixture.active_path)]);
        assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
        assert_eq!(fs::read_to_string(&path).unwrap(), "valuable local work\n");
        assert_eq!(fixture.record().state, "ready");
        fs::remove_file(&path).unwrap();
        if relative.starts_with("build/") {
            fs::remove_dir(fixture.active_path.join("build")).unwrap();
        }
    }
    git(&fixture.active_path, &["restore", "src/main.txt"]);
    success(fixture.thin(&["remove", text(&fixture.active_path)]));
}

#[test]
fn removal_refuses_unreferenced_detached_commit_until_rescued() {
    let fixture = Fixture::new("ready", false);
    git(&fixture.active_path, &["checkout", "--detach"]);
    fs::write(fixture.active_path.join("src/main.txt"), "detached work\n").unwrap();
    git(&fixture.active_path, &["commit", "-am", "detached work"]);
    let commit = git(&fixture.active_path, &["rev-parse", "HEAD"]);
    let output = fixture.thin(&["remove", text(&fixture.active_path)]);
    assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
    assert!(diagnostic(&output).contains("local branch"));
    assert_eq!(git(&fixture.active_path, &["rev-parse", "HEAD"]), commit);
    git(&fixture.active_path, &["branch", "rescued"]);
    success(fixture.thin(&["remove", text(&fixture.active_path)]));
    assert_eq!(git(&fixture.source, &["rev-parse", "rescued"]), commit);
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
}

#[test]
fn recovery_removes_incomplete_no_checkout_stage_and_is_idempotent() {
    let fixture = Fixture::new("creating", true);
    assert!(!fixture.active_path.join("src/main.txt").exists());
    let original_registration = git(&fixture.source, &["worktree", "list", "--porcelain"]);
    success(fixture.thin(&["recover", "--dry-run"]));
    assert_eq!(
        git(&fixture.source, &["worktree", "list", "--porcelain"]),
        original_registration
    );
    assert!(fixture.metadata_file.exists());
    success(fixture.thin(&["recover"]));
    assert!(!fixture.active_path.exists());
    assert!(!fixture.metadata_file.exists());
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
    fixture.assert_only_source_registered();
    success(fixture.thin(&["recover"]));
    fixture.assert_only_source_registered();
}

#[test]
fn recovery_preserves_unknown_files_in_interrupted_stage() {
    let fixture = Fixture::new("creating", true);
    let local = fixture.active_path.join("local-notes.txt");
    fs::write(&local, "notes added after interruption\n").unwrap();
    for flags in [vec!["recover", "--dry-run"], vec!["recover"]] {
        let output = fixture.thin(&flags);
        assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
        assert!(diagnostic(&output).contains("unexpected file"));
        assert_eq!(
            fs::read_to_string(&local).unwrap(),
            "notes added after interruption\n"
        );
        assert!(fixture.metadata_file.exists());
        assert!(fixture.active_path.join(".git").exists());
    }
}

#[test]
fn recovery_refuses_changed_directory_identity() {
    let mut fixture = Fixture::new("creating", true);
    fixture.metadata.inode = fixture.metadata.inode.wrapping_add(1);
    fixture.save();
    let output = fixture.thin(&["recover"]);
    assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
    assert!(diagnostic(&output).contains("identity changed"));
    assert!(fixture.metadata_file.exists());
    assert!(fixture.active_path.join(".git").exists());
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
}

#[test]
fn recovery_preserves_modified_tracked_file_in_interrupted_stage() {
    let fixture = Fixture::new("creating", false);
    fs::write(
        fixture.active_path.join("src/main.txt"),
        "post-crash edit\n",
    )
    .unwrap();
    let output = fixture.thin(&["recover"]);
    assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
    assert!(diagnostic(&output).contains("staging contents changed"));
    assert_eq!(
        fs::read_to_string(fixture.active_path.join("src/main.txt")).unwrap(),
        "post-crash edit\n"
    );
    assert!(fixture.metadata_file.exists());
    assert_eq!(fixture.record().state, "creating");
}

#[test]
fn recovery_repairs_publication_interrupted_after_native_rename() {
    let fixture = Fixture::new("publishing", false);
    git_thin::backend::rename_directory_noreplace(
        &fixture.metadata.staging_path,
        &fixture.metadata.path,
    )
    .unwrap();
    assert!(!fixture.metadata.staging_path.exists());
    assert!(fixture.metadata.path.join("src/main.txt").exists());
    let old_registration = git(&fixture.source, &["worktree", "list", "--porcelain"]);
    assert!(old_registration.contains(text(&fixture.metadata.staging_path)));
    success(fixture.thin(&["recover", "--dry-run"]));
    assert_eq!(fixture.record().state, "publishing");
    assert_eq!(
        git(&fixture.source, &["worktree", "list", "--porcelain"]),
        old_registration
    );
    success(fixture.thin(&["recover"]));
    assert_eq!(fixture.record().state, "ready");
    let registration = git(&fixture.source, &["worktree", "list", "--porcelain"]);
    assert!(registration.contains(text(&fixture.metadata.path)));
    assert!(!registration.contains(text(&fixture.metadata.staging_path)));
    assert!(git(&fixture.metadata.path, &["status", "--porcelain"]).is_empty());
    success(fixture.thin(&["check", text(&fixture.metadata.path)]));
    success(fixture.thin(&["remove", text(&fixture.metadata.path)]));
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
}

#[test]
fn recovery_completes_interrupted_clean_removal() {
    let fixture = Fixture::new("removing", false);
    success(fixture.thin(&["recover", "--dry-run"]));
    assert!(fixture.active_path.exists());
    success(fixture.thin(&["recover"]));
    assert!(!fixture.active_path.exists());
    assert!(!fixture.metadata_file.exists());
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
    fixture.assert_only_source_registered();
}

#[test]
fn recovery_preserves_staged_content_when_working_file_was_restored() {
    let fixture = Fixture::new("creating", false);
    let path = fixture.active_path.join("README.md");
    fs::write(&path, "valuable staged content\n").unwrap();
    git(&fixture.active_path, &["add", "README.md"]);
    fs::write(&path, "readme\n").unwrap();
    let output = fixture.thin(&["recover", "--json"]);
    assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
    assert!(diagnostic(&output).contains("staged work differs"));
    assert_eq!(
        git(&fixture.active_path, &["show", ":README.md"]),
        "valuable staged content"
    );
    assert!(fixture.metadata_file.exists());
}

#[test]
fn missing_directory_with_orphan_registration_retains_recovery_journal() {
    let fixture = Fixture::new("removing", false);
    fs::remove_dir_all(&fixture.active_path).unwrap();
    let output = fixture.thin(&["recover", "--json"]);
    assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
    assert!(diagnostic(&output).contains("still has a Git registration"));
    assert!(fixture.metadata_file.exists());
    git(
        &fixture.source,
        &["worktree", "remove", "--force", text(&fixture.active_path)],
    );
    success(fixture.thin(&["recover", "--json"]));
    assert!(!fixture.metadata_file.exists());
    fixture.assert_only_source_registered();
}
