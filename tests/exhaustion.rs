//! Opt-in, real ENOSPC test for a small disposable filesystem.
//!
//! GIT_THIN_EXHAUSTION_ROOT must name a dedicated disposable volume, not a shared
//! disk. The test owns and deletes only its temporary directory. It refuses to
//! write more than 256 MiB of ballast; CI provides a separate 128 MiB APFS image.
#![cfg(unix)]

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output};

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
    let mut chunk = [0_u8; 64 * 1024];
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
    let Some(root) = std::env::var_os("GIT_THIN_EXHAUSTION_ROOT") else {
        eprintln!("SKIP real ENOSPC test: set GIT_THIN_EXHAUSTION_ROOT to a dedicated disposable 128 MiB volume");
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

    let ballast_path = parent.join("owned-ballast");
    let mut ballast = File::create_new(&ballast_path).unwrap();
    let full = fill_until_enospc(&mut ballast);
    let destination = parent.join("agent");
    let output = full.as_ref().ok().map(|()| {
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
    fs::remove_file(&ballast_path).unwrap();
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
