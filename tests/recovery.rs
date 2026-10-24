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
        fs::write(source.join("empty"), b"").unwrap();
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
            schema_version: 2,
            id,
            creation_commit: commit,
            branch: "agent/fixture".into(),
            backend: "test-fixture-ordinary-git-checkout".into(),
            scope: vec![ScopeEntry {
                path: ".".into(),
                directory: true,
            }],
            state: if state == "creating" && !no_checkout {
                "verifying"
            } else {
                state
            }
            .into(),
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
    assert_eq!(fixture.record().state, "verifying");
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

fn assert_staged_deletion_preserved(state: &str) {
    let mut fixture = Fixture::new(state, false);
    // Exercise the exact legacy journal states from the original data-loss bug.
    fixture.metadata.schema_version = 1;
    fixture.metadata.state = state.into();
    fixture.save();
    git(&fixture.active_path, &["rm", "--cached", "README.md"]);
    let staged = git(&fixture.active_path, &["diff", "--cached", "--name-status"]);
    assert!(staged.contains("D\tREADME.md"));
    let preview = fixture.thin(&["recover", "--dry-run", "--json"]);
    let result = fixture.thin(&["recover", "--json"]);
    println!("{state} preview: {}", diagnostic(&preview));
    println!("{state} recovery: {}", diagnostic(&result));
    assert_eq!(
        preview.status.code(),
        Some(2),
        "staged deletion must prevent cleanup preview"
    );
    assert_eq!(
        result.status.code(),
        Some(2),
        "staged deletion must prevent cleanup"
    );
    assert_eq!(
        git(&fixture.active_path, &["diff", "--cached", "--name-status"]),
        staged
    );
    assert_eq!(
        fs::read(fixture.active_path.join("README.md")).unwrap(),
        b"readme\n"
    );
    assert!(fixture.metadata_file.exists());
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
}

#[test]
fn recovery_preserves_staged_deletion_creating() {
    assert_staged_deletion_preserved("creating");
}

#[test]
fn recovery_preserves_staged_deletion_publishing() {
    assert_staged_deletion_preserved("publishing");
}

#[derive(Debug, PartialEq, Eq)]
struct PreservedState {
    contents: Vec<(PathBuf, Vec<u8>, u32, u64)>,
    index: Option<Vec<u8>>,
    head: String,
    refs: String,
    registrations: String,
    journal: Vec<u8>,
    identity: (u64, u64),
}

fn snapshot(fixture: &Fixture) -> PreservedState {
    fn walk(root: &Path, relative: &Path, result: &mut Vec<(PathBuf, Vec<u8>, u32, u64)>) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let path = relative.join(entry.unwrap().file_name());
            let metadata = fs::symlink_metadata(root.join(&path)).unwrap();
            if metadata.is_dir() {
                walk(root, &path, result);
            } else {
                let contents = if metadata.file_type().is_symlink() {
                    fs::read_link(root.join(&path))
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec()
                } else {
                    fs::read(root.join(&path)).unwrap()
                };
                result.push((path, contents, metadata.mode(), metadata.ino()));
            }
        }
    }
    let mut contents = Vec::new();
    walk(&fixture.active_path, Path::new(""), &mut contents);
    contents.sort();
    let identity = fs::symlink_metadata(&fixture.active_path).unwrap();
    let index = git(
        &fixture.active_path,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    );
    PreservedState {
        contents,
        index: fs::read(index).ok(),
        head: git(&fixture.active_path, &["rev-parse", "HEAD"]),
        refs: git(&fixture.source, &["show-ref"]),
        registrations: git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]),
        journal: fs::read(&fixture.metadata_file).unwrap(),
        identity: (identity.dev(), identity.ino()),
    }
}

#[test]
fn initialized_indexes_preserve_all_forms_of_staged_deletion_and_edits() {
    for state in ["verifying", "publishing"] {
        for edit in [
            "cached-one",
            "cached-all",
            "empty",
            "rm",
            "rename",
            "mixed",
            "missing-index",
            "unstaged-delete",
            "intent-to-add-empty",
        ] {
            let fixture = Fixture::new(state, false);
            match edit {
                "cached-one" => {
                    git(&fixture.active_path, &["rm", "--cached", "README.md"]);
                }
                "cached-all" => {
                    git(&fixture.active_path, &["rm", "--cached", "-r", "."]);
                }
                "empty" => {
                    git(&fixture.active_path, &["rm", "--cached", "empty"]);
                }
                "rm" => {
                    git(&fixture.active_path, &["rm", "README.md"]);
                }
                "rename" => {
                    git(&fixture.active_path, &["mv", "README.md", "renamed"]);
                }
                "mixed" => {
                    git(&fixture.active_path, &["rm", "--cached", "README.md"]);
                    fs::write(
                        fixture.active_path.join("src/main.txt"),
                        "valuable unstaged change\n",
                    )
                    .unwrap();
                }
                "missing-index" => {
                    let index = git(
                        &fixture.active_path,
                        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
                    );
                    fs::remove_file(index).unwrap();
                }
                "unstaged-delete" => {
                    fs::remove_file(fixture.active_path.join("README.md")).unwrap();
                }
                "intent-to-add-empty" => {
                    git(&fixture.active_path, &["rm", "--cached", "empty"]);
                    git(&fixture.active_path, &["add", "-N", "empty"]);
                    assert!(
                        git(&fixture.active_path, &["diff", "--cached", "--name-status"])
                            .contains("D\tempty")
                    );
                }
                _ => unreachable!(),
            }
            let before = snapshot(&fixture);
            for args in [
                ["recover", "--dry-run", "--json"].as_slice(),
                ["recover", "--json"].as_slice(),
                ["recover", "--json"].as_slice(),
            ] {
                let output = fixture.thin(args);
                assert_eq!(
                    output.status.code(),
                    Some(2),
                    "{state}/{edit}: {}",
                    diagnostic(&output)
                );
                assert_eq!(snapshot(&fixture), before, "{state}/{edit}");
            }
        }
    }
}

#[test]
fn ambiguous_legacy_creating_and_index_transition_records_are_preserved() {
    for (schema, state, no_checkout) in [
        (1, "creating", true),
        (1, "creating", false),
        (2, "indexing", true),
        (2, "indexing", false),
    ] {
        let mut fixture = Fixture::new(state, no_checkout);
        fixture.metadata.schema_version = schema;
        fixture.metadata.state = state.into();
        fixture.save();
        let before = snapshot(&fixture);
        for args in [
            vec!["recover", "--dry-run"],
            vec!["recover"],
            vec!["recover"],
        ] {
            let result = fixture.thin(&args);
            assert_eq!(result.status.code(), Some(2), "{}", diagnostic(&result));
            assert!(diagnostic(&result).contains("ambiguous index initialization"));
            assert_eq!(snapshot(&fixture), before);
        }
    }
}

#[test]
fn schema_one_ready_worktrees_remain_usable_without_reinterpretation() {
    let mut fixture = Fixture::new("ready", false);
    fixture.metadata.schema_version = 1;
    fixture.save();
    success(fixture.thin(&["check", text(&fixture.active_path)]));
    success(fixture.thin(&["status", text(&fixture.active_path)]));
    success(fixture.thin(&["scope", "add", text(&fixture.active_path), "future/"]));
    assert_eq!(fixture.record().schema_version, 1);
    success(fixture.thin(&["remove", text(&fixture.active_path)]));
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
}

#[test]
fn known_pre_index_partial_clone_can_recover_but_an_appeared_index_cannot() {
    let fixture = Fixture::new("creating", true);
    fs::write(fixture.active_path.join("README.md"), b"readme\n").unwrap();
    let before = snapshot(&fixture);
    success(fixture.thin(&["recover", "--dry-run"]));
    assert_eq!(snapshot(&fixture), before);
    success(fixture.thin(&["recover"]));
    assert!(!fixture.active_path.exists());
    fixture.assert_branch_preserved(&fixture.metadata.creation_commit);

    let fixture = Fixture::new("creating", true);
    git(
        &fixture.active_path,
        &["read-tree", &fixture.metadata.creation_commit],
    );
    let before = snapshot(&fixture);
    let result = fixture.thin(&["recover"]);
    assert_eq!(result.status.code(), Some(2));
    assert!(diagnostic(&result).contains("index appeared"));
    assert_eq!(snapshot(&fixture), before);
}

#[test]
fn malformed_unknown_schema_and_unknown_state_block_all_records_before_cleanup() {
    for kind in ["truncated", "schema", "state"] {
        let fixture = Fixture::new("creating", true);
        let before = snapshot(&fixture);
        let mut bad = fixture.metadata.clone();
        bad.id = uuid::Uuid::new_v4().to_string();
        // Sort after the valid record, so validation must precede any action.
        bad.path = fixture.metadata.path.parent().unwrap().join("zz-malformed");
        match kind {
            "schema" => bad.schema_version = 777,
            "state" => bad.state = "unknown-future-state".into(),
            _ => (),
        }
        let bad_path = fixture
            .metadata_file
            .parent()
            .unwrap()
            .join(format!("{}.json", bad.id));
        let bytes = if kind == "truncated" {
            b"{\"schema_version\":".to_vec()
        } else {
            serde_json::to_vec(&bad).unwrap()
        };
        fs::write(&bad_path, &bytes).unwrap();
        for args in [
            vec!["recover", "--dry-run"],
            vec!["recover"],
            vec!["recover"],
        ] {
            let result = fixture.thin(&args);
            assert_eq!(
                result.status.code(),
                Some(2),
                "{kind}: {}",
                diagnostic(&result)
            );
            assert!(
                diagnostic(&result).contains("manual") || diagnostic(&result).contains("manually")
            );
            assert_eq!(snapshot(&fixture), before);
            assert_eq!(fs::read(&bad_path).unwrap(), bytes);
        }
    }
}

#[test]
fn recovery_preserves_changes_added_after_removal_intent() {
    let fixture = Fixture::new("removing", false);
    git(&fixture.active_path, &["rm", "--cached", "README.md"]);
    let before = snapshot(&fixture);
    for args in [
        vec!["recover", "--dry-run"],
        vec!["recover"],
        vec!["recover"],
    ] {
        assert_eq!(fixture.thin(&args).status.code(), Some(2));
        assert_eq!(snapshot(&fixture), before);
    }
}

#[test]
fn replaced_and_moved_staging_directories_are_retained_with_their_journals() {
    for replace in [false, true] {
        let fixture = Fixture::new("creating", true);
        let moved = fixture.active_path.with_file_name("retained-original");
        git(
            &fixture.source,
            &["worktree", "move", text(&fixture.active_path), text(&moved)],
        );
        let registration = git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]);
        let journal = fs::read(&fixture.metadata_file).unwrap();
        let identity = fs::symlink_metadata(&moved).unwrap().ino();
        if replace {
            fs::create_dir(&fixture.active_path).unwrap();
            fs::write(
                fixture.active_path.join("unrelated"),
                b"not owned by git-thin\n",
            )
            .unwrap();
        }
        for args in [
            vec!["recover", "--dry-run"],
            vec!["recover"],
            vec!["recover"],
        ] {
            let output = fixture.thin(&args);
            assert_eq!(output.status.code(), Some(2), "{}", diagnostic(&output));
            assert!(diagnostic(&output).contains(if replace {
                "identity changed"
            } else {
                "moved or is orphaned"
            }));
            assert_eq!(fs::read(&fixture.metadata_file).unwrap(), journal);
            assert_eq!(fs::symlink_metadata(&moved).unwrap().ino(), identity);
            assert_eq!(
                git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]),
                registration
            );
            if replace {
                assert_eq!(
                    fs::read(fixture.active_path.join("unrelated")).unwrap(),
                    b"not owned by git-thin\n"
                );
            }
            fixture.assert_branch_preserved(&fixture.metadata.creation_commit);
        }
    }
}

#[test]
fn inaccessible_recorded_paths_never_become_missing_resources() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("UNAVAILABLE: inaccessible-parent recovery requires an unprivileged test user");
        return;
    }
    for state in ["creating", "removing", "publishing"] {
        let mut fixture = Fixture::new(state, state == "creating");
        git(
            &fixture.source,
            &["worktree", "remove", "--force", text(&fixture.active_path)],
        );
        let parent = fixture.source.parent().unwrap().join("inaccessible-parent");
        fs::create_dir(&parent).unwrap();
        let recorded = parent.join("retained");
        fs::create_dir(&recorded).unwrap();
        fs::write(recorded.join("valuable"), b"keep this local work\n").unwrap();
        let metadata = fs::metadata(&recorded).unwrap();
        fixture.metadata.device = metadata.dev();
        fixture.metadata.inode = metadata.ino();
        if state == "creating" {
            fixture.metadata.staging_path = recorded.clone();
        } else {
            fixture.metadata.path = recorded.clone();
        }
        fixture.save();
        let journal = fs::read(&fixture.metadata_file).unwrap();
        let refs = git(&fixture.source, &["show-ref"]);
        let registrations = git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]);
        let permissions = fs::metadata(&parent).unwrap().permissions();
        for args in [
            vec!["recover", "--dry-run"],
            vec!["recover"],
            vec!["recover"],
        ] {
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
            let result = fixture.thin(&args);
            // Restore before assertions so panic cannot leave the fixture locked.
            fs::set_permissions(&parent, permissions.clone()).unwrap();
            assert_eq!(
                result.status.code(),
                Some(2),
                "{state}: {}",
                diagnostic(&result)
            );
            assert!(diagnostic(&result).contains("cannot inspect path"));
            assert_eq!(fs::read(&fixture.metadata_file).unwrap(), journal);
            assert_eq!(
                fs::read(recorded.join("valuable")).unwrap(),
                b"keep this local work\n"
            );
            assert_eq!(fs::metadata(&recorded).unwrap().ino(), metadata.ino());
            assert_eq!(git(&fixture.source, &["show-ref"]), refs);
            assert_eq!(
                git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]),
                registrations
            );
        }
    }
    eprintln!(
        "EXECUTED real inaccessible-parent EACCES recovery as uid {}",
        unsafe { libc::geteuid() }
    );
}

#[test]
fn moved_removal_registrations_preserve_work_after_branch_changes() {
    for head_change in ["unchanged", "rename", "detach"] {
        let mut fixture = Fixture::new("removing", false);
        let moved = fixture
            .active_path
            .with_file_name("moved-removal-with-work");
        git(
            &fixture.source,
            &["worktree", "move", text(&fixture.active_path), text(&moved)],
        );
        // The journal retains the original destination; snapshots follow the
        // actual directory so every retained resource can be compared.
        fixture.active_path = moved;
        match head_change {
            "rename" => {
                git(&fixture.active_path, &["branch", "-m", "agent/renamed"]);
            }
            "detach" => {
                git(&fixture.active_path, &["checkout", "--detach"]);
            }
            _ => {}
        }
        fs::write(fixture.active_path.join("README.md"), b"staged work\n").unwrap();
        git(&fixture.active_path, &["add", "README.md"]);
        fs::write(fixture.active_path.join("README.md"), b"unstaged work\n").unwrap();
        fs::write(fixture.active_path.join("untracked"), b"new work\n").unwrap();
        fs::create_dir(fixture.active_path.join("build")).unwrap();
        fs::write(fixture.active_path.join("build/output"), b"ignored work\n").unwrap();
        let before = snapshot(&fixture);
        for args in [
            vec!["recover", "--dry-run"],
            vec!["recover"],
            vec!["recover"],
        ] {
            let result = fixture.thin(&args);
            assert_eq!(
                result.status.code(),
                Some(2),
                "{head_change}: {}",
                diagnostic(&result)
            );
            assert!(diagnostic(&result).contains("moved or is orphaned"));
            assert_eq!(snapshot(&fixture), before, "{head_change}");
        }
    }
}

#[test]
fn moved_staging_registrations_survive_branch_renaming_and_detachment() {
    for head_change in ["rename", "detach"] {
        let fixture = Fixture::new("publishing", false);
        let moved = fixture.active_path.with_file_name("moved-with-work");
        git(
            &fixture.source,
            &["worktree", "move", text(&fixture.active_path), text(&moved)],
        );
        if head_change == "rename" {
            git(&moved, &["branch", "-m", "agent/renamed"]);
        } else {
            git(&moved, &["checkout", "--detach"]);
        }
        let journal = fs::read(&fixture.metadata_file).unwrap();
        let registrations = git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]);
        let refs = git(&fixture.source, &["show-ref"]);
        let index = git(
            &moved,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        );
        let index_bytes = fs::read(&index).unwrap();
        let identity = fs::symlink_metadata(&moved).unwrap().ino();
        for args in [
            vec!["recover", "--dry-run"],
            vec!["recover"],
            vec!["recover"],
        ] {
            let result = fixture.thin(&args);
            assert_eq!(
                result.status.code(),
                Some(2),
                "{head_change}: {}",
                diagnostic(&result)
            );
            assert!(diagnostic(&result).contains("moved or is orphaned"));
            assert_eq!(fs::read(&fixture.metadata_file).unwrap(), journal);
            assert_eq!(fs::read(moved.join("README.md")).unwrap(), b"readme\n");
            assert_eq!(fs::read(&index).unwrap(), index_bytes);
            assert_eq!(fs::symlink_metadata(&moved).unwrap().ino(), identity);
            assert_eq!(git(&fixture.source, &["show-ref"]), refs);
            assert_eq!(
                git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]),
                registrations
            );
        }
    }
}
