//! Cleanup invariants exercised with ordinary Git worktrees, independent of cloning.
#![cfg(unix)]
#[allow(dead_code)]
mod support;
use std::fs;
use support::{git, json_output, text, thin, Fixture, GitSnapshot};

#[test]
fn removal_refuses_staged_only_and_flag_hidden_edits_without_changing_snapshots() {
    for mode in ["staged", "assume-unchanged", "skip-worktree", "stat-cache"] {
        let f = Fixture::new();
        let file = f.worktree.join("outside");
        let original = fs::read(&file).unwrap();
        let times = fs::metadata(&file).unwrap();
        if mode == "assume-unchanged" || mode == "skip-worktree" {
            git(
                &f.worktree,
                &["update-index", &format!("--{mode}"), "outside"],
            );
        }
        fs::write(&file, b"changed baseline\n").unwrap();
        if mode == "staged" {
            git(&f.worktree, &["add", "outside"]);
            fs::write(&file, &original).unwrap();
        }
        if mode == "stat-cache" {
            fs::File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_times(
                    fs::FileTimes::new()
                        .set_accessed(times.accessed().unwrap())
                        .set_modified(times.modified().unwrap()),
                )
                .unwrap();
        }
        let before = GitSnapshot::read(&f.worktree);
        let bytes = fs::read(&file).unwrap();
        let journal = fs::read(&f.journal).unwrap();
        for dry in [true, false] {
            let mut args = vec!["remove", text(&f.worktree), "--json"];
            if dry {
                args.push("--dry-run");
            }
            json_output(thin(&f.source, &args), 2);
            assert_eq!(GitSnapshot::read(&f.worktree), before, "{mode}");
            assert_eq!(fs::read(&file).unwrap(), bytes);
            assert_eq!(fs::read(&f.journal).unwrap(), journal);
        }
    }
}

#[test]
fn branch_switch_and_rename_preserve_actual_retaining_refs_on_removal() {
    for rename in [true, false] {
        let f = Fixture::new();
        if rename {
            git(&f.worktree, &["branch", "-m", "renamed"]);
        } else {
            git(&f.worktree, &["switch", "-c", "switched"]);
        }
        fs::write(f.worktree.join("allowed/file"), b"committed change\n").unwrap();
        git(&f.worktree, &["commit", "-am", "retained"]);
        let refs = git(
            &f.source,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );
        json_output(
            thin(
                &f.source,
                &["remove", text(&f.worktree), "--dry-run", "--json"],
            ),
            0,
        );
        assert!(f.worktree.exists());
        json_output(thin(&f.source, &["remove", text(&f.worktree), "--json"]), 0);
        assert!(!f.worktree.exists());
        assert!(!f.journal.exists());
        assert_eq!(
            git(
                &f.source,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
            refs
        );
    }
}

#[test]
fn git_locked_worktree_is_retained_until_explicitly_unlocked() {
    let f = Fixture::new();
    git(
        &f.source,
        &[
            "worktree",
            "lock",
            "--reason",
            "preserve this checkout",
            text(&f.worktree),
        ],
    );
    let before = GitSnapshot::read(&f.worktree);
    let journal = fs::read(&f.journal).unwrap();
    let preview = json_output(
        thin(
            &f.source,
            &["remove", text(&f.worktree), "--dry-run", "--json"],
        ),
        2,
    );
    assert!(preview["error"].as_str().unwrap().contains("locked"));
    assert_eq!(fs::read(&f.journal).unwrap(), journal);
    let output = json_output(thin(&f.source, &["remove", text(&f.worktree), "--json"]), 2);
    assert!(output["error"].as_str().unwrap().contains("locked"));
    assert_eq!(GitSnapshot::read(&f.worktree), before);
    assert_eq!(fs::read(&f.journal).unwrap(), journal);
    assert!(f.worktree.exists());
    git(&f.source, &["worktree", "unlock", text(&f.worktree)]);
    json_output(thin(&f.source, &["remove", text(&f.worktree), "--json"]), 0);
}
