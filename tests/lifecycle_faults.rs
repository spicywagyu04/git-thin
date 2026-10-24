//! Real native-clone operations stopped at durable lifecycle boundaries.
//! Injected errors/exit(86) are labeled separately from real ENOSPC coverage.
#![cfg(all(unix, feature = "fault-injection"))]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn diagnostic(output: &Output) -> String {
    format!(
        "{:?}: {} {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn success(output: Output) -> Output {
    assert!(output.status.success(), "{}", diagnostic(&output));
    output
}

fn command(program: &str, root: &Path) -> Command {
    let mut result = Command::new(program);
    result
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_THIN_TEST_FAILPOINT",
    ] {
        result.env_remove(key);
    }
    result
}

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    success(command("git", root).args(args).output().unwrap()).stdout
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

struct Fixture {
    _temp: TempDir,
    parent: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let source = parent.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "user.name", "Lifecycle Tests"]);
        git(&source, &["config", "user.email", "tests@example.invalid"]);
        git(&source, &["config", "core.autocrlf", "false"]);
        fs::write(source.join("a"), b"first clone\n").unwrap();
        fs::write(source.join("b"), b"second clone\n").unwrap();
        fs::write(source.join("empty"), b"").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "baseline"]);
        Self {
            destination: parent.join("managed"),
            parent,
            source,
            _temp: temp,
        }
    }

    fn thin_command(&self, args: &[&str]) -> Command {
        let mut result = command(env!("CARGO_BIN_EXE_git-thin"), &self.source);
        result.arg("--json").args(args);
        result
    }

    fn thin(&self, args: &[&str]) -> Output {
        self.thin_command(args).output().unwrap()
    }

    fn native(&self) -> bool {
        let result = self.thin(&["doctor"]);
        if result.status.success() {
            return true;
        }
        assert_ne!(
            std::env::var("GIT_THIN_REQUIRE_CLONE").as_deref(),
            Ok("1"),
            "required native cloning unavailable: {}",
            diagnostic(&result)
        );
        eprintln!(
            "UNAVAILABLE: native lifecycle fault tests: {}",
            diagnostic(&result)
        );
        false
    }

    fn add(&self, failpoint: &str) -> Output {
        self.thin_command(&[
            "add",
            text(&self.destination),
            "--branch",
            "agent/fault",
            "--write",
            ".",
        ])
        .env("GIT_THIN_TEST_FAILPOINT", failpoint)
        .output()
        .unwrap()
    }

    fn journals(&self) -> Vec<PathBuf> {
        let path = self.source.join(".git/git-thin");
        if !path.exists() {
            return Vec::new();
        }
        let mut files: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        files
    }

    fn record(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.journals()[0]).unwrap()).unwrap()
    }

    fn stages(&self) -> Vec<PathBuf> {
        let mut result: Vec<_> = fs::read_dir(&self.parent)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(".git-thin-")
            })
            .collect();
        result.sort();
        result
    }

    fn refs(&self) -> Vec<u8> {
        git(&self.source, &["show-ref"])
    }
    fn registrations(&self) -> Vec<u8> {
        git(&self.source, &["worktree", "list", "--porcelain", "-z"])
    }

    fn source_snapshot(&self) -> Vec<Vec<u8>> {
        ["a", "b", "empty", ".git/index", ".git/HEAD", ".git/config"]
            .iter()
            .map(|p| fs::read(self.source.join(p)).unwrap())
            .collect()
    }

    fn recover_idempotently(&self) {
        success(self.thin(&["recover"]));
        let records = self
            .journals()
            .iter()
            .map(fs::read)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let registrations = self.registrations();
        let refs = self.refs();
        success(self.thin(&["recover", "--dry-run"]));
        success(self.thin(&["recover"]));
        assert_eq!(
            self.journals()
                .iter()
                .map(fs::read)
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            records
        );
        assert_eq!(self.registrations(), registrations);
        assert_eq!(self.refs(), refs);
    }

    fn worktree_snapshot(&self) -> Vec<(PathBuf, u64, u32, Vec<u8>)> {
        fn collect(path: &Path, result: &mut Vec<(PathBuf, u64, u32, Vec<u8>)>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            let bytes = if metadata.is_dir() {
                Vec::new()
            } else if metadata.file_type().is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                fs::read(path).unwrap()
            };
            result.push((path.to_owned(), metadata.ino(), metadata.mode(), bytes));
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    collect(&entry.unwrap().path(), result);
                }
            }
        }
        let mut result = Vec::new();
        let mut roots = self.stages();
        if self.destination.exists() {
            roots.push(self.destination.clone());
        }
        for root in roots {
            collect(&root, &mut result);
            if root.join(".git").exists() {
                let output = command("git", &root)
                    .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
                    .output()
                    .unwrap();
                if output.status.success() {
                    let index = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim_end());
                    if index.exists() {
                        collect(&index, &mut result);
                    }
                }
            }
        }
        result.sort();
        result
    }

    fn preview_unchanged(&self) -> Output {
        let records = self
            .journals()
            .iter()
            .map(fs::read)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let registrations = self.registrations();
        let refs = self.refs();
        let stages = self.stages();
        let contents = self.worktree_snapshot();
        let result = self.thin(&["recover", "--dry-run"]);
        assert_eq!(
            self.journals()
                .iter()
                .map(fs::read)
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            records
        );
        assert_eq!(self.registrations(), registrations);
        assert_eq!(self.refs(), refs);
        assert_eq!(self.stages(), stages);
        assert_eq!(self.worktree_snapshot(), contents);
        result
    }
}

#[test]
fn actual_creation_transitions_recover_or_retain_without_loss() {
    for point in [
        "after-reservation",
        "after-first-journal",
        "after-registration",
        "before-clone",
        "after-partial-clone",
        "before-index-initialization",
        "after-index-initialization",
        "after-index-journal",
        "before-verification",
        "after-verification",
        "after-publishing-journal",
        "after-rename",
        "after-repair",
        "after-ready-journal",
    ] {
        let fixture = Fixture::new();
        if !fixture.native() {
            return;
        }
        let source = fixture.source_snapshot();
        let result = fixture.add(&format!("crash-{point}"));
        assert_eq!(
            result.status.code(),
            Some(86),
            "{point}: {}",
            diagnostic(&result)
        );
        let refs = fixture.refs();
        let stages = fixture.stages();
        if point == "after-reservation" {
            assert_eq!(stages.len(), 1);
            assert!(fixture.journals().is_empty());
            assert!(fs::read_dir(&stages[0]).unwrap().next().is_none());
            fixture.recover_idempotently();
            // Recovery cannot establish ownership without a journal.
            assert_eq!(fixture.stages(), stages);
        } else if matches!(
            point,
            "before-index-initialization" | "after-index-initialization"
        ) {
            assert_eq!(fixture.record()["state"], "indexing");
            let journal = fs::read(&fixture.journals()[0]).unwrap();
            let contents = fixture.worktree_snapshot();
            assert_eq!(fixture.preview_unchanged().status.code(), Some(2));
            for _ in 0..2 {
                let recovery = fixture.thin(&["recover"]);
                assert_eq!(recovery.status.code(), Some(2));
                assert!(diagnostic(&recovery).contains("ambiguous index initialization"));
                assert_eq!(fs::read(&fixture.journals()[0]).unwrap(), journal);
                assert_eq!(fixture.stages(), stages);
                assert_eq!(fixture.worktree_snapshot(), contents);
            }
        } else if matches!(
            point,
            "after-rename" | "after-repair" | "after-ready-journal"
        ) {
            assert!(fixture.destination.exists());
            let metadata = fs::symlink_metadata(&fixture.destination).unwrap();
            let index_path = String::from_utf8(git(
                &fixture.destination,
                &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            ))
            .unwrap();
            let index = fs::read(index_path.trim_end()).unwrap();
            success(fixture.preview_unchanged());
            fixture.recover_idempotently();
            assert_eq!(fixture.record()["state"], "ready");
            assert_eq!(
                fs::symlink_metadata(&fixture.destination).unwrap().ino(),
                metadata.ino()
            );
            assert_eq!(fs::read(index_path.trim_end()).unwrap(), index);
            assert_eq!(fs::read(fixture.destination.join("a")).unwrap(), source[0]);
            assert_eq!(fs::read(fixture.destination.join("b")).unwrap(), source[1]);
            success(fixture.thin(&["check", text(&fixture.destination)]));
        } else {
            if point == "after-partial-clone" {
                assert_eq!(fixture.record()["state"], "creating");
                assert!(stages[0].join("a").exists());
                assert!(!stages[0].join("b").exists());
            }
            success(fixture.preview_unchanged());
            fixture.recover_idempotently();
            assert!(fixture.journals().is_empty(), "{point}");
            assert!(fixture.stages().is_empty(), "{point}");
            assert!(!fixture.destination.exists());
        }
        assert_eq!(fixture.refs(), refs, "branches must survive {point}");
        assert_eq!(
            fixture.source_snapshot(),
            source,
            "source changed at {point}"
        );
        eprintln!("EXECUTED native abrupt exit(86): {point}");
    }
}

#[test]
fn crashes_during_every_metadata_write_stage_leave_a_recoverable_or_ambiguous_journal() {
    for state in ["creating", "indexing", "verifying", "publishing", "ready"] {
        for operation in [
            "serialize",
            "write",
            "file-sync",
            "persist",
            "directory-sync",
        ] {
            let fixture = Fixture::new();
            if !fixture.native() {
                return;
            }
            let source = fixture.source_snapshot();
            let point = format!("crash-{state}-metadata-{operation}");
            assert_eq!(fixture.add(&point).status.code(), Some(86), "{point}");
            let refs = fixture.refs();
            let recorded_state = fixture
                .journals()
                .first()
                .map(|_| fixture.record()["state"].as_str().unwrap().to_owned());
            if recorded_state.as_deref() == Some("indexing") {
                assert_eq!(
                    fixture.preview_unchanged().status.code(),
                    Some(2),
                    "{point}"
                );
                let before = fs::read(&fixture.journals()[0]).unwrap();
                let contents = fixture.worktree_snapshot();
                for _ in 0..2 {
                    assert_eq!(fixture.thin(&["recover"]).status.code(), Some(2), "{point}");
                }
                assert_eq!(fs::read(&fixture.journals()[0]).unwrap(), before);
                assert_eq!(fixture.worktree_snapshot(), contents);
                assert!(!fixture.stages().is_empty());
            } else {
                success(fixture.preview_unchanged());
                fixture.recover_idempotently();
                if state == "creating" && operation != "directory-sync" {
                    assert_eq!(fixture.stages().len(), 1, "unjournaled reservation {point}");
                } else if state == "ready" {
                    assert_eq!(fixture.record()["state"], "ready");
                    assert_eq!(fs::read(fixture.destination.join("a")).unwrap(), source[0]);
                } else {
                    assert!(fixture.stages().is_empty(), "{point}");
                    assert!(fixture.journals().is_empty(), "{point}");
                }
            }
            assert_eq!(fixture.refs(), refs);
            assert_eq!(fixture.source_snapshot(), source);
            eprintln!("EXECUTED native abrupt exit(86): {point}; last state {recorded_state:?}");
        }
    }
}

#[test]
fn returned_io_errors_after_cloning_never_publish_or_discard_branches() {
    for error in ["ENOSPC", "EACCES", "EIO"] {
        for operation in [
            "serialize",
            "write",
            "file-sync",
            "persist",
            "directory-sync",
        ] {
            let fixture = Fixture::new();
            if !fixture.native() {
                return;
            }
            let source = fixture.source_snapshot();
            let point = format!("{error}-publishing-metadata-{operation}");
            let result = fixture.add(&point);
            assert_eq!(
                result.status.code(),
                Some(2),
                "{point}: {}",
                diagnostic(&result)
            );
            assert!(diagnostic(&result).contains(&format!("injected {error}")));
            assert!(!fixture.destination.exists());
            assert!(fixture.stages().is_empty());
            assert!(fixture.journals().is_empty());
            let branch = git(&fixture.source, &["rev-parse", "agent/fault"]);
            assert_eq!(branch, git(&fixture.source, &["rev-parse", "main"]));
            fixture.recover_idempotently();
            assert_eq!(fixture.source_snapshot(), source);
            eprintln!("EXECUTED simulated I/O error after full clone: {point}");
        }
    }
}

#[test]
fn removal_boundaries_and_metadata_failures_retain_refs_and_finish_idempotently() {
    let points = [
        "after-removing-journal",
        "after-git-removal",
        "before-journal-delete",
        "after-journal-delete",
        "removing-metadata-serialize",
        "removing-metadata-write",
        "removing-metadata-file-sync",
        "removing-metadata-persist",
        "removing-metadata-directory-sync",
    ];
    for point in points {
        let fixture = Fixture::new();
        if !fixture.native() {
            return;
        }
        success(fixture.add(""));
        let source = fixture.source_snapshot();
        let refs = fixture.refs();
        let result = fixture
            .thin_command(&["remove", text(&fixture.destination)])
            .env("GIT_THIN_TEST_FAILPOINT", format!("crash-{point}"))
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(86),
            "{point}: {}",
            diagnostic(&result)
        );
        success(fixture.preview_unchanged());
        fixture.recover_idempotently();
        let before_intent =
            point.starts_with("removing-metadata-") && point != "removing-metadata-directory-sync";
        if before_intent {
            assert_eq!(fixture.record()["state"], "ready");
            assert!(fixture.destination.exists());
            success(fixture.thin(&["remove", text(&fixture.destination)]));
        }
        assert!(!fixture.destination.exists(), "{point}");
        assert!(fixture.journals().is_empty(), "{point}");
        assert_eq!(fixture.refs(), refs);
        assert_eq!(fixture.source_snapshot(), source);
        eprintln!("EXECUTED native abrupt exit(86) during removal: {point}");
    }
}

struct PausedChild(Option<Child>);
impl Drop for PausedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn sigkill_after_partial_clone_releases_lock_and_retains_the_new_branch() {
    let fixture = Fixture::new();
    if !fixture.native() {
        return;
    }
    let source = fixture.source_snapshot();
    let ready = fixture.parent.join("ready");
    let proceed = fixture.parent.join("proceed");
    let mut child = PausedChild(Some(
        fixture
            .thin_command(&["add", text(&fixture.destination), "--branch", "agent/fault"])
            .env("GIT_THIN_TEST_FAILPOINT", "pause-after-partial-clone")
            .env("GIT_THIN_TEST_READY_FILE", &ready)
            .env("GIT_THIN_TEST_CONTINUE_FILE", &proceed)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() {
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "child exited before handshake"
        );
        assert!(Instant::now() < deadline, "handshake timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    child.0.as_mut().unwrap().kill().unwrap();
    let status = child.0.take().unwrap().wait().unwrap();
    assert!(!status.success());
    let refs = fixture.refs();
    success(fixture.preview_unchanged());
    fixture.recover_idempotently();
    assert!(fixture.stages().is_empty());
    assert!(!fixture.destination.exists());
    assert_eq!(fixture.refs(), refs);
    assert_eq!(fixture.source_snapshot(), source);
    eprintln!("EXECUTED native SIGKILL after partial clone");
}

fn paused_add(fixture: &Fixture, point: &str) -> (PausedChild, PathBuf) {
    let ready = fixture.parent.join("ready");
    let proceed = fixture.parent.join("proceed");
    let mut child = PausedChild(Some(
        fixture
            .thin_command(&["add", text(&fixture.destination), "--branch", "agent/fault"])
            .env("GIT_THIN_TEST_FAILPOINT", format!("pause-{point}"))
            .env("GIT_THIN_TEST_READY_FILE", &ready)
            .env("GIT_THIN_TEST_CONTINUE_FILE", &proceed)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() {
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "child exited before handshake"
        );
        assert!(Instant::now() < deadline, "handshake timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    (child, proceed)
}

fn resume_with_timeout(mut child: PausedChild, proceed: &Path) -> std::process::ExitStatus {
    fs::write(proceed, b"continue").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
            child.0.take();
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "child did not finish after handshake"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn immediate_rollback_preserves_staged_user_deletions() {
    for point in [
        "before-verification",
        "after-partial-clone",
        "before-index-initialization",
    ] {
        let fixture = Fixture::new();
        if !fixture.native() {
            return;
        }
        let source = fixture.source_snapshot();
        let (child, proceed) = paused_add(&fixture, point);
        let stage = &fixture.stages()[0];
        if point != "before-verification" {
            git(stage, &["read-tree", "HEAD"]);
        }
        git(stage, &["rm", "--cached", "a"]);
        let index_path = String::from_utf8(git(
            stage,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        ))
        .unwrap();
        let index = fs::read(index_path.trim_end()).unwrap();
        let refs = fixture.refs();
        let registrations = fixture.registrations();
        let journal = fs::read(&fixture.journals()[0]).unwrap();
        assert_eq!(resume_with_timeout(child, &proceed).code(), Some(2));
        assert_eq!(fs::read(index_path.trim_end()).unwrap(), index);
        assert_eq!(fs::read(stage.join("a")).unwrap(), source[0]);
        assert_eq!(fixture.refs(), refs);
        assert_eq!(fixture.registrations(), registrations);
        assert_eq!(fs::read(&fixture.journals()[0]).unwrap(), journal);
        assert_eq!(fixture.preview_unchanged().status.code(), Some(2));
        assert_eq!(fixture.thin(&["recover"]).status.code(), Some(2));
        assert_eq!(fs::read(index_path.trim_end()).unwrap(), index);
        assert_eq!(fixture.source_snapshot(), source);
    }
}

struct RestorePermissions(PathBuf, fs::Permissions);
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, self.1.clone());
    }
}

#[test]
fn real_unprivileged_permission_failure_preserves_partial_creation() {
    use std::os::unix::fs::PermissionsExt;
    // Root can bypass directory permissions; do not mislabel that as execution.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("UNAVAILABLE: real EACCES requires an unprivileged test user");
        return;
    }
    let fixture = Fixture::new();
    if !fixture.native() {
        return;
    }
    let source = fixture.source_snapshot();
    let (child, proceed) = paused_add(&fixture, "after-partial-clone");
    let stage = fixture.stages()[0].clone();
    let original = fs::metadata(&stage).unwrap().permissions();
    let restore = RestorePermissions(stage.clone(), original);
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o500)).unwrap();
    let refs = fixture.refs();
    assert_eq!(resume_with_timeout(child, &proceed).code(), Some(2));
    assert!(!fixture.destination.exists());
    assert!(stage.join("a").exists());
    assert!(!stage.join("b").exists());
    assert!(!fixture.journals().is_empty());
    assert_eq!(fixture.refs(), refs);
    drop(restore);
    let preview = fixture.preview_unchanged();
    if preview.status.success() {
        fixture.recover_idempotently();
        assert!(fixture.stages().is_empty());
    } else {
        // Some Git versions unregister a worktree even when deleting its files
        // fails. Retain this orphan and its journal for manual recovery.
        assert!(
            diagnostic(&preview).contains("registration"),
            "{}",
            diagnostic(&preview)
        );
        let journal = fs::read(&fixture.journals()[0]).unwrap();
        let identity = fs::symlink_metadata(&stage).unwrap().ino();
        for _ in 0..2 {
            let result = fixture.thin(&["recover"]);
            assert_eq!(result.status.code(), Some(2));
            assert!(diagnostic(&result).contains("registration"));
            assert_eq!(fs::read(&fixture.journals()[0]).unwrap(), journal);
            assert_eq!(fs::read(stage.join("a")).unwrap(), source[0]);
            assert_eq!(fs::symlink_metadata(&stage).unwrap().ino(), identity);
        }
    }
    assert_eq!(fixture.refs(), refs);
    assert_eq!(fixture.source_snapshot(), source);
    eprintln!(
        "EXECUTED real EACCES as uid {} after first native clone",
        unsafe { libc::geteuid() }
    );
}

#[test]
fn replacing_a_reserved_directory_never_registers_or_removes_the_replacement() {
    use std::os::unix::fs::PermissionsExt;
    for fail_first_save in [false, true] {
        if fail_first_save && unsafe { libc::geteuid() } == 0 {
            eprintln!("UNAVAILABLE: first-save real EACCES needs an unprivileged test user");
            continue;
        }
        let fixture = Fixture::new();
        if !fixture.native() {
            return;
        }
        let source = fixture.source_snapshot();
        let refs = fixture.refs();
        let registrations = fixture.registrations();
        let (child, proceed) = paused_add(&fixture, "after-reservation");
        let stage = fixture.stages()[0].clone();
        let retained = fixture.parent.join("retained-reservation");
        fs::rename(&stage, &retained).unwrap();
        fs::create_dir(&stage).unwrap();
        let identity = fs::metadata(&stage).unwrap().ino();
        let restore = if fail_first_save {
            let directory = fixture.source.join(".git/git-thin");
            let original = fs::metadata(&directory).unwrap().permissions();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
            Some(RestorePermissions(directory, original))
        } else {
            None
        };
        assert_eq!(resume_with_timeout(child, &proceed).code(), Some(2));
        drop(restore);
        assert_eq!(fs::metadata(&stage).unwrap().ino(), identity);
        assert!(fs::read_dir(&stage).unwrap().next().is_none());
        assert!(fs::read_dir(&retained).unwrap().next().is_none());
        assert_eq!(fixture.refs(), refs);
        assert_eq!(fixture.registrations(), registrations);
        assert_eq!(fixture.source_snapshot(), source);
        if fail_first_save {
            assert!(fixture.journals().is_empty());
            fixture.recover_idempotently();
        } else {
            for _ in 0..2 {
                let result = fixture.thin(&["recover"]);
                assert_eq!(result.status.code(), Some(2));
                assert!(diagnostic(&result).contains("identity changed"));
            }
        }
        assert_eq!(fs::metadata(&stage).unwrap().ino(), identity);
        assert!(retained.exists());
    }
}

#[test]
fn immediate_rollback_preserves_intent_to_add_of_a_baseline_empty_file() {
    let fixture = Fixture::new();
    if !fixture.native() {
        return;
    }
    let (child, proceed) = paused_add(&fixture, "before-verification");
    let stage = fixture.stages()[0].clone();
    git(&stage, &["rm", "--cached", "empty"]);
    git(&stage, &["add", "-N", "empty"]);
    assert!(
        String::from_utf8(git(&stage, &["diff", "--cached", "--name-status"]))
            .unwrap()
            .contains("D\tempty")
    );
    let contents = fixture.worktree_snapshot();
    let refs = fixture.refs();
    let registrations = fixture.registrations();
    let journal = fs::read(&fixture.journals()[0]).unwrap();
    assert_eq!(resume_with_timeout(child, &proceed).code(), Some(2));
    assert_eq!(fixture.worktree_snapshot(), contents);
    assert_eq!(fixture.refs(), refs);
    assert_eq!(fixture.registrations(), registrations);
    assert_eq!(fs::read(&fixture.journals()[0]).unwrap(), journal);
    assert_eq!(fixture.preview_unchanged().status.code(), Some(2));
    assert_eq!(fixture.thin(&["recover"]).status.code(), Some(2));
    assert_eq!(fixture.worktree_snapshot(), contents);
}
