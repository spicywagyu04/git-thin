# Contributing

Use Rust 1.89 or later and Git 2.39 or later. The main native target is macOS on APFS. Linux development requires a filesystem that supports reflinks to exercise successful cloning; the unsupported-filesystem path is also important.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features -- --show-output
cargo test --locked --all-targets -- --show-output
cargo build --locked --release
python3 scripts/production_smoke.py target/release/git-thin --require-native
```

Run integration tests on real Git repositories. Do not replace filesystem behavior with a mock and present the result as APFS validation. When a host cannot clone, distinguish unavailable capability from a passed cloning test. macOS CI exercises the native backend; a local Linux run alone cannot establish macOS correctness.

Set `GIT_THIN_REQUIRE_CLONE=1` to fail if native cloning is unavailable; macOS CI requires this. The `fault-injection` feature enables deterministic interruption, rollback, source-change, and simulated disk-exhaustion tests. Keep it out of release builds. To exercise actual cross-volume rejection, set `GIT_THIN_CROSS_VOLUME_ROOT` to a writable directory on another filesystem from `TMPDIR`. Tests explicitly report unavailable filesystem scenarios; a simulated ENOSPC does not substitute for a measured full-volume run.

CI separates ordinary Linux checks, the minimum Rust 1.89.0/Git 2.39.0 combination,
native Btrfs, and both default and case-sensitive APFS. Native lanes set
`GIT_THIN_REQUIRE_CLONE=1` and `GIT_THIN_REQUIRE_VOLUMES=1`, validate their
filesystems before running, and retain full test output and capability reports.
An unavailable required volume fails the lane. Ordinary developer runs can omit
the volume variables; their explicit unavailable messages are not native passes.
The production smoke runs a build without `fault-injection` through creation,
checking, and removal with test controls set, including pause and crash requests.
On a host without native cloning, omit `--require-native` only for an explicitly
incomplete portable smoke result.

The Btrfs CI job requires a Linux kernel with Btrfs and runner permission to
create loop mounts. It formats and mounts only newly created images under the
runner temporary directory. Tests run as the unprivileged runner user; privilege
is needed only to prepare and unmount those filesystems. A hosted runner that
denies mounts cannot satisfy this lane and must not fall back to ordinary copies.

Actual exhaustion tests accept only the root of a dedicated mounted filesystem
with capacity at most 256 MiB; CI uses 128 MiB. Never set
`GIT_THIN_EXHAUSTION_ROOT` to shared data. The tests serialize their fills, bound
ballast to 256 MiB and 65,536 empty inodes, and release all owned ballast even on
failure. One test fills before creation; the feature-enabled test pauses after
the first cloned file, fills the filesystem, resumes, and checks failure plus
repeatable recovery. Ready/continue files live outside the exhausted filesystem.
These are real filesystem ENOSPC cases, distinct from injected EACCES/EIO/ENOSPC
and abrupt-process-exit tests.

For prepared filesystems, record capabilities before the native suite:

```sh
GIT_THIN_REQUIRE_CLONE=1 python3 scripts/validate_environment.py \
  --filesystem apfs --require-volumes
```

Set `TMPDIR`, `GIT_THIN_CROSS_VOLUME_ROOT`, and `GIT_THIN_EXHAUSTION_ROOT`
before that command. All three must identify different filesystems. Use
`--filesystem btrfs` for the Linux native lane. The validator does not provision
or fill disks; the CI workflow provides a concrete disposable-image recipe.

For changes to creation or cleanup, cover both success and partial failure. Useful cases include independent edits and staging, source edits during cloning, interrupted creation, disk exhaustion, unsupported filesystems, cross-volume destinations, and refusing to discard work. Recovery tests must prove that existing paths and branches survive.

Scope changes should cover committed violations as well as staged and unstaged edits, new files, deletions, both rename sides, ignored tracked paths, and incompatible histories. Path handling needs spaces, Unicode, newlines, executable files, internal symlinks, and rejected escaping links.

Never add a silent full-copy fallback, a source-file hard-link shortcut, automatic Git hooks, or a global Git configuration change. Do not claim strict write protection. Preserve the difference between final-state scope validation and filesystem enforcement.

Keep commands machine-readable: use NUL-delimited Git output for path lists and structured JSON for integrations. Avoid lossy path conversion when it could change what is validated or removed. Errors should explain the unsupported condition and how to proceed.

Use [the benchmark procedure](docs/benchmarks.md) for performance work. Record hardware, OS, filesystem, Git and Rust versions, tool commit, fixture size, and measurement method. Publish results only after collecting them. Logical sizes and `du` totals are not exact APFS shared-extent measurements.

Before publishing, follow [release preparation](docs/releases.md). Source contributions do not authorize releasing binaries or registering a name.
