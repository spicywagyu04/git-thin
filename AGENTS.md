# Working on git-thin

This repository implements a Rust Git extension. Read `README.md`, `docs/architecture.md`, and `docs/scope-policy.md` before changing lifecycle or scope behavior.

- Use native copy-on-write cloning. Never silently fall back to copying or share regular source files with hard links or source-pointing symlinks.
- Do not change the original worktree's contents or permissions.
- Persist the immutable creation commit and preserve independent Git indexes and `HEAD`s.
- Verify actual cloned contents. Timestamp-based clean status alone is insufficient.
- Treat cleanup as ownership-sensitive. Never delete a preexisting directory or branch. Keep ambiguous interrupted resources for inspection.
- Editing scope is final-state validation, not write enforcement. Preserve all user edits when checks fail.
- Use NUL-delimited Git output for paths. Cover Unicode, spaces, and newlines without shell interpolation.
- Do not modify global Git configuration or install hooks automatically.

For your own isolated work, use `git thin doctor`, then `git thin add PATH --branch BRANCH --write PATH...` once the executable is built. Choose explicit scope paths, expand with `git thin scope add` when needed, and run `git thin check` before handing work back, including after commits. Preserve uncommitted and untracked work before cleanup. Use `git thin remove --dry-run` to inspect removal; ordinary removal preserves branches and commits.

Run formatting, Clippy, and relevant tests before handing off changes. Successful Linux tests do not prove the macOS backend; use real APFS integration tests for APFS claims. Do not run an 8 GB benchmark as a routine test. Follow `docs/benchmarks.md` on a deliberately prepared volume and report measurement limits.
