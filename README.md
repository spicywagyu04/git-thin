# git-thin

Independent Git worktrees that share unchanged file data through native filesystem copy-on-write. `git-thin` is a working name; the name and publication destinations must be checked before a release is published.

The tool creates a real linked worktree on a new branch at the source checkout's current commit. Every tracked file is present, with its own file identity. Normal Git staging, commits, diffs, and merges continue to work. There is no full checkout followed by deduplication, separate baseline cache, or silent copying fallback.

**Editing scope is validation, not write protection.** `--write` declares intended edits. `git thin check` reports final-state changes outside that scope; it does not block writes or prove that a file was never temporarily changed. Read [the scope policy](docs/scope-policy.md) before assigning work to an agent.

## Install from this checkout

Install a current stable Rust toolchain (Rust 1.89 or later) and Git 2.39 or later, then run from this repository:

```sh
cargo install --path . --locked
git thin -h
```

Cargo installs `git-thin` into its binary directory, which must be on `PATH`. Git discovers it as `git thin`. No package registry or downloadable release is assumed to exist. See [release preparation](docs/releases.md) for binary packaging and checksums.

Use `git thin -h` or `git-thin --help` for built-in help. Git reserves `git thin --help` for its external manual-page viewer.

## Use

Run these commands from a source repository with a clean tracked checkout:

```sh
git thin doctor

git thin add ../billing-agent \
  --branch agent/billing \
  --write services/billing \
  --write packages/shared \
  --write package.json

git -C ../billing-agent status
# Edit, stage, and commit with ordinary Git commands.
git -C ../billing-agent add services/billing
git -C ../billing-agent commit -m 'Update billing'

git thin scope add ../billing-agent packages/auth
git thin check ../billing-agent
git thin status ../billing-agent
git thin list

git thin remove ../billing-agent --dry-run
git thin remove ../billing-agent
```

Scopes are relative to the worktree root. Existing directories are recursive; files are exact matches. A trailing slash can declare a directory that does not exist yet. Glob matching is not supported. Declaring a path editable does not copy its contents.

Use `--dry-run` with `add` and `remove` to inspect a proposed operation. Use global `--json` for structured output, including errors:

```sh
git thin --json add ../billing-agent --branch agent/billing --write services/billing --dry-run
git thin --json check ../billing-agent
git thin recover --dry-run
```

`recover` inspects interrupted operations. Read its output before recovery, particularly if a process was stopped while creating a worktree. Recovery only removes resources whose ownership can be established; ambiguous resources require manual inspection.

Exit codes are `0` for success, `1` for scope violations, and `2` for usage or operational errors. Creation JSON includes `clone_ms` and `verification_ms` so automation can distinguish cloning from content verification.

Routine removal refuses uncommitted, untracked, and ignored work. Branches and commits are preserved, including branches created by an interrupted creation operation. Commit or otherwise preserve work before removing its checkout; ignored build outputs may consume considerable space too.

If a checkout has detached `HEAD` commits that no local branch retains, create a branch at `HEAD` before removing it. This prevents cleanup from leaving those commits without a local branch reference.

## Supported configurations

| Configuration | Behavior |
| --- | --- |
| macOS, APFS, source and destination on the same filesystem | Native `clonefile` cloning |
| Linux, a filesystem supporting `FICLONE`, same filesystem | Native reflinks; capability is probed, not inferred from an OS label |
| Complete clean tracked checkout at a commit | Required |
| Ignored or untracked source files | Left in place; not copied |
| Executable files and internal relative tracked symlinks | Preserved |
| Absolute or escaping symlinks | Rejected |
| Sparse checkouts, submodules, Git LFS, custom checkout filters | Rejected |
| Different filesystems or unsupported clone operations | Rejected before a large checkout is created |

The source must not have skip-worktree or assume-unchanged index entries. `core.autocrlf` must be unset or false. Active `filter`, `working-tree-encoding`, `ident`, and `eol` attributes are rejected. Other checkout transformations that make working-file bytes differ from the recorded blobs also fail content verification. A failure is an error, never permission to fall back to full copying. `doctor` checks the current location; `add` also checks its actual destination.

The current implementation targets Unix paths. Filenames containing spaces, Unicode, and newlines are supported; non-UTF-8 paths are rejected where metadata cannot represent them safely.

## Storage and safety limits

- Clones have independent file identities. Editing either checkout does not edit the other. Git metadata and indexes remain independent where Git requires it.
- Physical allocation grows with newly written data and the filesystem's allocation behavior. Atomic-save applications can replace a whole file. A one-line edit is not a promise of a one-line disk allocation.
- Branch switching and rebasing can replace files and allocate additional storage.
- Content verification reads the cloned data. Creation time depends on repository size and file count, even when cloning itself is inexpensive.
- New non-ignored files participate in scope checks. Ignored generated outputs are separate from scope and still consume space. Ignore rules never exempt an already tracked file.
- This is a developer tool, not a sandbox or a security boundary. Filesystem permissions, Git hooks, and post-run checks do not provide strict isolation from a hostile process.
- Do not run general Git worktree removal or relocation concurrently with a git-thin lifecycle operation. Its repository lock coordinates other git-thin processes.

See [architecture and recovery](docs/architecture.md), [benchmarks](docs/benchmarks.md), and [contributing](CONTRIBUTING.md). Benchmark instructions are provided; no APFS savings result is claimed before it has been measured on real APFS hardware or a controlled APFS volume.

## License

[MIT](LICENSE).
