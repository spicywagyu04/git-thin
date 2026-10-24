//! Ordinary Git fixtures: these do not establish native cloning or allocation.

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

pub fn text(path: &Path) -> &str {
    path.to_str().expect("UTF-8 fixture path")
}

pub fn command(program: &str, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR");
    command
}

pub fn diagnostic(output: &Output) -> String {
    format!(
        "exit {:?}; stdout: {}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn git(root: &Path, args: &[&str]) -> String {
    let output = command("git", root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        diagnostic(&output)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
}

pub fn thin(root: &Path, args: &[&str]) -> Output {
    command(env!("CARGO_BIN_EXE_git-thin"), root)
        .args(args)
        .output()
        .unwrap()
}

pub fn json_output(output: Output, exit: i32) -> Value {
    assert_eq!(output.status.code(), Some(exit), "{}", diagnostic(&output));
    assert!(
        output.stderr.is_empty(),
        "JSON diagnostics belong on stdout: {}",
        diagnostic(&output)
    );
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid JSON: {error}; {}", diagnostic(&output)));
    if exit == 2 {
        assert_eq!(value["exit_code"], 2);
        assert!(value["error"]
            .as_str()
            .is_some_and(|message| !message.is_empty()));
    }
    value
}

pub struct Fixture {
    _temp: TempDir,
    pub root: PathBuf,
    pub source: PathBuf,
    pub worktree: PathBuf,
    pub baseline: String,
    pub journal: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_format("sha1", false)
    }

    pub fn with_format(format: &str, empty: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        git(
            &source,
            &["init", "-b", "main", &format!("--object-format={format}")],
        );
        git(&source, &["config", "user.name", "Compatibility Tests"]);
        git(&source, &["config", "user.email", "tests@example.invalid"]);
        git(&source, &["config", "core.autocrlf", "false"]);
        if !empty {
            fs::create_dir(source.join("allowed")).unwrap();
            fs::write(source.join("allowed/file"), "allowed baseline\n").unwrap();
            fs::write(source.join("outside"), "outside baseline\n").unwrap();
            fs::write(source.join(".gitignore"), "build/\n").unwrap();
            git(&source, &["add", "."]);
        }
        git(&source, &["commit", "--allow-empty", "-qm", "baseline"]);
        let baseline = git(&source, &["rev-parse", "HEAD"]);
        let worktree = root.join("managed space\t雪\ncheckout");
        git(
            &source,
            &["worktree", "add", "-b", "agent/test", text(&worktree)],
        );
        let id = uuid::Uuid::new_v4().to_string();
        let directory = fs::symlink_metadata(&worktree).unwrap();
        let journal_dir = source.join(".git/git-thin");
        fs::create_dir(&journal_dir).unwrap();
        let journal = journal_dir.join(format!("{id}.json"));
        // Ready schema-1 metadata remains usable across journal evolution.
        // This fixture intentionally uses ordinary Git, never a clone claim.
        fs::write(
            &journal,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1, "id": id, "creation_commit": baseline,
                "branch": "agent/test", "backend": "ordinary-git-fixture",
                "scope": [{"path": "allowed", "directory": true}], "state": "ready",
                "path": worktree, "staging_path": root.join(format!(".git-thin-{id}")),
                "device": directory.dev(), "inode": directory.ino(),
                "clone_ms": 0, "verification_ms": 0
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            source,
            worktree,
            baseline,
            journal,
        }
    }

    pub fn check(&self, exit: i32) -> Value {
        json_output(
            thin(&self.source, &["check", text(&self.worktree), "--json"]),
            exit,
        )
    }

    pub fn metadata(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.journal).unwrap()).unwrap()
    }

    pub fn scope(&self, paths: &[&str]) {
        let mut args = vec!["--json", "scope", "add", text(&self.worktree)];
        args.extend_from_slice(paths);
        let value = json_output(thin(&self.source, &args), 0);
        assert_eq!(value["worktree"]["creation_commit"], self.baseline);
    }

    pub fn assert_check(&self, violations: &[&str], changed: &[&str]) {
        let value = self.check(if violations.is_empty() { 0 } else { 1 });
        assert_eq!(value["violations"], json!(violations));
        assert_eq!(value["changed_paths"], json!(changed));
        assert_eq!(value["clean"], violations.is_empty());
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct GitSnapshot {
    index: Vec<u8>,
    head: String,
    flags: String,
    refs: String,
    config: Vec<u8>,
}

impl GitSnapshot {
    pub fn read(root: &Path) -> Self {
        let index = git(
            root,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        );
        let config = git(
            root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "config",
            ],
        );
        Self {
            index: fs::read(index).unwrap(),
            head: git(root, &["rev-parse", "HEAD"]),
            flags: git(root, &["ls-files", "-v", "-z"]),
            refs: git(root, &["for-each-ref", "--format=%(refname) %(objectname)"]),
            config: fs::read(config).unwrap(),
        }
    }
}
