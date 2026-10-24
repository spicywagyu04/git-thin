//! Scope and command contracts use ordinary Git worktrees, independently of cloning.
#![cfg(unix)]

mod support;

use serde_json::json;
use std::fs;
use support::*;

#[test]
fn empty_baseline_intent_to_add_preserves_staged_deletion_violation() {
    let fixture = Fixture::new();
    git(&fixture.worktree, &["rm", "--cached", "outside-empty"]);
    fixture.assert_check(&["outside-empty"], &["outside-empty"]);
    git(
        &fixture.worktree,
        &["add", "--intent-to-add", "outside-empty"],
    );
    assert_eq!(
        git(&fixture.worktree, &["diff", "--cached", "--name-status"]),
        "D\toutside-empty"
    );
    let before = GitSnapshot::read(&fixture.worktree);
    let source_before = GitSnapshot::read(&fixture.source);
    fixture.assert_check(&["outside-empty"], &["outside-empty"]);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    assert_eq!(GitSnapshot::read(&fixture.source), source_before);
    assert!(fs::read(fixture.worktree.join("outside-empty"))
        .unwrap()
        .is_empty());

    fs::remove_file(fixture.worktree.join("outside-empty")).unwrap();
    fixture.assert_check(&["outside-empty"], &["outside-empty"]);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    fs::write(fixture.worktree.join("outside-empty"), b"").unwrap();
    fixture.assert_check(&["outside-empty"], &["outside-empty"]);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);

    fixture.scope(&["outside-empty"]);
    fixture.assert_check(&[], &["outside-empty"]);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    assert_eq!(fixture.metadata()["creation_commit"], fixture.baseline);

    // Normal staging removes the intent flag and restores the baseline index.
    git(&fixture.worktree, &["add", "outside-empty"]);
    fixture.assert_check(&[], &[]);
    assert_eq!(GitSnapshot::read(&fixture.source), source_before);
}

#[test]
fn intent_to_add_cli_keeps_violations_and_preserves_all_snapshots() {
    let fixture = Fixture::new();
    let names = [
        "--json",
        "-leading",
        "allowed/empty",
        "allowed/new",
        "empty",
        "tab\t雪\nfile",
    ];
    for name in names {
        fs::write(
            fixture.worktree.join(name),
            if name.ends_with("empty") {
                ""
            } else {
                "new bytes\n"
            },
        )
        .unwrap();
    }
    let violations = ["--json", "-leading", "empty", "tab\t雪\nfile"];
    fixture.assert_check(&violations, &names);
    let mut args = vec!["add", "--intent-to-add", "--"];
    args.extend(names);
    git(&fixture.worktree, &args);
    let before = GitSnapshot::read(&fixture.worktree);
    let source_before = GitSnapshot::read(&fixture.source);
    fixture.assert_check(&violations, &names);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    assert_eq!(GitSnapshot::read(&fixture.source), source_before);
    for name in names {
        assert_eq!(
            fs::read_to_string(fixture.worktree.join(name)).unwrap(),
            if name.ends_with("empty") {
                ""
            } else {
                "new bytes\n"
            }
        );
    }

    fs::remove_file(fixture.worktree.join("empty")).unwrap();
    fixture.assert_check(&violations, &names);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    fixture.scope(&["empty", "tab\t雪\nfile"]);
    fixture.assert_check(&["--json", "-leading"], &names);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    assert_eq!(fixture.metadata()["creation_commit"], fixture.baseline);

    git(
        &fixture.worktree,
        &[
            "add",
            "--",
            "--json",
            "-leading",
            "allowed/empty",
            "allowed/new",
            "tab\t雪\nfile",
        ],
    );
    fixture.assert_check(&["--json", "-leading"], &names);
    git(
        &fixture.worktree,
        &[
            "reset",
            "-q",
            "HEAD",
            "--",
            "--json",
            "-leading",
            "allowed/empty",
            "allowed/new",
            "empty",
            "tab\t雪\nfile",
        ],
    );
    for name in names.into_iter().filter(|name| *name != "empty") {
        fs::remove_file(fixture.worktree.join(name)).unwrap();
    }
    fixture.assert_check(&[], &[]);
}

#[test]
fn no_declared_writes_and_root_scope_have_explicit_meanings() {
    let fixture = Fixture::new();
    let mut metadata = fixture.metadata();
    metadata["scope"] = json!([]);
    fs::write(&fixture.journal, serde_json::to_vec(&metadata).unwrap()).unwrap();
    fixture.assert_check(&[], &[]);
    fs::write(fixture.worktree.join("allowed/file"), "edited\n").unwrap();
    fs::write(fixture.worktree.join("outside"), "edited\n").unwrap();
    fixture.assert_check(&["allowed/file", "outside"], &["allowed/file", "outside"]);
    fixture.scope(&["."]);
    fixture.assert_check(&[], &["allowed/file", "outside"]);
}

#[test]
fn future_file_scope_is_exact_and_trailing_slash_scope_is_recursive() {
    let fixture = Fixture::new();
    fixture.scope(&["future-file", "future-directory/"]);
    fs::create_dir(fixture.worktree.join("future-file")).unwrap();
    fs::create_dir(fixture.worktree.join("future-directory")).unwrap();
    fs::write(fixture.worktree.join("future-file/child"), "not allowed").unwrap();
    fs::write(fixture.worktree.join("future-directory/child"), "allowed").unwrap();
    fixture.assert_check(
        &["future-file/child"],
        &["future-directory/child", "future-file/child"],
    );
    fs::remove_file(fixture.worktree.join("future-file/child")).unwrap();
    fs::remove_dir(fixture.worktree.join("future-file")).unwrap();
    fs::write(fixture.worktree.join("future-file"), "exact file").unwrap();
    fixture.assert_check(&[], &["future-directory/child", "future-file"]);
}

#[test]
fn replacements_report_deleted_baseline_paths_and_added_paths() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.worktree.join("outside")).unwrap();
    fs::create_dir(fixture.worktree.join("outside")).unwrap();
    fs::write(fixture.worktree.join("outside/child"), "replacement").unwrap();
    fixture.assert_check(&["outside", "outside/child"], &["outside", "outside/child"]);
    fs::remove_file(fixture.worktree.join("allowed/file")).unwrap();
    fs::remove_dir(fixture.worktree.join("allowed")).unwrap();
    std::os::unix::fs::symlink("outside", fixture.worktree.join("allowed")).unwrap();
    fixture.assert_check(
        &["outside", "outside/child"],
        &["allowed", "allowed/file", "outside", "outside/child"],
    );
}

#[test]
fn rename_crossing_each_boundary_reports_both_sides() {
    for (from, to, violations, changed) in [
        (
            "allowed/file",
            "elsewhere",
            vec!["elsewhere"],
            vec!["allowed/file", "elsewhere"],
        ),
        (
            "outside",
            "allowed/moved",
            vec!["outside"],
            vec!["allowed/moved", "outside"],
        ),
        (
            "outside",
            "elsewhere",
            vec!["elsewhere", "outside"],
            vec!["elsewhere", "outside"],
        ),
    ] {
        let fixture = Fixture::new();
        git(&fixture.worktree, &["mv", from, to]);
        let before = GitSnapshot::read(&fixture.worktree);
        fixture.assert_check(&violations, &changed);
        assert_eq!(GitSnapshot::read(&fixture.worktree), before);
        git(&fixture.worktree, &["commit", "-qm", "rename"]);
        fixture.assert_check(&violations, &changed);
    }
}

#[test]
fn json_success_contract_and_global_flag_positions() {
    let fixture = Fixture::new();
    for args in [
        vec!["--json", "check", text(&fixture.worktree)],
        vec!["check", "--json", text(&fixture.worktree)],
        vec!["check", text(&fixture.worktree), "--json"],
    ] {
        let result = json_output(thin(&fixture.source, &args), 0);
        assert_eq!(
            result,
            json!({"clean": true, "changed_paths": [], "violations": []})
        );
    }
    let status = json_output(
        thin(
            &fixture.source,
            &["status", text(&fixture.worktree), "--json"],
        ),
        0,
    );
    assert_eq!(status["scope_check"]["clean"], true);
    assert_eq!(status["worktree"]["path"], text(&fixture.worktree));
    let listed = json_output(thin(&fixture.source, &["list", "--json"]), 0);
    assert_eq!(listed["worktrees"].as_array().unwrap().len(), 1);
    assert_eq!(listed["worktrees"][0]["creation_commit"], fixture.baseline);
    fixture.scope(&["outside"]);
    for args in [
        vec!["recover", "--json", "--dry-run"],
        vec!["--json", "recover"],
    ] {
        let result = json_output(thin(&fixture.source, &args), 0);
        assert!(result["recovered"].as_array().unwrap().is_empty());
    }
    let before = GitSnapshot::read(&fixture.worktree);
    let journal = fs::read(&fixture.journal).unwrap();
    let preview = json_output(
        thin(
            &fixture.source,
            &["remove", text(&fixture.worktree), "--dry-run", "--json"],
        ),
        0,
    );
    assert_eq!(preview["dry_run"], true);
    assert_eq!(GitSnapshot::read(&fixture.worktree), before);
    assert_eq!(fs::read(&fixture.journal).unwrap(), journal);
    assert!(fixture.worktree.join("outside").exists());
    let removed = json_output(
        thin(
            &fixture.source,
            &["--json", "remove", text(&fixture.worktree)],
        ),
        0,
    );
    assert_eq!(removed["removed"], text(&fixture.worktree));
    assert_eq!(removed["branch_preserved"], "agent/test");
    assert!(!fixture.worktree.exists());
    assert!(!fixture.journal.exists());
    assert_eq!(
        git(&fixture.source, &["rev-parse", "agent/test"]),
        fixture.baseline
    );
}

#[test]
fn json_usage_errors_are_structured_and_help_and_version_are_text() {
    let fixture = Fixture::new();
    for args in [
        vec!["--json"],
        vec!["--json", "unknown-command"],
        vec!["add", "--json"],
        vec!["check", "--json"],
        vec!["scope", "add", text(&fixture.worktree), "--json"],
        vec!["list", "--unknown", "--json"],
    ] {
        json_output(thin(&fixture.source, &args), 2);
    }
    for flag in ["--help", "--version"] {
        let output = thin(&fixture.source, &["--json", flag]);
        assert!(output.status.success(), "{}", diagnostic(&output));
        assert!(String::from_utf8_lossy(&output.stdout).contains("git thin"));
        assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err());
    }
}

#[test]
fn every_subcommand_reports_missing_repository_as_json() {
    let outside = tempfile::tempdir().unwrap();
    for args in [
        vec!["doctor", "--json"],
        vec!["list", "--json"],
        vec!["recover", "--json"],
        vec!["add", "new", "--branch", "branch", "--json"],
        vec!["check", "missing", "--json"],
        vec!["status", "missing", "--json"],
        vec!["remove", "missing", "--json"],
        vec!["scope", "add", "missing", ".", "--json"],
    ] {
        let result = json_output(thin(outside.path(), &args), 2);
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("not a git repository"));
    }
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn invalid_worktrees_branches_and_destinations_preserve_repository() {
    let fixture = Fixture::new();
    let before = GitSnapshot::read(&fixture.source);
    let target_before = GitSnapshot::read(&fixture.worktree);
    let destination = fixture.root.join("new");
    let missing = fixture.root.join("missing");
    for args in [
        vec!["check", text(&missing), "--json"],
        vec!["status", text(&missing), "--json"],
        vec!["remove", text(&missing), "--json"],
        vec!["scope", "add", text(&missing), ".", "--json"],
        vec![
            "add",
            text(&destination),
            "--branch",
            "bad..branch",
            "--json",
        ],
        vec!["add", text(&destination), "--branch", "main", "--json"],
        vec!["add", text(&fixture.worktree), "--branch", "new", "--json"],
        vec![
            "scope",
            "add",
            text(&fixture.worktree),
            "../escape",
            "--json",
        ],
    ] {
        json_output(thin(&fixture.source, &args), 2);
        assert_eq!(GitSnapshot::read(&fixture.source), before);
        assert_eq!(GitSnapshot::read(&fixture.worktree), target_before);
        assert!(!destination.exists());
    }
}
