use crate::{
    backend,
    git::{self, Repo},
    scope::{self, ScopeEntry},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

const SCHEMA: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub schema_version: u32,
    pub id: String,
    pub creation_commit: String,
    pub branch: String,
    pub backend: String,
    pub scope: Vec<ScopeEntry>,
    pub state: String,
    pub path: PathBuf,
    pub staging_path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub clone_ms: u128,
    pub verification_ms: u128,
}

struct Store {
    dir: PathBuf,
    _lock: File,
}
impl Store {
    fn open(repo: &Repo) -> Result<Self, String> {
        let dir = repo.common.join("git-thin");
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("operations.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|_| "another git-thin operation is running; retry when it finishes")?;
        Ok(Self { dir, _lock: lock })
    }
    fn save(&self, metadata: &Metadata) -> Result<(), String> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir).map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(&mut temp, metadata).map_err(|e| e.to_string())?;
        temp.write_all(b"\n").map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist(self.dir.join(format!("{}.json", metadata.id)))
            .map_err(|e| e.to_string())?;
        git::sync_directory(&self.dir)
    }
    fn records(&self) -> Result<Vec<Metadata>, String> {
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let meta: Metadata =
                serde_json::from_reader(File::open(&path).map_err(|e| e.to_string())?).map_err(
                    |e| format!("invalid metadata {path:?}: {e}; preserve it for manual recovery"),
                )?;
            if meta.schema_version != SCHEMA
                || uuid::Uuid::parse_str(&meta.id).is_err()
                || path.file_stem().and_then(|s| s.to_str()) != Some(&meta.id)
            {
                return Err(format!("unsupported or invalid metadata {path:?}; upgrade git-thin or recover manually"));
            }
            result.push(meta);
        }
        result.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(result)
    }
    fn delete(&self, metadata: &Metadata) -> Result<(), String> {
        fs::remove_file(self.dir.join(format!("{}.json", metadata.id)))
            .map_err(|e| e.to_string())?;
        git::sync_directory(&self.dir)
    }
    fn find(&self, path: &Path) -> Result<Metadata, String> {
        let path = absolute_target(path)?;
        self.records()?
            .into_iter()
            .find(|m| m.path == path)
            .ok_or_else(|| format!("not a managed worktree: {path:?}"))
    }
}

fn absolute_target(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let name = absolute
        .file_name()
        .ok_or("destination must name a directory")?;
    if name == "." || name == ".." {
        return Err("destination must name a directory".into());
    }
    let parent = absolute
        .parent()
        .ok_or("destination needs a parent directory")?;
    let parent =
        fs::canonicalize(parent).map_err(|e| format!("destination parent must exist: {e}"))?;
    Ok(parent.join(name))
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
fn owns(meta: &Metadata, path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_dir() && m.dev() == meta.device && m.ino() == meta.inode)
}
fn assert_registered(repo: &Repo, meta: &Metadata, path: &Path) -> Result<(), String> {
    if !owns(meta, path) {
        return Err(format!(
            "worktree directory identity changed: {path:?}; refusing to modify it"
        ));
    }
    let discovered = Repo::discover(path)?;
    if discovered.root != path || discovered.common != repo.common || !path.join(".git").is_file() {
        return Err(format!(
            "worktree registration does not match this repository: {path:?}"
        ));
    }
    Ok(())
}
fn require_ready(repo: &Repo, meta: &Metadata) -> Result<(), String> {
    if meta.state != "ready" {
        return Err("unfinished operation; run git thin recover first".into());
    }
    assert_registered(repo, meta, &meta.path)
}

pub fn doctor(repo: &Repo) -> Result<Value, String> {
    let (commit, entries) = git::validate_source(repo)?;
    backend::probe(
        &repo.root,
        repo.root.parent().ok_or("source has no parent")?,
    )?;
    Ok(
        json!({"compatible":true,"source":repo.root,"commit":commit,"tracked_files":entries.len(),"backend":backend::name(),"scope_enforcement":"validation only; writes are not blocked"}),
    )
}

pub fn add(
    repo: &Repo,
    destination: &Path,
    branch: &str,
    write: &[String],
    dry_run: bool,
) -> Result<Value, String> {
    let destination = absolute_target(destination)?;
    if exists(&destination) {
        return Err(format!(
            "destination already exists: {destination:?}; choose a new path"
        ));
    }
    if destination.starts_with(&repo.root) || destination.starts_with(&repo.common) {
        return Err(
            "destination must be outside the source checkout and Git metadata directory".into(),
        );
    }
    if branch.starts_with('-') || branch == "HEAD" {
        return Err("invalid new branch name".into());
    }
    let branch_ref = format!("refs/heads/{branch}");
    git::run(&repo.root, ["check-ref-format", &branch_ref])?;
    if git::run(&repo.root, ["show-ref", "--verify", &branch_ref]).is_ok() {
        return Err(format!(
            "branch already exists: {branch}; choose a new branch"
        ));
    }
    let (commit, entries) = git::validate_source(repo)?;
    let scope = scope::normalize_entries(&repo.root, write)?;
    let parent = destination.parent().ok_or("destination has no parent")?;
    backend::probe(&repo.root, parent)?;
    if dry_run {
        return Ok(
            json!({"dry_run":true,"path":destination,"branch":branch,"creation_commit":commit,"tracked_files":entries.len(),"scope":scope,"backend":backend::name()}),
        );
    }
    let store = Store::open(repo)?;
    // Recheck after acquiring the repository lock: another completed creation
    // may have consumed the requested branch while preflight was reading files.
    if exists(&destination) {
        return Err("destination appeared during preflight; choose a new path".into());
    }
    if git::run(&repo.root, ["show-ref", "--verify", &branch_ref]).is_ok() {
        return Err(format!(
            "branch already exists: {branch}; choose a new branch"
        ));
    }
    if store.records()?.iter().any(|m| m.path == destination) {
        return Err(
            "destination has existing git-thin metadata; inspect status or run recover".into(),
        );
    }
    let id = uuid::Uuid::new_v4().to_string();
    let staging = parent.join(format!(".git-thin-{id}"));
    // An exclusive mkdir is the ownership boundary. A crash before journaling may
    // leave an empty directory; it is never guessed at or deleted by recovery.
    fs::create_dir(&staging).map_err(|e| format!("cannot reserve staging directory: {e}"))?;
    let owned = fs::symlink_metadata(&staging).map_err(|e| e.to_string())?;
    let mut meta = Metadata {
        schema_version: SCHEMA,
        id,
        creation_commit: commit.clone(),
        branch: branch.into(),
        backend: backend::name().into(),
        scope,
        state: "creating".into(),
        path: destination.clone(),
        staging_path: staging.clone(),
        device: owned.dev(),
        inode: owned.ino(),
        clone_ms: 0,
        verification_ms: 0,
    };
    if let Err(e) = store.save(&meta) {
        let _ = fs::remove_dir(&staging);
        return Err(e);
    }
    let result = (|| {
        git::run(
            &repo.root,
            [
                std::ffi::OsStr::new("worktree"),
                std::ffi::OsStr::new("add"),
                std::ffi::OsStr::new("--no-checkout"),
                std::ffi::OsStr::new("-b"),
                std::ffi::OsStr::new(branch),
                staging.as_os_str(),
                std::ffi::OsStr::new(&commit),
            ],
        )?;
        assert_registered(repo, &meta, &staging)?;
        fault("after-registration")?;
        fault("before-clone")?;
        fault("no-space")?;
        let start = Instant::now();
        for entry in &entries {
            match entry.mode.as_str() {
                "120000" => backend::reproduce_symlink(&repo.root, &staging, &entry.path)?,
                _ => backend::clone_regular(
                    &repo.root,
                    &staging,
                    &entry.path,
                    entry.mode == "100755",
                )?,
            }
        }
        meta.clone_ms = start.elapsed().as_millis();
        git::run(&staging, ["read-tree", &commit])?;
        fault("before-verification")?;
        let start = Instant::now();
        git::verify(&staging, &entries)?;
        if !git::run(
            &staging,
            ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?
        .is_empty()
        {
            return Err("destination does not match the creation commit".into());
        }
        meta.verification_ms = start.elapsed().as_millis();
        meta.state = "publishing".into();
        store.save(&meta)?;
        if exists(&destination) {
            return Err("destination appeared during creation; leaving it untouched".into());
        }
        backend::rename_directory_noreplace(&staging, &destination)?;
        git::run(
            &repo.root,
            [
                std::ffi::OsStr::new("worktree"),
                std::ffi::OsStr::new("repair"),
                destination.as_os_str(),
            ],
        )?;
        assert_registered(repo, &meta, &destination)?;
        meta.state = "ready".into();
        store.save(&meta)?;
        Ok(json!({"worktree":meta,"clone_ms":meta.clone_ms,"verification_ms":meta.verification_ms}))
    })();
    if let Err(error) = result {
        // Never roll back a published directory: it may already be in use.
        if owns(&meta, &destination) {
            return Err(format!(
                "{error}; destination was published; run git thin recover to finalize metadata"
            ));
        }
        return match rollback(repo, &store, &meta, false) {
            Ok(()) => Err(format!(
                "{error}; owned staging files cleaned up; any created branch is preserved"
            )),
            Err(cleanup) => Err(format!(
                "{error}; cleanup needs git thin recover: {cleanup}"
            )),
        };
    }
    result
}

fn validate_staging(repo: &Repo, meta: &Metadata, verify_bytes: bool) -> Result<(), String> {
    let entries = git::tree(&repo.root, &meta.creation_commit)?;
    let expected: std::collections::HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
    let mut directories = std::collections::HashSet::new();
    for entry in &entries {
        for ancestor in entry.path.ancestors().skip(1) {
            directories.insert(ancestor.to_path_buf());
        }
    }
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for child in fs::read_dir(meta.staging_path.join(&relative)).map_err(|e| e.to_string())? {
            let child = child.map_err(|e| e.to_string())?;
            let path = relative.join(child.file_name());
            if path == Path::new(".git") {
                continue;
            }
            let kind = child.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() && directories.contains(&path) {
                pending.push(path);
            } else if !kind.is_dir() && expected.contains(&path) {
                // The tracked manifest identifies files this creation can own.
            } else {
                return Err(format!("unexpected file in interrupted staging directory: {path:?}; move it to safety before recovery"));
            }
        }
    }
    if verify_bytes {
        // Before read-tree, the index may be absent/empty. Any entries that do
        // exist must still be baseline entries, so staged user work is retained.
        let index = git::run(&meta.staging_path, ["ls-files", "--stage", "-z"])?;
        for record in index.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let tab = record
                .iter()
                .position(|b| *b == b'\t')
                .ok_or("invalid staging index")?;
            let path = std::str::from_utf8(&record[tab + 1..])
                .map_err(|_| "non-UTF-8 staging index path")?;
            let header =
                std::str::from_utf8(&record[..tab]).map_err(|_| "invalid staging index header")?;
            if !entries
                .iter()
                .any(|e| e.path == Path::new(path) && header == format!("{} {} 0", e.mode, e.oid))
            {
                return Err(format!("staged work differs from the creation commit at {path:?}; preserve it before recovery"));
            }
        }
        let present: Vec<_> = entries
            .into_iter()
            .filter(|e| exists(&meta.staging_path.join(&e.path)))
            .collect();
        git::verify(&meta.staging_path, &present).map_err(|e| {
            format!("staging contents changed; preserve local work before manual recovery: {e}")
        })?;
    }
    Ok(())
}

fn registered_branch_path(repo: &Repo, branch: &str) -> Result<Option<PathBuf>, String> {
    let output = git::run(&repo.root, ["worktree", "list", "--porcelain", "-z"])?;
    let branch = format!("branch refs/heads/{branch}");
    let mut path = None;
    for field in output.split(|b| *b == 0) {
        if let Some(value) = field.strip_prefix(b"worktree ") {
            path = Some(PathBuf::from(
                std::str::from_utf8(value).map_err(|_| "non-UTF-8 registered worktree path")?,
            ));
        }
        if field == branch.as_bytes() {
            return Ok(path);
        }
    }
    Ok(None)
}

fn validate_rollback(repo: &Repo, meta: &Metadata, verify_bytes: bool) -> Result<(), String> {
    let path = &meta.staging_path;
    if exists(path) {
        if !owns(meta, path) {
            return Err(format!(
                "staging identity changed; refusing cleanup: {path:?}"
            ));
        }
        if exists(&path.join(".git")) {
            assert_registered(repo, meta, path)?;
            if git::text(path, ["rev-parse", "HEAD"])? != meta.creation_commit {
                return Err("staging HEAD changed; preserve its work and recover manually".into());
            }
            validate_staging(repo, meta, verify_bytes)?;
        } else if fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
        {
            return Err(
                "unregistered staging directory is not empty; manual recovery required".into(),
            );
        }
    }
    if let Some(registered) = registered_branch_path(repo, &meta.branch)? {
        if registered != *path || !exists(path) {
            return Err(format!("operation registration moved or is orphaned at {registered:?}; repair it before recovery"));
        }
    }
    Ok(())
}

fn rollback(repo: &Repo, store: &Store, meta: &Metadata, verify_bytes: bool) -> Result<(), String> {
    validate_rollback(repo, meta, verify_bytes)?;
    let path = &meta.staging_path;
    if exists(path) {
        if !owns(meta, path) {
            return Err(format!(
                "staging directory identity changed; refusing cleanup: {path:?}"
            ));
        }
        if exists(&path.join(".git")) {
            assert_registered(repo, meta, path)?;
            let head = git::text(path, ["rev-parse", "HEAD"])?;
            if head != meta.creation_commit {
                return Err("staging HEAD changed; preserve its work and recover manually".into());
            }
            git::run(
                &repo.root,
                [
                    std::ffi::OsStr::new("worktree"),
                    std::ffi::OsStr::new("remove"),
                    std::ffi::OsStr::new("--force"),
                    path.as_os_str(),
                ],
            )?;
        } else {
            // Before registration the only owned content is the empty reservation.
            fs::remove_dir(path).map_err(|e| {
                format!("unregistered staging directory is not empty or cannot be removed: {e}")
            })?;
        }
    }
    store.delete(meta)
}

pub fn list(repo: &Repo) -> Result<Value, String> {
    Ok(json!({"worktrees":Store::open(repo)?.records()?}))
}
pub fn status(repo: &Repo, path: &Path) -> Result<Value, String> {
    let store = Store::open(repo)?;
    let meta = store.find(path)?;
    require_ready(repo, &meta)?;
    let report = scope::check(&meta.path, &meta.creation_commit, &meta.scope)?;
    Ok(json!({"worktree":meta,"scope_check":report}))
}
pub fn check(repo: &Repo, path: &Path) -> Result<scope::CheckReport, String> {
    let store = Store::open(repo)?;
    let meta = store.find(path)?;
    require_ready(repo, &meta)?;
    scope::check(&meta.path, &meta.creation_commit, &meta.scope)
}
pub fn expand_scope(repo: &Repo, path: &Path, paths: &[String]) -> Result<Value, String> {
    let store = Store::open(repo)?;
    let mut meta = store.find(path)?;
    require_ready(repo, &meta)?;
    for entry in scope::normalize_entries(&meta.path, paths)? {
        if !meta
            .scope
            .iter()
            .any(|e| e.path == entry.path && e.directory == entry.directory)
        {
            meta.scope.push(entry);
        }
    }
    store.save(&meta)?;
    Ok(json!({"worktree":meta}))
}
fn ensure_removable(path: &Path) -> Result<(), String> {
    let head = git::text(path, ["rev-parse", "HEAD"])?;
    let entries = git::tree(path, &head)?;
    scope::validate_checkout_configuration(path, &entries)?;
    if !git::run(
        path,
        [
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?
    .is_empty()
    {
        return Err("worktree has uncommitted, untracked, or ignored files; commit or move them before removal".into());
    }
    // Git normally trusts stat data; independently verify actual tracked contents.
    if git::run(
        path,
        [
            "for-each-ref",
            "--format=%(refname)",
            "--contains",
            &head,
            "refs/heads/",
        ],
    )?
    .is_empty()
    {
        return Err("HEAD is not reachable from any local branch; create a branch at HEAD before removing this worktree so its commits remain reachable".into());
    }
    git::verify(path, &entries)
}
pub fn remove(repo: &Repo, path: &Path, dry_run: bool) -> Result<Value, String> {
    let store = Store::open(repo)?;
    let mut meta = store.find(path)?;
    require_ready(repo, &meta)?;
    ensure_removable(&meta.path)?;
    if dry_run {
        return Ok(json!({"dry_run":true,"path":meta.path,"branch_preserved":meta.branch}));
    }
    meta.state = "removing".into();
    store.save(&meta)?;
    if let Err(e) = git::run(
        &repo.root,
        [
            std::ffi::OsStr::new("worktree"),
            std::ffi::OsStr::new("remove"),
            meta.path.as_os_str(),
        ],
    ) {
        meta.state = "ready".into();
        let _ = store.save(&meta);
        return Err(e);
    }
    store.delete(&meta)?;
    Ok(json!({"removed":meta.path,"branch_preserved":meta.branch}))
}
pub fn recover(repo: &Repo, dry_run: bool) -> Result<Value, String> {
    let store = Store::open(repo)?;
    let mut actions = Vec::new();
    for mut meta in store.records()? {
        if meta.state == "ready" {
            continue;
        }
        let action = match meta.state.as_str() {
            "creating" | "publishing" => {
                if owns(&meta, &meta.path) {
                    assert_registered(repo, &meta, &meta.path)?;
                    if git::text(&meta.path, ["rev-parse", "HEAD"])? != meta.creation_commit {
                        return Err(
                            "published worktree HEAD changed; manual recovery required".into()
                        );
                    }
                    git::verify(&meta.path, &git::tree(&meta.path, &meta.creation_commit)?)?;
                    if !dry_run {
                        git::run(
                            &repo.root,
                            [
                                std::ffi::OsStr::new("worktree"),
                                std::ffi::OsStr::new("repair"),
                                meta.path.as_os_str(),
                            ],
                        )?;
                        meta.state = "ready".into();
                        store.save(&meta)?;
                    }
                    "finalize"
                } else {
                    validate_rollback(repo, &meta, true)?;
                    if !dry_run {
                        rollback(repo, &store, &meta, true)?;
                    }
                    "rollback"
                }
            }
            "removing" => {
                if exists(&meta.path) {
                    assert_registered(repo, &meta, &meta.path)?;
                    ensure_removable(&meta.path)?;
                    if !dry_run {
                        git::run(
                            &repo.root,
                            [
                                std::ffi::OsStr::new("worktree"),
                                std::ffi::OsStr::new("remove"),
                                meta.path.as_os_str(),
                            ],
                        )?;
                    }
                } else {
                    let registered =
                        git::run(&repo.root, ["worktree", "list", "--porcelain", "-z"])?;
                    let expected = format!(
                        "worktree {}",
                        meta.path.to_str().ok_or("non-UTF-8 worktree path")?
                    );
                    if registered
                        .split(|b| *b == 0)
                        .any(|field| field == expected.as_bytes())
                    {
                        return Err(format!("removed directory still has a Git registration at {:?}; inspect and remove that stale registration with Git before recovery", meta.path));
                    }
                }
                if !dry_run {
                    store.delete(&meta)?;
                }
                "finish_removal"
            }
            state => return Err(format!("unknown operation state: {state}")),
        };
        actions.push(json!({"id":meta.id,"path":meta.path,"action":action}));
    }
    Ok(json!({"dry_run":dry_run,"recovered":actions}))
}

#[cfg(not(feature = "fault-injection"))]
fn fault(_: &str) -> Result<(), String> {
    Ok(())
}
#[cfg(feature = "fault-injection")]
fn fault(point: &str) -> Result<(), String> {
    let configured = std::env::var("GIT_THIN_TEST_FAILPOINT").unwrap_or_default();
    if configured == format!("crash-{point}") {
        std::process::exit(86);
    }
    if configured == format!("pause-{point}") {
        let ready = std::env::var("GIT_THIN_TEST_READY_FILE").map_err(|e| e.to_string())?;
        let proceed = std::env::var("GIT_THIN_TEST_CONTINUE_FILE").map_err(|e| e.to_string())?;
        fs::write(ready, b"ready").map_err(|e| e.to_string())?;
        let start = Instant::now();
        while !Path::new(&proceed).exists() {
            if start.elapsed().as_secs() > 30 {
                return Err("test pause timed out".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    if configured == point {
        return Err(if point == "no-space" {
            "injected ENOSPC: no space left on device".into()
        } else {
            format!("injected failure at {point}")
        });
    }
    Ok(())
}
