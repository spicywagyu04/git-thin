use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

pub fn command(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .arg("-C")
        .arg(root)
        .env("GIT_OPTIONAL_LOCKS", "0");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(key);
    }
    cmd
}

pub fn run<I, S>(root: &Path, args: I) -> Result<Vec<u8>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = command(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

pub fn text<I, S>(root: &Path, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let bytes = run(root, args)?;
    let s = String::from_utf8(bytes).map_err(|_| "non-UTF-8 Git output is not supported")?;
    Ok(s.strip_suffix('\n').unwrap_or(&s).to_owned())
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub root: PathBuf,
    pub common: PathBuf,
}
impl Repo {
    pub fn discover(path: &Path) -> Result<Self, String> {
        let root = PathBuf::from(text(path, ["rev-parse", "--show-toplevel"])?);
        let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
        let common = PathBuf::from(text(
            &root,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?);
        Ok(Self {
            root,
            common: fs::canonicalize(common).map_err(|e| e.to_string())?,
        })
    }
}

#[derive(Debug)]
pub struct Entry {
    pub mode: String,
    pub oid: String,
    pub path: PathBuf,
}
pub fn tree(root: &Path, commit: &str) -> Result<Vec<Entry>, String> {
    let bytes = run(root, ["ls-tree", "-rz", "--full-tree", commit])?;
    let mut result = Vec::new();
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or("invalid ls-tree output")?;
        let header = std::str::from_utf8(&record[..tab]).map_err(|e| e.to_string())?;
        let fields: Vec<_> = header.split(' ').collect();
        if fields.len() != 3 {
            return Err("invalid ls-tree header".into());
        }
        let path = std::str::from_utf8(&record[tab + 1..])
            .map_err(|_| "non-UTF-8 tracked filenames are not supported")?;
        let path = PathBuf::from(path);
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(format!("unsafe tracked path: {path:?}"));
        }
        if fields[0] == "160000" {
            return Err("submodules are not supported; use a source without submodules".into());
        }
        if !matches!(fields[0], "100644" | "100755" | "120000") {
            return Err(format!("unsupported file mode {}", fields[0]));
        }
        result.push(Entry {
            mode: fields[0].into(),
            oid: fields[2].into(),
            path,
        });
    }
    Ok(result)
}

pub(crate) fn hash_reader(root: &Path, reader: &mut dyn io::Read) -> Result<String, String> {
    let mut child = command(root)
        .args(["hash-object", "--no-filters", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let copied = io::copy(
        reader,
        &mut child.stdin.take().ok_or("Git stdin unavailable")?,
    );
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    copied.map_err(|e| format!("content verification read failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git content hash failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8(output.stdout)
        .map_err(|e| e.to_string())?
        .trim()
        .into())
}

/// Read actual bytes, independently of Git's index stat cache and assume-unchanged flags.
pub fn verify(root: &Path, entries: &[Entry]) -> Result<(), String> {
    for entry in entries {
        let actual = if entry.mode == "120000" {
            let target = crate::backend::read_symlink(root, &entry.path)?;
            use std::os::unix::ffi::OsStrExt;
            hash_reader(root, &mut target.as_os_str().as_bytes())?
        } else {
            let mut file = crate::backend::open_regular(root, &entry.path)?;
            let meta = file.metadata().map_err(|e| e.to_string())?;
            let exec = meta.permissions().mode() & 0o100 != 0;
            if exec != (entry.mode == "100755") {
                return Err(format!(
                    "executable mode differs from commit: {:?}",
                    entry.path
                ));
            }
            hash_reader(root, &mut file)?
        };
        if actual != entry.oid {
            return Err(format!("content differs from the creation commit: {:?}; the source may have changed, or checkout transformations may be enabled",entry.path));
        }
    }
    Ok(())
}

pub fn validate_source(repo: &Repo) -> Result<(String, Vec<Entry>), String> {
    let root = &repo.root;
    if text(root, ["config", "--bool", "core.sparseCheckout"]).unwrap_or_default() == "true" {
        return Err(
            "sparse source checkouts are unsupported; disable sparse checkout first".into(),
        );
    }
    for state in [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        let path = text(
            root,
            ["rev-parse", "--path-format=absolute", "--git-path", state],
        )?;
        if Path::new(&path).exists() {
            return Err(format!(
                "repository operation in progress ({state}); finish or abort it first"
            ));
        }
    }
    let commit = text(root, ["rev-parse", "--verify", "HEAD^{commit}"])?;
    let entries = tree(root, &commit)?;
    let flags = run(root, ["ls-files", "-v", "-z"])?;
    if flags
        .split(|b| *b == 0)
        .any(|r| !r.is_empty() && (r[0] == b'S' || r[0].is_ascii_lowercase()))
    {
        return Err("skip-worktree or assume-unchanged index entries are unsupported; restore a complete, verifiable checkout".into());
    }
    if !matches!(
        text(root, ["config", "--get", "core.autocrlf"])
            .unwrap_or_default()
            .as_str(),
        "" | "false"
    ) {
        return Err(
            "core.autocrlf checkout transformations are unsupported; use an untransformed checkout"
                .into(),
        );
    }
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
        .map_err(|e| e.to_string())?;
    let mut input = Vec::new();
    for entry in &entries {
        input.extend_from_slice(entry.path.to_str().ok_or("non-UTF-8 path")?.as_bytes());
        input.push(0);
    }
    // Concurrent stdin writer avoids pipe deadlocks on large trees.
    let mut stdin = child.stdin.take().ok_or("Git stdin unavailable")?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    writer
        .join()
        .map_err(|_| "attribute writer failed")?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("could not inspect checkout attributes".into());
    }
    let fields: Vec<_> = output.stdout.split(|b| *b == 0).collect();
    for attr in fields.as_chunks::<3>().0 {
        if attr[2] != b"unspecified" && attr[2] != b"unset" {
            return Err(format!(
                "unsupported checkout attribute {} on {:?} (including LFS/custom filters)",
                String::from_utf8_lossy(attr[1]),
                String::from_utf8_lossy(attr[0])
            ));
        }
    }
    if !run(
        root,
        ["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    )?
    .is_empty()
    {
        return Err(
            "source has staged or unstaged tracked changes; commit or stash them first".into(),
        );
    }
    verify(root, &entries)?;
    Ok((commit, entries))
}

pub fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("cannot sync directory {path:?}: {e}"))
}
