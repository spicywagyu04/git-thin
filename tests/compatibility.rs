//! Ordinary Git compatibility is distinct from tests explicitly requiring native clones.
#![cfg(unix)]

mod support;

use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use support::*;

fn can_clone(fixture: &Fixture) -> bool {
    match git_thin::backend::probe(&fixture.source, &fixture.root) {
        Ok(()) => true,
        Err(error) => {
            assert!(
                error
                    .to_ascii_lowercase()
                    .contains("native copy-on-write cloning is unsupported here")
                    && std::env::var("GIT_THIN_REQUIRE_CLONE").as_deref() != Ok("1"),
                "native clone required or unexpected probe error: {error}"
            );
            eprintln!("UNAVAILABLE native compatibility scenario: {error}");
            false
        }
    }
}

fn add(source: &Path, destination: &Path, branch: &str, scope: Option<&str>) -> serde_json::Value {
    let mut args = vec!["add", text(destination), "--branch", branch, "--json"];
    if let Some(scope) = scope {
        args.extend(["--write", scope]);
    }
    json_output(thin(source, &args), 0)
}

fn index(root: &Path) -> Vec<u8> {
    fs::read(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    ))
    .unwrap()
}

#[test]
fn sha1_and_sha256_scope_use_the_fixed_object_format_and_intent_to_add() {
    for format in ["sha1", "sha256"] {
        let fixture = Fixture::with_format(format, false);
        assert_eq!(
            fixture.baseline.len(),
            if format == "sha1" { 40 } else { 64 }
        );
        fixture.assert_check(&[], &[]);
        fs::write(fixture.worktree.join("new"), "added\n").unwrap();
        git(&fixture.worktree, &["add", "-N", "new"]);
        fixture.assert_check(&["new"], &["new"]);
        git(&fixture.worktree, &["add", "new"]);
        git(&fixture.worktree, &["commit", "-qm", "addition"]);
        fixture.assert_check(&["new"], &["new"]);
        fixture.scope(&["new"]);
        fixture.assert_check(&[], &["new"]);
        assert_eq!(fixture.metadata()["creation_commit"], fixture.baseline);
    }
}

#[test]
fn merge_and_successful_rebase_keep_upstream_changes_in_scope_comparison() {
    for operation in ["merge", "rebase"] {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("outside"), "upstream change\n").unwrap();
        git(&fixture.source, &["commit", "-am", "upstream"]);
        fs::write(fixture.worktree.join("allowed/file"), "agent change\n").unwrap();
        git(&fixture.worktree, &["commit", "-am", "agent"]);
        git(&fixture.worktree, &[operation, "main"]);
        let source_before = GitSnapshot::read(&fixture.source);
        let worktree_before = GitSnapshot::read(&fixture.worktree);
        fixture.assert_check(&["outside"], &["allowed/file", "outside"]);
        assert_eq!(GitSnapshot::read(&fixture.source), source_before);
        assert_eq!(GitSnapshot::read(&fixture.worktree), worktree_before);
        fixture.scope(&["outside"]);
        fixture.assert_check(&[], &["allowed/file", "outside"]);
        assert_eq!(fixture.metadata()["creation_commit"], fixture.baseline);
        git(&fixture.worktree, &["switch", "-c", "agent/compatible"]);
        fixture.assert_check(&[], &["allowed/file", "outside"]);
        assert_eq!(fixture.metadata()["creation_commit"], fixture.baseline);
    }
}

#[test]
fn conflicted_index_is_checked_without_resolving_or_changing_it() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("outside"), "upstream\n").unwrap();
    git(&fixture.source, &["commit", "-am", "upstream"]);
    fs::write(fixture.worktree.join("outside"), "agent\n").unwrap();
    git(&fixture.worktree, &["commit", "-am", "agent"]);
    let merge = command("git", &fixture.worktree)
        .args(["merge", "main"])
        .output()
        .unwrap();
    assert!(
        !merge.status.success(),
        "expected conflict: {}",
        diagnostic(&merge)
    );
    assert!(!git(&fixture.worktree, &["ls-files", "--unmerged"]).is_empty());
    let before = GitSnapshot::read(&fixture.worktree);
    let bytes = fs::read(fixture.worktree.join("outside")).unwrap();
    fixture.assert_check(&["outside"], &["outside"]);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    assert_eq!(fs::read(fixture.worktree.join("outside")).unwrap(), bytes);
    assert!(Path::new(&git(
        &fixture.worktree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "MERGE_HEAD"
        ]
    ))
    .exists());
}

fn reject_source(fixture: &Fixture, expected: &str) {
    let before = GitSnapshot::read(&fixture.source);
    let source_bytes = tree_snapshot(&fixture.source);
    let registrations = git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]);
    let destination = fixture.root.join("rejected");
    let result = json_output(
        thin(
            &fixture.source,
            &[
                "add",
                text(&destination),
                "--branch",
                "agent/rejected",
                "--json",
            ],
        ),
        2,
    );
    assert!(
        result["error"].as_str().unwrap().contains(expected),
        "{result}"
    );
    assert_eq!(GitSnapshot::read(&fixture.source), before);
    assert_eq!(tree_snapshot(&fixture.source), source_bytes);
    assert_eq!(
        git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]),
        registrations
    );
    assert!(!destination.exists());
    assert!(
        !git(&fixture.source, &["for-each-ref", "--format=%(refname)"])
            .contains("refs/heads/agent/rejected")
    );
}

#[test]
fn real_merge_cherry_pick_revert_and_sequencer_conflicts_reject_source() {
    for operation in ["merge", "cherry-pick", "revert", "sequencer"] {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("outside"), "upstream\n").unwrap();
        git(&fixture.source, &["commit", "-am", "upstream"]);
        let upstream = git(&fixture.source, &["rev-parse", "HEAD"]);
        fs::write(fixture.worktree.join("outside"), "topic\n").unwrap();
        git(&fixture.worktree, &["commit", "-am", "topic"]);
        let topic = git(&fixture.worktree, &["rev-parse", "HEAD"]);
        let args: Vec<&str> = match operation {
            "merge" => vec!["merge", "agent/test"],
            "cherry-pick" => vec!["cherry-pick", "agent/test"],
            "revert" => {
                fs::write(fixture.source.join("outside"), "later\n").unwrap();
                git(&fixture.source, &["commit", "-am", "later"]);
                vec!["revert", "--no-edit", &upstream]
            }
            "sequencer" => {
                fs::write(fixture.worktree.join("allowed/file"), "next\n").unwrap();
                git(&fixture.worktree, &["commit", "-am", "next"]);
                vec!["cherry-pick", &topic, "agent/test"]
            }
            _ => unreachable!(),
        };
        let output = command("git", &fixture.source)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "expected {operation} conflict");
        if operation == "sequencer" {
            assert!(fixture.source.join(".git/sequencer").exists());
        }
        reject_source(&fixture, "operation in progress");
    }
}

type TreeSnapshot = BTreeMap<PathBuf, (u32, Vec<u8>)>;

fn tree_snapshot(root: &Path) -> TreeSnapshot {
    fn walk(root: &Path, relative: &Path, entries: &mut TreeSnapshot) {
        for entry in fs::read_dir(root.join(relative)).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if relative.as_os_str().is_empty() && name == ".git" {
                continue;
            }
            let relative = relative.join(name);
            let metadata = fs::symlink_metadata(root.join(&relative)).unwrap();
            if metadata.is_dir() {
                walk(root, &relative, entries);
            } else {
                let bytes = if metadata.file_type().is_symlink() {
                    use std::os::unix::ffi::OsStrExt;
                    fs::read_link(root.join(&relative))
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec()
                } else {
                    fs::read(root.join(&relative)).unwrap()
                };
                entries.insert(relative, (metadata.permissions().mode(), bytes));
            }
        }
    }
    let mut entries = BTreeMap::new();
    walk(root, Path::new(""), &mut entries);
    entries
}

#[test]
fn native_empty_tree_and_linked_source_support_both_object_formats() {
    for format in ["sha1", "sha256"] {
        let fixture = Fixture::with_format(format, true);
        if !can_clone(&fixture) {
            return;
        }
        let destination = fixture.root.join("from-linked-empty");
        let source_index = index(&fixture.source);
        let linked_index = index(&fixture.worktree);
        let value = add(&fixture.worktree, &destination, "native/empty", None);
        assert_eq!(value["worktree"]["creation_commit"], fixture.baseline);
        assert_eq!(value["worktree"]["scope"], json!([]));
        assert_eq!(git(&destination, &["rev-parse", "HEAD"]), fixture.baseline);
        assert!(git(&destination, &["ls-files", "-z"]).is_empty());
        assert_eq!(index(&fixture.worktree), linked_index);
        assert_eq!(index(&fixture.source), source_index);
        // The requested new branch is the only intended ref addition.
        assert_eq!(
            git(&fixture.worktree, &["rev-parse", "HEAD"]),
            fixture.baseline
        );
        let result = json_output(
            thin(&fixture.worktree, &["check", text(&destination), "--json"]),
            0,
        );
        assert_eq!(result["changed_paths"], json!([]));
        assert_eq!(
            git(&fixture.source, &["rev-parse", "main"]),
            fixture.baseline
        );
    }
}

#[test]
fn native_literal_paths_and_dangling_links_preserve_distinct_contents() {
    let fixture = Fixture::new();
    if !can_clone(&fixture) {
        return;
    }
    let names = [
        "-leading",
        "--json",
        ":(literal)name",
        "tab\tfile",
        "line\nfile",
        "雪",
    ];
    for name in names {
        fs::write(fixture.source.join(name), name).unwrap();
    }
    symlink("not-created", fixture.source.join("dangling")).unwrap();
    git(&fixture.source, &["add", "-A"]);
    git(&fixture.source, &["commit", "-qm", "unusual paths"]);
    let destination = fixture.root.join("-native\t雪\ncheckout");
    add(&fixture.source, &destination, "native/paths", Some("."));
    assert_eq!(tree_snapshot(&destination), tree_snapshot(&fixture.source));
    for name in names {
        assert_ne!(
            fs::metadata(destination.join(name)).unwrap().ino(),
            fs::metadata(fixture.source.join(name)).unwrap().ino()
        );
    }
    let report = json_output(
        thin(&fixture.source, &["check", text(&destination), "--json"]),
        0,
    );
    assert_eq!(report["changed_paths"], json!([]));
    assert_eq!(report["violations"], json!([]));
}

#[test]
fn native_sha256_nonempty_linked_source_verifies_raw_bytes_and_modes() {
    let fixture = Fixture::with_format("sha256", false);
    if !can_clone(&fixture) {
        return;
    }
    let linked = &fixture.worktree;
    fs::set_permissions(
        linked.join("allowed/file"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("allowed/file", linked.join("link")).unwrap();
    git(linked, &["add", "-A"]);
    git(linked, &["commit", "-qm", "executable and link"]);
    let commit = git(linked, &["rev-parse", "HEAD"]);
    let linked_index = index(linked);
    let source_index = index(&fixture.source);
    let destination = fixture.root.join("native-sha256");
    let result = add(linked, &destination, "native/sha256", Some("allowed"));
    assert_eq!(result["worktree"]["creation_commit"], commit);
    assert_eq!(commit.len(), 64);
    assert_eq!(tree_snapshot(&destination), tree_snapshot(linked));
    assert_eq!(index(linked), linked_index);
    assert_eq!(index(&fixture.source), source_index);
    fs::write(destination.join("outside"), "outside modified\n").unwrap();
    let before = GitSnapshot::read(&destination);
    let check = json_output(thin(linked, &["check", text(&destination), "--json"]), 1);
    assert_eq!(check["violations"], json!(["outside"]));
    assert_eq!(check["changed_paths"], json!(["outside"]));
    assert_eq!(GitSnapshot::read(&destination), before);
    assert_eq!(
        fs::read_to_string(linked.join("outside")).unwrap(),
        "outside baseline\n"
    );
    assert_eq!(
        git(&fixture.source, &["rev-parse", "HEAD"]),
        fixture.baseline
    );
}

#[test]
fn cyclic_and_escaping_link_chains_reject_without_following_or_removing_links() {
    for cycle in [false, true] {
        let fixture = Fixture::new();
        if cycle {
            symlink("link-b", fixture.source.join("link-a")).unwrap();
            symlink("link-a", fixture.source.join("link-b")).unwrap();
        } else {
            fs::create_dir(fixture.source.join("nested")).unwrap();
            symlink("..", fixture.source.join("nested/up")).unwrap();
            // Lexical targets stay inside, but expanding nested/up makes the
            // following '..' escape to the source's parent.
            symlink("nested/up/../sentinel", fixture.source.join("link-a")).unwrap();
            fs::write(fixture.root.join("sentinel"), "must stay outside\n").unwrap();
        }
        git(&fixture.source, &["add", "-A"]);
        git(&fixture.source, &["commit", "-qm", "unsafe chain"]);
        reject_source(&fixture, if cycle { "cycle" } else { "escapes" });
        if !cycle {
            assert_eq!(
                fs::read_to_string(fixture.root.join("sentinel")).unwrap(),
                "must stay outside\n"
            );
        }
    }
}

#[test]
fn native_doctor_and_add_preview_have_json_and_leave_no_lifecycle_effects() {
    let fixture = Fixture::new();
    if !can_clone(&fixture) {
        return;
    }
    let before = GitSnapshot::read(&fixture.source);
    let registration = git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]);
    let journal_before = fs::read(&fixture.journal).unwrap();
    let names = || {
        fs::read_dir(&fixture.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let directories_before = names();
    let doctor = json_output(thin(&fixture.source, &["doctor", "--json"]), 0);
    assert_eq!(doctor["compatible"], true);
    assert_eq!(doctor["commit"], fixture.baseline);
    let destination = fixture.root.join("preview-only");
    let preview = json_output(
        thin(
            &fixture.source,
            &[
                "add",
                text(&destination),
                "--branch",
                "native/preview",
                "--dry-run",
                "--json",
            ],
        ),
        0,
    );
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["scope"], json!([]));
    assert_eq!(preview["creation_commit"], fixture.baseline);
    assert!(!destination.exists());
    assert_eq!(names(), directories_before);
    assert_eq!(GitSnapshot::read(&fixture.source), before);
    assert_eq!(fs::read(&fixture.journal).unwrap(), journal_before);
    assert_eq!(
        git(&fixture.source, &["worktree", "list", "--porcelain", "-z"]),
        registration
    );
}
