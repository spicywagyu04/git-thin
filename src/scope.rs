//! Intended editing scope: final-state validation, never a write barrier.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};
use std::process::{Command, Output, Stdio};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ScopeEntry {
    /// Repository-relative UTF-8 path; `.` denotes the entire repository.
    pub path: String,
    /// Directories include descendants; files match only their exact path.
    pub directory: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckReport {
    pub clean: bool,
    pub changed_paths: Vec<String>,
    pub violations: Vec<String>,
}

/// Resolve scope syntax without following symlinks. A trailing slash declares a
/// directory even when it does not exist yet; other nonexistent paths are files.
pub fn normalize_entries(root: &Path, paths: &[String]) -> Result<Vec<ScopeEntry>, String> {
    let mut result: Vec<ScopeEntry> = Vec::new();
    for input in paths {
        let normalized = normalize_path(input)?;
        let mut directory = normalized == "." || input.ends_with('/');
        if normalized != "." {
            let mut current = root.to_path_buf();
            let components: Vec<_> = Path::new(&normalized).components().collect();
            for (i, component) in components.iter().enumerate() {
                current.push(component.as_os_str());
                let leaf = i + 1 == components.len();
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        if !leaf || directory {
                            return Err(format!(
                                "editing scope {input:?} traverses a symlink; declare the link itself or its repository-relative target instead"
                            ));
                        }
                    }
                    Ok(metadata) if metadata.is_dir() => {
                        if leaf {
                            directory = true;
                        }
                    }
                    Ok(metadata) if metadata.is_file() => {
                        if !leaf || directory {
                            return Err(format!(
                                "editing scope {input:?} uses a file as a directory"
                            ));
                        }
                    }
                    Ok(_) => {
                        return Err(format!(
                            "editing scope {input:?} names an unsupported file type"
                        ))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(error) => {
                        return Err(format!("cannot inspect editing scope {input:?}: {error}"))
                    }
                }
            }
        }
        if let Some(existing) = result.iter_mut().find(|entry| entry.path == normalized) {
            existing.directory |= directory;
        } else {
            result.push(ScopeEntry {
                path: normalized,
                directory,
            });
        }
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

fn normalize_path(input: &str) -> Result<String, String> {
    if input.is_empty() || input.contains('\0') {
        return Err("editing scope paths must be nonempty and cannot contain NUL".into());
    }
    if input.chars().any(|c| matches!(c, '*' | '?' | '[' | ']')) {
        return Err(format!("glob syntax is unsupported in editing scope {input:?}; specify exact files or directories"));
    }
    let mut parts = Vec::new();
    for component in Path::new(input).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or("editing scope paths must be UTF-8")?;
                if part.eq_ignore_ascii_case(".git") {
                    return Err(
                        "editing scope cannot include Git's administrative .git path".into(),
                    );
                }
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "editing scope {input:?} contains '..'; use a repository-relative path"
                ))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "editing scope {input:?} must be repository-relative"
                ))
            }
        }
    }
    Ok(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}

fn allows(entries: &[ScopeEntry], path: &str) -> bool {
    entries.iter().any(|entry| {
        entry.path == path
            || (entry.directory
                && (entry.path == "."
                    || path
                        .strip_prefix(&entry.path)
                        .is_some_and(|suffix| suffix.starts_with('/'))))
    })
}

fn command(root: &Path) -> Command {
    let mut command = crate::git::command(root);
    command.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.ignorestat=false",
        "-c",
        "core.filemode=true",
        "-c",
        "core.symlinks=true",
        "-c",
        "core.sparseCheckout=false",
        "-c",
        "core.splitIndex=false",
        "-c",
        "index.sparse=false",
    ]);
    command
}

fn run(command: &mut Command, operation: &str) -> Result<Output, String> {
    let output = command
        .output()
        .map_err(|error| format!("cannot {operation}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot {operation}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

fn add_paths(paths: &mut BTreeSet<String>, bytes: &[u8]) -> Result<(), String> {
    for path in bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|_| {
            "repository contains a non-UTF-8 filename; this release requires UTF-8 filenames for scope validation"
                .to_owned()
        })?;
        paths.insert(path.to_owned());
    }
    Ok(())
}

/// Reject transformations introduced after creation before any command could
/// run a checkout filter. `check-attr` only inspects attributes; it does not run
/// the filter commands they name.
pub(crate) fn validate_checkout_configuration(
    root: &Path,
    baseline: &[crate::git::Entry],
) -> Result<(), String> {
    let autocrlf = crate::git::text(root, ["config", "--get", "core.autocrlf"]).unwrap_or_default();
    if !matches!(autocrlf.as_str(), "" | "false") {
        return Err("core.autocrlf checkout transformations are unsupported during scope checks; restore an untransformed checkout configuration".into());
    }

    let mut paths = BTreeSet::new();
    for entry in baseline {
        paths.insert(
            entry
                .path
                .to_str()
                .ok_or("non-UTF-8 tracked filename")?
                .to_owned(),
        );
    }
    let head_paths = run(
        command(root).args(["ls-tree", "-rz", "--name-only", "--full-tree", "HEAD"]),
        "enumerate committed paths for checkout attribute validation",
    )?;
    add_paths(&mut paths, &head_paths.stdout)?;
    let index_and_new_paths = run(
        command(root).args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--full-name",
            "-z",
            "--",
        ]),
        "enumerate staged and new paths for checkout attribute validation",
    )?;
    add_paths(&mut paths, &index_and_new_paths.stdout)?;

    let mut child = command(root)
        .args([
            "check-attr",
            "-z",
            "--stdin",
            "filter",
            "working-tree-encoding",
            "ident",
            "eol",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot inspect checkout attributes: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Git attribute-check stdin unavailable")?;
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for path in paths {
            stdin.write_all(path.as_bytes())?;
            stdin.write_all(&[0])?;
        }
        Ok(())
    });
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot inspect checkout attributes: {error}"))?;
    writer
        .join()
        .map_err(|_| "checkout attribute writer failed")?
        .map_err(|error| format!("cannot send paths for checkout attribute validation: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot inspect checkout attributes: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let fields: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    for attribute in fields.as_chunks::<3>().0 {
        if attribute[2] != b"unspecified" && attribute[2] != b"unset" {
            return Err(format!(
                "unsupported checkout attribute {} on {:?}; remove introduced checkout filters or transformations before checking scope",
                String::from_utf8_lossy(attribute[1]), String::from_utf8_lossy(attribute[0])
            ));
        }
    }
    Ok(())
}

/// Inspect physical working files without a Git clean filter, text conversion,
/// index flags, or cached stat fields. Descriptor-relative readers refuse to
/// follow symlink ancestors even if a path changes during the check.
fn working_changes(
    root: &Path,
    baseline: &[crate::git::Entry],
    changed: &mut BTreeSet<String>,
) -> Result<(), String> {
    for entry in baseline {
        let path = entry.path.to_str().ok_or("non-UTF-8 tracked filename")?;
        let mut ancestor = root.to_path_buf();
        let mut missing_or_replaced_ancestor = false;
        for component in entry.path.parent().unwrap_or(Path::new("")).components() {
            ancestor.push(component);
            match fs::symlink_metadata(&ancestor) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    missing_or_replaced_ancestor = true;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing_or_replaced_ancestor = true;
                    break;
                }
                Err(error) => return Err(format!("cannot inspect working path {path:?}: {error}")),
            }
        }
        if missing_or_replaced_ancestor {
            changed.insert(path.to_owned());
            continue;
        }
        let metadata = match fs::symlink_metadata(root.join(&entry.path)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                changed.insert(path.to_owned());
                continue;
            }
            Err(error) => return Err(format!("cannot inspect working path {path:?}: {error}")),
        };
        let actual = if entry.mode == "120000" {
            if !metadata.file_type().is_symlink() {
                changed.insert(path.to_owned());
                continue;
            }
            let target = crate::backend::read_symlink(root, &entry.path)?;
            crate::git::hash_reader(
                &entry.oid,
                target.as_os_str().as_bytes().len() as u64,
                &mut target.as_os_str().as_bytes(),
            )?
        } else {
            if !metadata.is_file() {
                changed.insert(path.to_owned());
                continue;
            }
            let mut file = crate::backend::open_regular(root, &entry.path)?;
            let executable = file
                .metadata()
                .map_err(|error| format!("cannot inspect working file {path:?}: {error}"))?
                .permissions()
                .mode()
                & 0o100
                != 0;
            if executable != (entry.mode == "100755") {
                changed.insert(path.to_owned());
                continue;
            }
            crate::git::hash_reader(
                &entry.oid,
                file.metadata()
                    .map_err(|error| format!("cannot inspect working file {path:?}: {error}"))?
                    .len(),
                &mut file,
            )?
        };
        if actual != entry.oid {
            changed.insert(path.to_owned());
        }
    }
    Ok(())
}

/// Validate the three current snapshots (HEAD, index, working tree) against the
/// fixed creation commit. Working files are hashed as raw bytes, independently
/// of Git's clean filters, index flags, or cached stat information.
pub fn check(root: &Path, baseline: &str, entries: &[ScopeEntry]) -> Result<CheckReport, String> {
    if !matches!(baseline.len(), 40 | 64) || !baseline.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("managed worktree metadata contains an invalid creation commit".into());
    }
    for entry in entries {
        if normalize_path(&entry.path)? != entry.path || (entry.path == "." && !entry.directory) {
            return Err("managed worktree metadata contains an invalid editing scope".into());
        }
    }
    let ancestor = command(root)
        .args(["merge-base", "--is-ancestor", baseline, "HEAD"])
        .output()
        .map_err(|error| format!("cannot validate creation history: {error}"))?;
    if !ancestor.status.success() {
        if ancestor.status.code() == Some(1) {
            return Err(format!("incompatible history: HEAD no longer descends from creation commit {baseline}; restore compatible history before checking scope"));
        }
        return Err(format!(
            "cannot validate creation history: {}",
            String::from_utf8_lossy(&ancestor.stderr).trim()
        ));
    }

    let baseline_entries = crate::git::tree(root, baseline)?;
    validate_checkout_configuration(root, &baseline_entries)?;
    let mut changed = BTreeSet::new();
    let diff_options = [
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--name-only",
        "-z",
    ];
    let head = run(
        command(root)
            .arg("diff")
            .args(diff_options)
            .args([baseline, "HEAD", "--"]),
        "compare committed changes with the creation commit",
    )?;
    add_paths(&mut changed, &head.stdout)?;
    // Both index views are needed: visible intent-to-add entries expose new
    // additions, while invisible entries preserve staged deletions of baseline
    // files whose empty blobs would otherwise match the intent-to-add entries.
    // Both options are supported by the minimum Git version (2.39).
    for intent_view in ["--ita-visible-in-index", "--ita-invisible-in-index"] {
        let index = run(
            command(root).arg("diff").args(diff_options).args([
                "--cached",
                intent_view,
                baseline,
                "--",
            ]),
            "compare staged changes with the creation commit",
        )?;
        add_paths(&mut changed, &index.stdout)?;
    }

    working_changes(root, &baseline_entries, &mut changed)?;

    let untracked = run(
        command(root).args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "--full-name",
            "-z",
            "--",
        ]),
        "inspect new non-ignored files",
    )?;
    add_paths(&mut changed, &untracked.stdout)?;

    let changed_paths: Vec<String> = changed.into_iter().collect();
    let violations: Vec<String> = changed_paths
        .iter()
        .filter(|path| !allows(entries, path))
        .cloned()
        .collect();
    Ok(CheckReport {
        clean: violations.is_empty(),
        changed_paths,
        violations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn repository() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.name", "Scope Test"]);
        git(
            dir.path(),
            &["config", "user.email", "scope@example.invalid"],
        );
        fs::create_dir(dir.path().join("allowed")).unwrap();
        fs::write(dir.path().join("allowed/source"), "initial\n").unwrap();
        fs::write(dir.path().join("outside"), "initial\n").unwrap();
        fs::write(dir.path().join(".gitignore"), "build/\nignored-tracked\n").unwrap();
        git(dir.path(), &["add", "."]);
        fs::write(dir.path().join("ignored-tracked"), "initial\n").unwrap();
        git(dir.path(), &["add", "-f", "ignored-tracked"]);
        git(dir.path(), &["commit", "-qm", "baseline"]);
        let baseline = git(dir.path(), &["rev-parse", "HEAD"]);
        (dir, baseline)
    }

    fn allowed(root: &Path) -> Vec<ScopeEntry> {
        normalize_entries(root, &["allowed".into()]).unwrap()
    }

    #[test]
    fn clean_worktree_and_ignored_output_pass() {
        let (dir, baseline) = repository();
        fs::create_dir(dir.path().join("build")).unwrap();
        fs::write(dir.path().join("build/output"), "generated").unwrap();
        assert!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .clean
        );
    }

    #[test]
    fn directory_scope_has_a_component_boundary() {
        let (dir, baseline) = repository();
        fs::write(dir.path().join("allowed/new"), "yes").unwrap();
        fs::write(dir.path().join("allowed-sibling"), "no").unwrap();
        let result = check(dir.path(), &baseline, &allowed(dir.path())).unwrap();
        assert_eq!(result.violations, ["allowed-sibling"]);
        assert!(result.changed_paths.contains(&"allowed/new".into()));
    }

    #[test]
    fn detects_committed_changes_and_rename_both_sides() {
        let (dir, baseline) = repository();
        git(dir.path(), &["mv", "outside", "allowed/moved"]);
        git(dir.path(), &["commit", "-qm", "rename"]);
        let result = check(dir.path(), &baseline, &allowed(dir.path())).unwrap();
        assert_eq!(result.violations, ["outside"]);
        assert!(result.changed_paths.contains(&"allowed/moved".into()));
    }

    #[test]
    fn checks_index_even_when_working_file_is_restored() {
        let (dir, baseline) = repository();
        fs::write(dir.path().join("outside"), "staged\n").unwrap();
        git(dir.path(), &["add", "outside"]);
        fs::write(dir.path().join("outside"), "initial\n").unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["outside"]
        );
    }

    #[test]
    fn rejects_introduced_filters_without_running_them() {
        let (dir, baseline) = repository();
        git(
            dir.path(),
            &[
                "config",
                "filter.hide.clean",
                "touch filter-ran; printf 'initial\\n'",
            ],
        );
        fs::write(dir.path().join(".gitattributes"), "outside filter=hide\n").unwrap();
        fs::write(dir.path().join("outside"), "hidden edit\n").unwrap();
        let scope =
            normalize_entries(dir.path(), &["allowed".into(), ".gitattributes".into()]).unwrap();
        let error = check(dir.path(), &baseline, &scope).unwrap_err();
        assert!(
            error.contains("unsupported checkout attribute filter"),
            "{error}"
        );
        assert!(!dir.path().join("filter-ran").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("outside")).unwrap(),
            "hidden edit\n"
        );
    }

    #[test]
    fn rejects_introduced_checkout_transformations() {
        let (dir, baseline) = repository();
        for attribute in ["working-tree-encoding=UTF-16", "ident", "eol=crlf"] {
            fs::write(
                dir.path().join(".gitattributes"),
                format!("outside {attribute}\n"),
            )
            .unwrap();
            assert!(check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap_err()
                .contains("unsupported checkout attribute"));
        }
        fs::remove_file(dir.path().join(".gitattributes")).unwrap();
        git(dir.path(), &["config", "core.autocrlf", "input"]);
        assert!(check(dir.path(), &baseline, &allowed(dir.path()))
            .unwrap_err()
            .contains("core.autocrlf"));
    }

    #[test]
    fn validates_attributes_on_new_index_and_untracked_paths() {
        let (dir, baseline) = repository();
        fs::write(dir.path().join("new-staged"), "new\n").unwrap();
        git(dir.path(), &["add", "new-staged"]);
        fs::write(dir.path().join("new-untracked"), "new\n").unwrap();
        git(
            dir.path(),
            &["config", "filter.hide.clean", "touch filter-ran; cat"],
        );
        for path in ["new-staged", "new-untracked"] {
            fs::write(
                dir.path().join(".gitattributes"),
                format!("{path} filter=hide\n"),
            )
            .unwrap();
            let error = check(dir.path(), &baseline, &allowed(dir.path())).unwrap_err();
            assert!(error.contains(path), "{error}");
            assert!(!dir.path().join("filter-ran").exists());
        }
    }

    #[test]
    fn text_normalization_cannot_hide_raw_working_file_edits() {
        let (dir, baseline) = repository();
        fs::write(dir.path().join(".gitattributes"), "outside text\n").unwrap();
        fs::write(dir.path().join("outside"), "initial\r\n").unwrap();
        let scope =
            normalize_entries(dir.path(), &["allowed".into(), ".gitattributes".into()]).unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &scope).unwrap().violations,
            ["outside"]
        );
    }

    #[test]
    fn new_tracked_paths_remain_checked_after_commits_and_working_edits() {
        let (dir, baseline) = repository();
        fs::write(dir.path().join("new-committed"), "new\n").unwrap();
        git(dir.path(), &["add", "new-committed"]);
        git(dir.path(), &["commit", "-qm", "add outside file"]);
        fs::write(dir.path().join("new-committed"), "changed after commit\n").unwrap();
        fs::write(dir.path().join("new-staged"), "new staged\n").unwrap();
        git(dir.path(), &["add", "new-staged"]);
        fs::remove_file(dir.path().join("new-staged")).unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["new-committed", "new-staged"]
        );
    }

    #[test]
    fn catches_staged_additions_and_deletions() {
        let (dir, baseline) = repository();
        fs::write(dir.path().join("new-staged"), "new\n").unwrap();
        git(dir.path(), &["add", "new-staged"]);
        git(dir.path(), &["rm", "outside"]);
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["new-staged", "outside"]
        );
    }

    #[test]
    fn empty_baseline_intent_to_add_cannot_hide_staged_deletions() {
        let (dir, _) = repository();
        let names = ["allowed/empty", "outside-empty", "-empty\t雪\nfile"];
        for name in names {
            fs::write(dir.path().join(name), b"").unwrap();
        }
        git(
            dir.path(),
            &[
                "add",
                "--",
                "allowed/empty",
                "outside-empty",
                "-empty\t雪\nfile",
            ],
        );
        git(dir.path(), &["commit", "-qm", "empty baseline files"]);
        let baseline = git(dir.path(), &["rev-parse", "HEAD"]);
        git(
            dir.path(),
            &[
                "rm",
                "--cached",
                "--",
                "allowed/empty",
                "outside-empty",
                "-empty\t雪\nfile",
            ],
        );
        git(
            dir.path(),
            &[
                "add",
                "--intent-to-add",
                "--",
                "allowed/empty",
                "outside-empty",
                "-empty\t雪\nfile",
            ],
        );
        let before = fs::read(dir.path().join(".git/index")).unwrap();
        let result = check(dir.path(), &baseline, &allowed(dir.path())).unwrap();
        assert_eq!(result.violations, ["-empty\t雪\nfile", "outside-empty"]);
        assert_eq!(
            result.changed_paths,
            ["-empty\t雪\nfile", "allowed/empty", "outside-empty"]
        );
        assert_eq!(fs::read(dir.path().join(".git/index")).unwrap(), before);
        assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), baseline);
        for name in names {
            assert!(fs::read(dir.path().join(name)).unwrap().is_empty());
        }
        git(
            dir.path(),
            &[
                "add",
                "--",
                "allowed/empty",
                "outside-empty",
                "-empty\t雪\nfile",
            ],
        );
        let restored = check(dir.path(), &baseline, &allowed(dir.path())).unwrap();
        assert!(restored.clean);
        assert!(restored.changed_paths.is_empty());
    }

    #[test]
    fn intent_to_add_is_a_change_even_when_the_working_file_disappears() {
        let (dir, baseline) = repository();
        for name in ["empty-new", "outside-new", "allowed/new", "-new\t雪\nfile"] {
            let bytes = if name == "empty-new" {
                ""
            } else {
                "new bytes\n"
            };
            fs::write(dir.path().join(name), bytes).unwrap();
        }
        let expected = ["-new\t雪\nfile", "empty-new", "outside-new"];
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            expected
        );
        git(
            dir.path(),
            &[
                "add",
                "--intent-to-add",
                "--",
                "empty-new",
                "outside-new",
                "allowed/new",
                "-new\t雪\nfile",
            ],
        );
        let index_before = fs::read(dir.path().join(".git/index")).unwrap();
        let head_before = git(dir.path(), &["rev-parse", "HEAD"]);
        let report = check(dir.path(), &baseline, &allowed(dir.path())).unwrap();
        assert_eq!(report.violations, expected);
        assert!(report.changed_paths.contains(&"allowed/new".to_owned()));
        assert_eq!(
            fs::read(dir.path().join(".git/index")).unwrap(),
            index_before
        );
        assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            fs::read_to_string(dir.path().join("outside-new")).unwrap(),
            "new bytes\n"
        );

        // The index snapshot still contains an intended addition after its
        // working file disappears. It remains changed until reset as well.
        fs::remove_file(dir.path().join("outside-new")).unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            expected
        );
        assert_eq!(
            fs::read(dir.path().join(".git/index")).unwrap(),
            index_before
        );
        git(
            dir.path(),
            &["add", "--", "empty-new", "allowed/new", "-new\t雪\nfile"],
        );
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            expected
        );
        git(
            dir.path(),
            &[
                "reset",
                "-q",
                "HEAD",
                "--",
                "empty-new",
                "outside-new",
                "allowed/new",
                "-new\t雪\nfile",
            ],
        );
        for name in ["empty-new", "allowed/new", "-new\t雪\nfile"] {
            fs::remove_file(dir.path().join(name)).unwrap();
        }
        let restored = check(dir.path(), &baseline, &allowed(dir.path())).unwrap();
        assert!(restored.clean);
        assert!(restored.changed_paths.is_empty());
    }

    #[test]
    fn ignores_temporary_changes_after_full_restoration() {
        let (dir, baseline) = repository();
        fs::write(dir.path().join("outside"), "temporary\n").unwrap();
        git(dir.path(), &["add", "outside"]);
        git(dir.path(), &["commit", "-qm", "change"]);
        git(dir.path(), &["revert", "--no-edit", "HEAD"]);
        assert!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .clean
        );
    }

    #[test]
    fn detects_deleted_and_ignored_tracked_files() {
        let (dir, baseline) = repository();
        fs::remove_file(dir.path().join("outside")).unwrap();
        fs::write(dir.path().join("ignored-tracked"), "edited").unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["ignored-tracked", "outside"]
        );
    }

    #[test]
    fn flags_cannot_hide_working_file_changes_or_modify_live_index() {
        let (dir, baseline) = repository();
        git(
            dir.path(),
            &["update-index", "--assume-unchanged", "outside"],
        );
        git(
            dir.path(),
            &["update-index", "--skip-worktree", "ignored-tracked"],
        );
        let original_index = fs::read(dir.path().join(".git/index")).unwrap();
        fs::write(dir.path().join("outside"), "changed\n").unwrap();
        fs::write(dir.path().join("ignored-tracked"), "changed\n").unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["ignored-tracked", "outside"]
        );
        assert_eq!(
            fs::read(dir.path().join(".git/index")).unwrap(),
            original_index
        );
    }

    #[test]
    fn preserves_spaces_unicode_and_newlines() {
        let (dir, baseline) = repository();
        for name in ["space name", "üñîçødé", "new\nline"] {
            fs::write(dir.path().join(name), "new").unwrap();
        }
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["new\nline", "space name", "üñîçødé"]
        );
    }

    #[test]
    fn rejects_incompatible_history() {
        let (dir, baseline) = repository();
        git(dir.path(), &["checkout", "--orphan", "unrelated"]);
        git(dir.path(), &["commit", "-qm", "unrelated root"]);
        assert!(check(dir.path(), &baseline, &allowed(dir.path()))
            .unwrap_err()
            .contains("incompatible history"));
    }

    #[test]
    fn scope_normalization_is_explicit_and_safe() {
        let (dir, _) = repository();
        let entries = normalize_entries(
            dir.path(),
            &["./allowed/".into(), "future/".into(), "new-file".into()],
        )
        .unwrap();
        assert_eq!(
            entries,
            vec![
                ScopeEntry {
                    path: "allowed".into(),
                    directory: true
                },
                ScopeEntry {
                    path: "future".into(),
                    directory: true
                },
                ScopeEntry {
                    path: "new-file".into(),
                    directory: false
                }
            ]
        );
        for invalid in [
            "",
            "../escape",
            "a/../escape",
            "/outside",
            "*.rs",
            ".git/config",
            "outside/child",
        ] {
            assert!(
                normalize_entries(dir.path(), &[invalid.into()]).is_err(),
                "{invalid}"
            );
        }
        let all = normalize_entries(dir.path(), &[".".into()]).unwrap();
        assert!(allows(&all, "any/path"));
    }

    #[cfg(unix)]
    #[test]
    fn scopes_never_traverse_symlinks_and_link_replacement_is_detected() {
        use std::os::unix::fs::symlink;
        let (dir, baseline) = repository();
        symlink("allowed", dir.path().join("link")).unwrap();
        assert!(normalize_entries(dir.path(), &["link/source".into()]).is_err());
        assert!(normalize_entries(dir.path(), &["link/".into()]).is_err());
        assert!(!normalize_entries(dir.path(), &["link".into()]).unwrap()[0].directory);
        fs::remove_file(dir.path().join("outside")).unwrap();
        symlink("allowed/source", dir.path().join("outside")).unwrap();
        let result = check(dir.path(), &baseline, &allowed(dir.path())).unwrap();
        assert!(result.violations.contains(&"outside".into()));
    }

    #[cfg(unix)]
    #[test]
    fn detects_executable_bit_changes_even_if_git_ignores_filemode() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, baseline) = repository();
        git(dir.path(), &["config", "core.filemode", "false"]);
        fs::set_permissions(
            dir.path().join("outside"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["outside"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn tracked_symlinks_are_checked_by_link_contents() {
        use std::os::unix::fs::symlink;
        let (dir, _) = repository();
        symlink("allowed/source", dir.path().join("link")).unwrap();
        git(dir.path(), &["add", "link"]);
        git(dir.path(), &["commit", "-qm", "link baseline"]);
        let baseline = git(dir.path(), &["rev-parse", "HEAD"]);
        fs::write(dir.path().join("allowed/source"), "allowed edit\n").unwrap();
        assert!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .clean
        );
        fs::remove_file(dir.path().join("link")).unwrap();
        symlink("outside", dir.path().join("link")).unwrap();
        assert_eq!(
            check(dir.path(), &baseline, &allowed(dir.path()))
                .unwrap()
                .violations,
            ["link"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_fail_explicitly() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let (dir, baseline) = repository();
        // APFS rejects malformed UTF-8 names before scope validation can inspect them.
        if let Err(error) = fs::write(dir.path().join(OsStr::from_bytes(b"nonutf8-\xff")), "new") {
            if error.raw_os_error() == Some(libc::EILSEQ) {
                eprintln!(
                    "INAPPLICABLE non-UTF-8 scope test: filesystem rejects non-UTF-8 filenames: {error}"
                );
                return;
            }
            panic!("cannot create non-UTF-8 scope fixture: {error}");
        }
        assert!(check(dir.path(), &baseline, &allowed(dir.path()))
            .unwrap_err()
            .contains("non-UTF-8"));
    }
}
