//! Process-level native tests. Handshakes place races at known transitions.
#![cfg(all(unix, feature = "fault-injection"))]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn text(p: &Path) -> &str {
    p.to_str().unwrap()
}
fn command(cwd: &Path, program: &str) -> Command {
    let mut c = Command::new(program);
    c.current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        c.env_remove(key);
    }
    c
}
fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn git(cwd: &Path, args: &[&str]) -> Vec<u8> {
    ok(command(cwd, "git").args(args).output().unwrap()).stdout
}
fn thin(cwd: &Path, args: &[&str]) -> Output {
    command(cwd, env!("CARGO_BIN_EXE_git-thin"))
        .args(args)
        .output()
        .unwrap()
}
fn error(out: Output, expected: &str) {
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["error"].as_str().unwrap().contains(expected), "{v}");
}
struct Fixture {
    _temp: tempfile::TempDir,
    parent: PathBuf,
    source: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let source = parent.join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "user.name", "Race tests"]);
        git(&source, &["config", "user.email", "race@example.invalid"]);
        git(&source, &["config", "core.autocrlf", "false"]);
        fs::create_dir(source.join("src")).unwrap();
        fs::write(source.join("src/file"), b"baseline bytes\n").unwrap();
        fs::write(source.join("README"), b"readme bytes\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-qm", "baseline"]);
        Self {
            _temp: temp,
            parent,
            source,
        }
    }
    fn native(&self) -> bool {
        match git_thin::backend::probe(&self.source, &self.parent) {
            Ok(()) => true,
            Err(e) => {
                assert!(
                    e.contains("native copy-on-write cloning is unsupported here")
                        && std::env::var("GIT_THIN_REQUIRE_CLONE").as_deref() != Ok("1"),
                    "{e}"
                );
                eprintln!("UNAVAILABLE native concurrency: {e}");
                false
            }
        }
    }
    fn add(&self, name: &str) -> PathBuf {
        let p = self.parent.join(name);
        ok(thin(
            &self.source,
            &[
                "add",
                text(&p),
                "--branch",
                name,
                "--write",
                "src",
                "--json",
            ],
        ));
        p
    }
}

// Drops always reap children, including a panic while a race is paused.
struct Paused {
    child: Child,
    ready: PathBuf,
    proceed: PathBuf,
    output: PathBuf,
}
impl Paused {
    fn add(f: &Fixture, name: &str, branch: &str, point: &str) -> Self {
        let ready = f.parent.join(format!("{name}.ready"));
        let proceed = f.parent.join(format!("{name}.continue"));
        let output = f.parent.join(format!("{name}.output"));
        let log = File::create(&output).unwrap();
        let child = command(&f.source, env!("CARGO_BIN_EXE_git-thin"))
            .args([
                "add",
                text(&f.parent.join(name)),
                "--branch",
                branch,
                "--write",
                "src",
                "--json",
            ])
            .env("GIT_THIN_TEST_FAILPOINT", format!("pause-{point}"))
            .env("GIT_THIN_TEST_READY_FILE", &ready)
            .env("GIT_THIN_TEST_CONTINUE_FILE", &proceed)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let mut p = Self {
            child,
            ready,
            proceed,
            output,
        };
        let start = Instant::now();
        while !p.ready.exists() {
            assert!(
                p.child.try_wait().unwrap().is_none(),
                "child exited before handshake: {}",
                fs::read_to_string(&p.output).unwrap()
            );
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "handshake timeout"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        p
    }
    fn finish(&mut self) -> (i32, serde_json::Value) {
        fs::write(&self.proceed, b"continue").unwrap();
        let start = Instant::now();
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                let bytes = fs::read(&self.output).unwrap();
                return (s.code().unwrap(), serde_json::from_slice(&bytes).unwrap());
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "completion timeout"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Paused {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    files: BTreeMap<PathBuf, (u32, Vec<u8>)>,
    index: Vec<u8>,
    head: Vec<u8>,
    config: Vec<u8>,
}
fn snapshot(root: &Path) -> Snapshot {
    fn walk(root: &Path, relative: &Path, result: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".git" {
                continue;
            }
            let p = relative.join(entry.file_name());
            let m = fs::symlink_metadata(root.join(&p)).unwrap();
            if m.is_dir() {
                walk(root, &p, result);
            } else {
                use std::os::unix::ffi::OsStrExt;
                let bytes = if m.file_type().is_symlink() {
                    fs::read_link(root.join(&p))
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec()
                } else {
                    fs::read(root.join(&p)).unwrap()
                };
                result.insert(p, (m.mode(), bytes));
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, Path::new(""), &mut files);
    let index_path = String::from_utf8(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    ))
    .unwrap();
    Snapshot {
        files,
        index: fs::read(index_path.trim_end()).unwrap(),
        head: git(root, &["rev-parse", "HEAD"]),
        config: git(root, &["config", "--local", "--list", "-z"]),
    }
}

#[test]
fn competing_creations_preserve_winner_and_independent_repositories() {
    for (second_name, second_branch) in
        [("winner", "other"), ("other", "winner"), ("other", "other")]
    {
        let f = Fixture::new();
        if !f.native() {
            return;
        }
        let before = snapshot(&f.source);
        let mut first = Paused::add(&f, "winner", "winner", "before-clone");
        let loser = thin(
            &f.source,
            &[
                "add",
                text(&f.parent.join(second_name)),
                "--branch",
                second_branch,
                "--json",
            ],
        );
        error(
            loser,
            if second_branch == "winner" {
                "branch already exists"
            } else {
                "another git-thin operation"
            },
        );
        let unrelated = Fixture::new();
        let independent = unrelated.add("independent");
        assert!(independent.join("src/file").exists());
        let (code, out) = first.finish();
        assert_eq!(code, 0, "{out}");
        assert_eq!(snapshot(&f.source), before);
        let commit = String::from_utf8(before.head.clone()).unwrap();
        git_thin::git::verify(
            &f.parent.join("winner"),
            &git_thin::git::tree(&f.source, commit.trim()).unwrap(),
        )
        .unwrap();
        if second_name != "winner" && second_branch != "winner" {
            f.add("other");
        } else {
            let retry = thin(
                &f.source,
                &[
                    "add",
                    text(&f.parent.join(second_name)),
                    "--branch",
                    second_branch,
                    "--json",
                ],
            );
            error(
                retry,
                if second_name == "winner" {
                    "destination already exists"
                } else {
                    "branch already exists"
                },
            );
        }
        let count = if second_name != "winner" && second_branch != "winner" {
            2
        } else {
            1
        };
        let records: serde_json::Value =
            serde_json::from_slice(&ok(thin(&f.source, &["list", "--json"])).stdout).unwrap();
        let trees = records["worktrees"].as_array().unwrap();
        assert_eq!(trees.len(), count);
        assert!(trees.iter().all(|m| m["state"] == "ready"));
        let registrations = git(&f.source, &["worktree", "list", "--porcelain", "-z"]);
        assert_eq!(
            registrations
                .split(|b| *b == 0)
                .filter(|r| r.starts_with(b"worktree "))
                .count(),
            count + 1
        );
        let branches = git(
            &f.source,
            &["for-each-ref", "--format=%(refname)", "refs/heads/"],
        );
        assert_eq!(
            branches,
            if count == 2 {
                b"refs/heads/main\nrefs/heads/other\nrefs/heads/winner\n".as_slice()
            } else {
                b"refs/heads/main\nrefs/heads/winner\n".as_slice()
            }
        );
        assert_eq!(
            git(&f.parent.join("winner"), &["rev-parse", "HEAD"]),
            before.head
        );
        assert_eq!(
            git(&f.parent.join("winner"), &["ls-files", "--stage", "-z"]),
            git(&f.source, &["ls-files", "--stage", "-z"])
        );
        assert!(!fs::read_dir(&f.parent).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".git-thin-")));
    }
}

#[test]
fn lock_contention_and_killed_holder_preserve_metadata_and_release_lock() {
    let f = Fixture::new();
    if !f.native() {
        return;
    }
    let managed = f.add("existing");
    let before = thin(&f.source, &["list", "--json"]).stdout;
    let mut holder = Paused::add(&f, "paused", "paused", "before-clone");
    for args in [
        vec!["recover", "--json"],
        vec!["remove", text(&managed), "--json"],
        vec!["scope", "add", text(&managed), "README", "--json"],
    ] {
        error(thin(&f.source, &args), "another git-thin operation");
    }
    holder.child.kill().unwrap();
    holder.child.wait().unwrap();
    let first = ok(thin(&f.source, &["recover", "--dry-run", "--json"]));
    assert!(String::from_utf8_lossy(&first.stdout).contains("rollback"));
    ok(thin(&f.source, &["recover", "--json"]));
    assert_eq!(thin(&f.source, &["list", "--json"]).stdout, before);
    assert!(f.source.join(".git/git-thin/operations.lock").exists());
    ok(thin(
        &f.source,
        &["scope", "add", text(&managed), "README", "--json"],
    ));
    let status: serde_json::Value =
        serde_json::from_slice(&ok(thin(&f.source, &["status", text(&managed), "--json"])).stdout)
            .unwrap();
    assert!(status["worktree"]["scope"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["path"] == "README"));
    f.add("after-kill");
}

#[test]
fn destination_directory_and_symlink_appearing_during_creation_survive() {
    for link in [false, true] {
        let f = Fixture::new();
        if !f.native() {
            return;
        }
        let before = snapshot(&f.source);
        let mut child = Paused::add(&f, "race", "race", "before-clone");
        let destination = f.parent.join("race");
        let target = f.parent.join("sentinel");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("valuable"), b"keep").unwrap();
        if link {
            symlink(&target, &destination).unwrap();
        } else {
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("valuable"), b"keep").unwrap();
        }
        let identity = fs::symlink_metadata(&destination).unwrap().ino();
        let (code, out) = child.finish();
        assert_eq!(code, 2, "{out}");
        assert_eq!(fs::symlink_metadata(&destination).unwrap().ino(), identity);
        assert_eq!(fs::read(destination.join("valuable")).unwrap(), b"keep");
        assert_eq!(snapshot(&f.source), before);
        ok(thin(&f.source, &["recover", "--json"]));
        assert_eq!(fs::symlink_metadata(&destination).unwrap().ino(), identity);
        ok(thin(&f.source, &["recover", "--json"]));
        assert_eq!(fs::read(destination.join("valuable")).unwrap(), b"keep");
        assert_eq!(fs::read(target.join("valuable")).unwrap(), b"keep");
        if link {
            assert_eq!(fs::read_link(&destination).unwrap(), target);
        }
    }
}

#[test]
fn source_mutations_at_clone_boundary_are_preserved_and_never_publish_wrong_bytes() {
    for pattern in [
        "in-place", "atomic", "recreate", "mode", "symlink", "parent",
    ] {
        let f = Fixture::new();
        if !f.native() {
            return;
        }
        let baseline = git(&f.source, &["rev-parse", "HEAD"]);
        let index = snapshot(&f.source).index;
        let mut child = Paused::add(&f, "mutation", "mutation", "before-clone");
        let p = f.source.join("src/file");
        match pattern {
            "in-place" => {
                let m = fs::metadata(&p).unwrap();
                fs::write(&p, b"modified bytes\n").unwrap();
                File::options()
                    .write(true)
                    .open(&p)
                    .unwrap()
                    .set_times(
                        fs::FileTimes::new()
                            .set_accessed(m.accessed().unwrap())
                            .set_modified(m.modified().unwrap()),
                    )
                    .unwrap();
            }
            "atomic" => {
                fs::write(f.source.join("replacement"), b"replacement\n").unwrap();
                fs::rename(f.source.join("replacement"), &p).unwrap();
            }
            "recreate" => {
                fs::remove_file(&p).unwrap();
                fs::write(&p, b"recreated\n").unwrap();
            }
            "mode" => fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap(),
            "symlink" => {
                fs::remove_file(&p).unwrap();
                symlink("../README", &p).unwrap();
            }
            "parent" => {
                fs::rename(f.source.join("src"), f.source.join("kept-src")).unwrap();
                symlink("kept-src", f.source.join("src")).unwrap();
            }
            _ => unreachable!(),
        }
        let edited = snapshot(&f.source);
        let (code, out) = child.finish();
        assert_eq!(snapshot(&f.source), edited, "source changed for {pattern}");
        assert_eq!(edited.index, index);
        assert_eq!(edited.head, baseline);
        if code == 0 {
            let commit = String::from_utf8(baseline).unwrap();
            git_thin::git::verify(
                &f.parent.join("mutation"),
                &git_thin::git::tree(&f.source, commit.trim()).unwrap(),
            )
            .unwrap();
        } else {
            assert_eq!(code, 2, "{pattern}: {out}");
            assert!(
                !f.parent.join("mutation").exists(),
                "incorrect worktree published: {out}"
            );
        }
    }
}
