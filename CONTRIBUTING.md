# Contributing

Use a current stable Rust toolchain and installed Git. The main native target is macOS on APFS. Linux development requires a filesystem that supports reflinks to exercise successful cloning; the unsupported-filesystem path is also important.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Run integration tests on real Git repositories. Do not replace filesystem behavior with a mock and present the result as APFS validation. When a host cannot clone, distinguish unavailable capability from a passed cloning test. macOS CI exercises the native backend; a local Linux run alone cannot establish macOS correctness.

Set `GIT_THIN_REQUIRE_CLONE=1` to fail if native cloning is unavailable; macOS CI requires this. The `fault-injection` feature enables deterministic interruption, rollback, source-change, and simulated disk-exhaustion tests. Keep it out of release builds. To exercise actual cross-volume rejection, set `GIT_THIN_CROSS_VOLUME_ROOT` to a writable directory on another filesystem from `TMPDIR`. Tests explicitly report unavailable filesystem scenarios; a simulated ENOSPC does not substitute for a measured full-volume run.

For changes to creation or cleanup, cover both success and partial failure. Useful cases include independent edits and staging, source edits during cloning, interrupted creation, disk exhaustion, unsupported filesystems, cross-volume destinations, and refusing to discard work. Recovery tests must prove that existing paths and branches survive.

Scope changes should cover committed violations as well as staged and unstaged edits, new files, deletions, both rename sides, ignored tracked paths, and incompatible histories. Path handling needs spaces, Unicode, newlines, executable files, internal symlinks, and rejected escaping links.

Never add a silent full-copy fallback, a source-file hard-link shortcut, automatic Git hooks, or a global Git configuration change. Do not claim strict write protection. Preserve the difference between final-state scope validation and filesystem enforcement.

Keep commands machine-readable: use NUL-delimited Git output for path lists and structured JSON for integrations. Avoid lossy path conversion when it could change what is validated or removed. Errors should explain the unsupported condition and how to proceed.

Use [the benchmark procedure](docs/benchmarks.md) for performance work. Record hardware, OS, filesystem, Git and Rust versions, tool commit, fixture size, and measurement method. Publish results only after collecting them. Logical sizes and `du` totals are not exact APFS shared-extent measurements.

Before publishing, follow [release preparation](docs/releases.md). Source contributions do not authorize releasing binaries or registering a name.
