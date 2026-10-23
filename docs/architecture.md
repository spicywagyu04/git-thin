# Architecture and recovery

`git-thin` is a Rust executable invoked either directly or as `git thin`. It delegates repository operations to the installed Git executable and keeps native filesystem cloning in a small backend. Git remains the authority for objects, branches, linked worktree registration, `HEAD`, and each worktree's index.

## Creation

Creation resolves the source `HEAD` to an immutable object ID before doing work. Preflight checks require a complete source checkout with no tracked staged or unstaged changes, no merge or rebase in progress, and no unsupported tree or checkout features. Untracked and ignored source files are not copied.

The backend probes actual cloning support and checks that source and destination use the same filesystem. macOS uses APFS cloning through `clonefile`; Linux uses `FICLONE`. An unsupported operation fails without a copying fallback. No source file's permissions or contents are changed.

Git registers a uniquely named sibling staging worktree with `git worktree add --no-checkout`. The requested destination must not already exist. The tracked tree is enumerated using Git's NUL-delimited machine-readable output. Regular files are cloned and supported tracked symlinks are reproduced as links, never traversed as source files. Executable modes are preserved. Relative symlinks that stay inside the worktree are supported; absolute or escaping links are rejected.

`git read-tree` initializes the destination index from the recorded commit without writing its files. Creation verifies actual file contents and modes against that commit. A timestamp-only clean check is insufficient: concurrent source edits can occur between preflight and cloning. If verification finds a mismatch, creation fails and recovery handles only resources belonging to that operation.

The staging worktree is moved into its final destination after verification using an atomic rename that refuses an existing destination, then `git worktree repair` updates Git's registration. Recovery can finish an interrupted publication. The worktree is reported as ready only after metadata persistence. No full-size baseline cache is created. Editable files use the same initial cloning path as every other tracked regular file. JSON creation results expose cloning and verification durations separately.

## Metadata and locks

Tool metadata lives in the repository's Git metadata area, outside tracked source files. It records a schema version, stable worktree identity, creation commit, filesystem backend, editing scope, and operation state. The creation commit is never silently replaced after a branch switch or rebase.

A repository-wide lock serializes git-thin creation, removal, scope updates, and recovery. This is advisory coordination between tool instances; it does not prevent a user from modifying the repository with another program.

Operations persist enough ownership information for recovery. The important rule is conservative cleanup: an interrupted operation must not delete a preexisting directory or branch. If ownership or current state cannot be established, recovery retains the resource and reports the ambiguity. Recovery preserves branches, including an unused branch created by the failed operation. Ordinary removal preserves branches and commits and refuses uncommitted, untracked, or ignored files. It also refuses a `HEAD` commit unreachable from any local branch; create a branch to retain detached commits before cleanup.

Use the CLI to inspect pending work:

```sh
git thin list
git thin recover --dry-run
git thin recover
```

Do not manually delete metadata to silence a failed operation before inspecting the affected worktree and branch. Do not run recovery concurrently with external worktree-moving or cleanup tools.

Recovery refuses unexpected files, changed tracked contents, or staged user work in an interrupted staging directory. It also retains metadata for moved or orphaned registrations that need manual repair. A crash between the exclusive staging-directory reservation and journal persistence can leave an empty unregistered directory; recovery deliberately does not guess ownership of such a directory.

## Scope checks

The recorded creation commit is the baseline. Scope validation compares the current final state, including tracked contents and new non-ignored files, against that commit. It reports paths for additions, modifications, deletions, and both sides of renames. A committed change can still violate the scope. An incompatible `HEAD` history is an error instead of a new baseline.

Scope checks do not undo changes and are not an operating-system write barrier. See [the policy](scope-policy.md) for exact user-facing expectations.

## Filesystem behavior

Copy-on-write sharing concerns data extents, not path visibility. All tracked paths are present. Regular-file clones have independent identities; the backend does not use hard links or links back into the source checkout to share regular-file contents.

Data allocation depends on the filesystem, snapshots, filesystem metadata, compression, and how applications save files. Summing file sizes measures logical content, not distinct physical extents. Even per-file allocated-block counts can attribute shared extents to multiple files. The benchmark procedure therefore measures a controlled filesystem and reports its limitations.

## Future milestones

Strict write enforcement needs a separately designed and tested sandbox or filesystem layer covering direct writes, atomic saves, renames, deletion, symlink traversal, and subprocesses. LFS, checkout filters, sparse sources, submodules, and overlays require their own compatibility work. None are implied by the MVP's scope validation.
