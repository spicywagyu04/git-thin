//! Opt-in, real ENOSPC test for a small disposable filesystem.
//!
//! GIT_THIN_EXHAUSTION_ROOT must name a dedicated disposable volume, not a shared
//! disk. The test owns and deletes only its temporary directory. It refuses to
//! write more than 256 MiB of ballast; CI provides a separate 128 MiB APFS image.
#![cfg(unix)]

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

static EXHAUSTION_LOCK: Mutex<()> = Mutex::new(());

fn disposable_root() -> Option<PathBuf> {
    let Some(root) = std::env::var_os("GIT_THIN_EXHAUSTION_ROOT") else {
        assert_ne!(
            std::env::var("GIT_THIN_REQUIRE_VOLUMES").as_deref(),
            Ok("1"),
            "required dedicated exhaustion volume is missing"
        );
        eprintln!("UNAVAILABLE real ENOSPC: set GIT_THIN_EXHAUSTION_ROOT to a dedicated disposable 128 MiB mounted volume");
        return None;
    };
    let root = PathBuf::from(root).canonicalize().unwrap();
    let parent = root.parent().expect("never fill a root filesystem");
    assert_ne!(
        fs::metadata(&root).unwrap().dev(),
        fs::metadata(parent).unwrap().dev(),
        "exhaustion root must be a dedicated mounted filesystem, not a shared directory"
    );
    let capacity = fs2::total_space(&root).unwrap();
    assert!(
        capacity > 0 && capacity <= 256 * 1024 * 1024,
        "refusing to fill a filesystem larger than 256 MiB: {capacity}"
    );
    eprintln!(
        "EXECUTING real ENOSPC on {root:?}; device={}; capacity={capacity}",
        fs::metadata(&root).unwrap().dev()
    );
    Some(root)
}

fn run(program: &str, directory: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .current_dir(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap()
}

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}; stdout: {}; stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = run("git", directory, args);
    assert!(output.status.success(), "{}", describe(&output));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn is_enospc(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOSPC)
}

fn fill_until_enospc(ballast: &mut File) -> Result<(), String> {
    const LIMIT: usize = 256 * 1024 * 1024;
    // Nonzero, pseudorandom data avoids turning the ballast into a sparse file
    // or relying on a filesystem's handling of highly compressible zeroes.
    // One sync per MiB keeps a 128 MiB image fill comfortably inside the
    // handshake deadline, including slower virtualized CI storage.
    let mut chunk = vec![0_u8; 1024 * 1024];
    let mut state = 0x9e37_79b9_u32;
    for byte in &mut chunk {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state as u8;
    }
    for _ in 0..LIMIT / chunk.len() {
        match ballast.write_all(&chunk).and_then(|()| ballast.sync_all()) {
            Ok(()) => {}
            Err(error) if is_enospc(&error) => {
                // A failed large write can leave a few allocation blocks free.
                // Consume those too, so the tiny clone probe cannot fit there.
                while ballast.metadata().map_err(|e| e.to_string())?.len() + 4096 <= LIMIT as u64 {
                    match ballast
                        .write_all(&chunk[..4096])
                        .and_then(|()| ballast.sync_all())
                    {
                        Ok(()) => {}
                        Err(error) if is_enospc(&error) => return Ok(()),
                        Err(error) => {
                            return Err(format!(
                                "final ballast write failed without ENOSPC: {error}"
                            ))
                        }
                    }
                }
                return Err("ENOSPC was transient and the 256 MiB safety cap was reached; use a dedicated smaller disposable volume".into());
            }
            Err(error) => return Err(format!("ballast write failed without ENOSPC: {error}")),
        }
    }
    Err("no ENOSPC within the 256 MiB safety cap; GIT_THIN_EXHAUSTION_ROOT must point to a dedicated disposable volume smaller than 256 MiB (CI uses 128 MiB APFS)".into())
}

#[test]
fn real_disk_exhaustion_fails_without_publishing_or_changing_existing_branches() {
    let _serial = EXHAUSTION_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(root) = disposable_root() else {
        return;
    };
    let owned = tempfile::tempdir_in(root).unwrap();
    let parent = owned.path().canonicalize().unwrap();
    let source = parent.join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-b", "main"]);
    git(&source, &["config", "user.name", "Git Thin Tests"]);
    git(&source, &["config", "user.email", "tests@example.invalid"]);
    git(&source, &["config", "core.autocrlf", "false"]);
    fs::write(source.join("tracked.txt"), "original content\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "baseline before filling volume"]);
    git(&source, &["branch", "preexisting"]);
    let baseline = git(&source, &["rev-parse", "HEAD"]);
    git_thin::backend::probe(&source, &parent)
        .expect("the disposable volume must support native cloning before it is filled");

    let ballast_directory = tempfile::tempdir_in(&parent).unwrap();
    let ballast_path = ballast_directory.path().join("owned-ballast");
    let mut ballast = File::create_new(&ballast_path).unwrap();
    let full = fill_until_enospc(&mut ballast)
        .and_then(|()| fill_metadata_until_enospc(ballast_directory.path()));
    let destination = parent.join("agent");
    let output = full.as_ref().ok().map(|_| {
        run(
            env!("CARGO_BIN_EXE_git-thin"),
            &source,
            &[
                "--json",
                "add",
                destination.to_str().unwrap(),
                "--branch",
                "agent/exhaustion",
                "--write",
                "tracked.txt",
            ],
        )
    });

    // Release all ballast before assertions, diagnostics, or fixture cleanup.
    // TempDir also removes owned files during unwinding if an earlier step fails.
    drop(ballast);
    ballast_directory.close().unwrap();
    full.unwrap_or_else(|error| panic!("{error}"));
    let output = output.unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    let diagnostic = describe(&output).to_ascii_lowercase();
    assert!(
        diagnostic.contains("no space") || diagnostic.contains("enospc"),
        "expected an actionable disk exhaustion error: {diagnostic}"
    );
    assert!(
        !destination.exists(),
        "failed operation published a worktree"
    );
    assert_eq!(git(&source, &["rev-parse", "main"]), baseline);
    assert_eq!(git(&source, &["rev-parse", "preexisting"]), baseline);
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "original content\n"
    );
    assert!(git(&source, &["status", "--porcelain"]).is_empty());
}

#[cfg(feature = "fault-injection")]
struct ChildGuard(Option<std::process::Child>);

#[cfg(feature = "fault-injection")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(feature = "fault-injection")]
fn wait_for_file(path: &Path, child: &mut ChildGuard) {
    let start = std::time::Instant::now();
    while !path.exists() {
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "child exited before the partial-clone handshake"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "partial-clone handshake timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(feature = "fault-injection")]
fn wait_for_child(child: &mut ChildGuard) -> Output {
    let start = std::time::Instant::now();
    while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "creation did not finish after exhausting the disposable volume"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.0.take().unwrap().wait_with_output().unwrap()
}

fn fill_metadata_until_enospc(directory: &Path) -> Result<usize, String> {
    // Btrfs may reserve metadata space separately from file data. Exhaust actual
    // inode creation too, within a bounded directory owned by this test. Empty
    // files do not increase the existing 256 MiB written-data cap.
    for number in 0..65_536 {
        match File::create_new(directory.join(format!("inode-{number}"))) {
            Ok(file) => drop(file),
            Err(error) if is_enospc(&error) => return Ok(number),
            Err(error) => return Err(format!("metadata fill failed without ENOSPC: {error}")),
        }
    }
    Err("metadata ENOSPC not reached within the 65,536-inode safety cap".into())
}

#[test]
#[cfg(feature = "fault-injection")]
fn real_disk_exhaustion_after_partial_clone_preserves_source_and_recovers() {
    let _serial = EXHAUSTION_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(root) = disposable_root() else {
        return;
    };
    let owned = tempfile::tempdir_in(&root).unwrap();
    let parent = owned.path().canonicalize().unwrap();
    let source = parent.join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-b", "main"]);
    git(&source, &["config", "user.name", "Git Thin Tests"]);
    git(&source, &["config", "user.email", "tests@example.invalid"]);
    git(&source, &["config", "core.autocrlf", "false"]);
    fs::write(source.join("a-first"), "first original\n").unwrap();
    fs::write(source.join("b-second"), "second original\n").unwrap();
    git(&source, &["add", "."]);
    git(
        &source,
        &["commit", "-m", "baseline before partial creation"],
    );
    git(&source, &["branch", "preexisting"]);
    let baseline = git(&source, &["rev-parse", "HEAD"]);
    let index_before = fs::read(source.join(".git/index")).unwrap();
    let modes_before = ["a-first", "b-second"].map(|name| {
        let metadata = fs::metadata(source.join(name)).unwrap();
        (metadata.ino(), metadata.mode())
    });
    git_thin::backend::probe(&source, &parent).unwrap();

    // The handshake is outside the volume we fill, so the test can resume and
    // reap its child even when all filesystem data and metadata space is gone.
    let control = tempfile::tempdir().unwrap();
    assert_ne!(
        fs::metadata(control.path()).unwrap().dev(),
        fs::metadata(&root).unwrap().dev()
    );
    let ready = control.path().join("ready");
    let resume = control.path().join("continue");
    let destination = parent.join("agent");
    let ballast = tempfile::tempdir_in(&parent).unwrap();
    let mut ballast_file = File::create_new(ballast.path().join("data")).unwrap();
    let mut child = ChildGuard(Some(
        Command::new(env!("CARGO_BIN_EXE_git-thin"))
            .current_dir(&source)
            .args([
                "--json",
                "add",
                destination.to_str().unwrap(),
                "--branch",
                "agent/partial-exhaustion",
                "--write",
                ".",
            ])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_THIN_TEST_FAILPOINT", "pause-after-partial-clone")
            .env("GIT_THIN_TEST_READY_FILE", &ready)
            .env("GIT_THIN_TEST_CONTINUE_FILE", &resume)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    wait_for_file(&ready, &mut child);
    let journal = fs::read_dir(source.join(".git/git-thin"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .expect("a durable journal must exist before the partial-clone pause");
    let record: serde_json::Value = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    let staging = PathBuf::from(record["staging_path"].as_str().unwrap());
    assert_eq!(
        fs::read(staging.join("a-first")).unwrap(),
        b"first original\n"
    );
    assert!(!staging.join("b-second").exists());
    assert!(git(&source, &["worktree", "list", "--porcelain"]).contains(staging.to_str().unwrap()));
    assert_eq!(
        git(&source, &["rev-parse", "agent/partial-exhaustion"]),
        baseline
    );

    let full = fill_until_enospc(&mut ballast_file)
        .and_then(|()| fill_metadata_until_enospc(ballast.path()));
    let output = if full.is_ok() {
        fs::write(&resume, "continue").unwrap();
        Some(wait_for_child(&mut child))
    } else {
        None
    };
    // Release ballast before every diagnostic assertion. Guards also release
    // the files and kill/reap the paused child if any earlier assertion unwinds.
    drop(ballast_file);
    ballast.close().unwrap();
    let metadata_files = full.unwrap_or_else(|error| panic!("{error}"));
    eprintln!(
        "EXECUTED real ENOSPC after one cloned file; additional ballast inodes={metadata_files}"
    );
    let output = output.unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", describe(&output));
    let diagnostic = describe(&output).to_ascii_lowercase();
    assert!(
        diagnostic.contains("no space") || diagnostic.contains("enospc"),
        "{diagnostic}"
    );
    assert!(
        !destination.exists(),
        "failed creation published a worktree"
    );
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index_before);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), baseline);
    assert_eq!(git(&source, &["rev-parse", "preexisting"]), baseline);
    assert_eq!(
        git(&source, &["rev-parse", "agent/partial-exhaustion"]),
        baseline
    );
    for (number, (name, expected)) in [
        ("a-first", b"first original\n".as_slice()),
        ("b-second", b"second original\n".as_slice()),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(fs::read(source.join(name)).unwrap(), *expected);
        let metadata = fs::metadata(source.join(name)).unwrap();
        assert_eq!((metadata.ino(), metadata.mode()), modes_before[number]);
    }
    for args in [
        vec!["--json", "recover", "--dry-run"],
        vec!["--json", "recover"],
        vec!["--json", "recover"],
    ] {
        let recovery = run(env!("CARGO_BIN_EXE_git-thin"), &source, &args);
        assert!(recovery.status.success(), "{}", describe(&recovery));
    }
    assert!(!staging.exists());
    assert!(!journal.exists());
    assert!(!git(&source, &["worktree", "list", "--porcelain"]).contains(staging.to_str().unwrap()));
    assert_eq!(
        git(&source, &["rev-parse", "agent/partial-exhaustion"]),
        baseline
    );
}
