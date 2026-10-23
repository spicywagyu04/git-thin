# Editing-scope policy

`--write` expresses the files an agent or engineer is expected to change. It grants no filesystem permission and imposes no write barrier. All ordinary Git and filesystem writes remain possible.

## Paths

Paths are interpreted relative to the managed worktree root:

```sh
git thin add ../agent --branch agent/task \
  --write services/billing \
  --write package.json \
  --write generated-source/
```

- An existing directory includes its descendants recursively.
- An exact file includes only that path, not similarly prefixed names.
- A trailing slash declares a recursive directory, including one not yet created.
- `.` declares the entire worktree.
- Absolute paths and parent traversal are rejected. Glob syntax is not supported.
- Symlink paths do not authorize writes through their targets. Scope validation cannot make a symlink safe for an arbitrary subprocess.

Expand a scope explicitly when a task needs additional files:

```sh
git thin scope add ../agent packages/auth
```

Scope changes are stored outside tracked source files. They do not duplicate file contents and do not change the immutable baseline.

## What a check means

`git thin check ../agent` compares three current snapshots with the commit recorded at creation: `HEAD`, the index, and working files. It takes the union of their changed paths, plus new non-ignored files. An out-of-scope addition, modification, deletion, or either side of a rename is a violation. Tracked paths remain checked even when an ignore pattern matches them.

The check preserves all changes and returns a nonzero status on violations or errors. An incompatible history, such as checking out an unrelated commit, is reported as an error. It does not silently reset the baseline to the new `HEAD`.

Working files are hashed as raw bytes, so index flags, cached timestamps, and line-ending normalization cannot hide edits. Introduced checkout filters and unsupported transformations cause an actionable error; checks do not execute those filters. Reading the tracked contents takes time proportional to the data being inspected.

A final-state check cannot prove that a file was never temporarily modified and restored. A change completely restored to the baseline in all three snapshots is not a final-state violation. Restoring only the working file does not hide a violation still present in a commit or staged version. Likewise, a reverted commit no longer contributes a violation once `HEAD`, index, and working files all match the baseline. Run the check before handing work back, and review the ordinary Git diff as well.

## Generated outputs

New ignored build outputs do not enter source-editing scope checks. They still occupy disk space and can contain useful work. Ignore rules never exempt tracked files. Keep build caches and generated output policies separate from source permissions, and inspect cleanup decisions before removing a worktree.

## Agent handoff

1. Start from a clean tracked source checkout and run `doctor`.
2. Create a new managed worktree with explicit scope and a fresh branch.
3. Give the agent that worktree as its working directory.
4. Expand scope deliberately when required by the task.
5. Run `check` before review, including after commits.
6. Preserve changes, then remove the worktree with git-thin. The branch remains available for merging.

Strict enforcement is a separate milestone. Read-only file modes alone do not cover replacement saves or directory renames, and Git hooks can be bypassed. Neither those mechanisms nor a successful check should be described as a security boundary.
