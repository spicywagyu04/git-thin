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

/// Hash Git's blob header and raw contents in bounded memory. The expected
/// object's length selects SHA-1 or SHA-256, so no process is needed per file.
/// SHA-1 retains collision detection rather than silently weakening Git's check.
pub(crate) fn hash_reader(
    expected_oid: &str,
    length: u64,
    reader: &mut dyn io::Read,
) -> Result<String, String> {
    use sha2::Digest;
    if !expected_oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid Git blob object ID".into());
    }
    match expected_oid.len() {
        40 => {
            let mut hash = sha1collisiondetection::Sha1CD::default();
            hash_contents(reader, length, |bytes| hash.update(bytes))?;
            let digest = hash
                .finalize_cd()
                .map_err(|_| "SHA-1 collision detected while verifying blob")?;
            Ok(format!("{digest:x}"))
        }
        64 => {
            let mut hash = sha2::Sha256::new();
            hash_contents(reader, length, |bytes| hash.update(bytes))?;
            Ok(format!("{:x}", hash.finalize()))
        }
        _ => Err("unsupported Git blob object ID length".into()),
    }
}

fn hash_contents(
    reader: &mut dyn io::Read,
    length: u64,
    mut update: impl FnMut(&[u8]),
) -> Result<(), String> {
    update(format!("blob {length}\0").as_bytes());
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("content verification read failed: {error}")),
        };
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or("file length overflow during verification")?;
        if total > length {
            return Err("file length changed during content verification".into());
        }
        update(&buffer[..count]);
    }
    if total != length {
        return Err("file length changed during content verification".into());
    }
    Ok(())
}

/// Read actual bytes, independently of Git's index stat cache and assume-unchanged flags.
pub fn verify(root: &Path, entries: &[Entry]) -> Result<(), String> {
    for entry in entries {
        let actual = if entry.mode == "120000" {
            let target = crate::backend::read_symlink(root, &entry.path)?;
            use std::os::unix::ffi::OsStrExt;
            hash_reader(
                &entry.oid,
                target.as_os_str().as_bytes().len() as u64,
                &mut target.as_os_str().as_bytes(),
            )?
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
            hash_reader(&entry.oid, meta.len(), &mut file)?
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
    // Status also inspects new index entries, including intent-to-add paths.
    // Validate their attributes before it can run a clean filter. Reuse the
    // NUL-delimited flags listing so clean preflight needs no extra Git call.
    let mut attribute_paths = std::collections::BTreeSet::new();
    for entry in &entries {
        attribute_paths.insert(entry.path.as_os_str().as_encoded_bytes());
    }
    for record in flags
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        if record.len() <= 2 || record[1] != b' ' {
            return Err("invalid ls-files output while validating checkout attributes".into());
        }
        attribute_paths.insert(&record[2..]);
    }
    let mut input = Vec::new();
    for path in attribute_paths {
        input.extend_from_slice(path);
        input.push(0);
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

#[cfg(test)]
mod hashing_tests {
    use super::*;
    #[test]
    fn streamed_blob_hashes_match_git_for_both_object_formats() {
        for format in ["sha1", "sha256"] {
            let dir = tempfile::tempdir().unwrap();
            run(dir.path(), ["init", &format!("--object-format={format}")]).unwrap();
            for size in [0, 1, 63, 64, 65, 65535, 65536, 65537, 2 * 1024 * 1024] {
                let bytes: Vec<u8> = (0..size).map(|n| (n * 37) as u8).collect();
                fs::write(dir.path().join("blob"), &bytes).unwrap();
                let expected = text(dir.path(), ["hash-object", "--no-filters", "blob"]).unwrap();
                assert_eq!(
                    hash_reader(&expected, size as u64, &mut bytes.as_slice()).unwrap(),
                    expected
                );
            }
        }
    }
    #[test]
    fn changing_lengths_and_read_errors_fail_without_a_digest() {
        let oid = "0".repeat(40);
        assert!(hash_reader(&oid, 4, &mut b"abc".as_slice())
            .unwrap_err()
            .contains("length changed"));
        assert!(hash_reader(&oid, 2, &mut b"abc".as_slice())
            .unwrap_err()
            .contains("length changed"));
        struct Broken;
        impl io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
        }
        assert!(hash_reader(&oid, 3, &mut Broken)
            .unwrap_err()
            .contains("read failed"));
        assert!(hash_reader("invalid", 0, &mut b"".as_slice()).is_err());
    }
}
