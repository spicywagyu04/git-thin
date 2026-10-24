# Preserve staged work and validate the native worktree lifecycle

An out-of-scope file could disappear from `check` after `git add -N`, and recovery
could discard staged deletions in interrupted worktrees. Scope checks now combine
both Git intent-to-add index views, including empty baseline files. Schema-2
journals distinguish pre-index, ambiguous initialization, and verified-index
states so recovery preserves staged work. Existing ready schema-1 worktrees
remain usable; ambiguous unfinished journals are retained for inspection.

Recovery also retains inaccessible paths, replaced reservations, and moved
registrations, including interrupted removal after a branch rename or detachment.
Locked worktrees are rejected before changing their journals. Source preflight
checks attributes on both committed and indexed paths before Git status can run
a clean filter on a newly staged file. The regression covers same-length working
edits, ordinary staging, intent-to-add, and special filenames across doctor,
creation preview, and creation.

Deterministic process tests cover lifecycle boundaries, metadata failures,
competing operations, source changes, and realistic independent editing. Path,
Git-workflow, scope, JSON, and unsupported-source coverage is expanded. Raw-byte
verification remains mandatory. Streaming Git blob hashing for SHA-1 and SHA-256
removes one Git process per file while retaining SHA-1 collision detection.

Native CI lanes require real cloning and dedicated test volumes; summaries
separate executed, unavailable, and inapplicable tests. The benchmark harness
retains exact binary identity, complete verification timing, memory measurements,
and raw allocation counters.

Validation on code candidate `d75269a`:

- Both original defects and later review findings have retained failing
  regressions and passing corrections.
- Ordinary APFS/Rust 1.89 and case-sensitive APFS/Rust 1.99.1 each passed full
  suites: 118 executed all-feature tests and 102 default-feature tests, with
  three explicitly inapplicable cases per suite. Cross-volume rejection,
  unprivileged EACCES, and startup/post-clone ENOSPC actually ran.
- Native Btrfs/Rust 1.99.1/Git 2.56.0 passed 118/102 executed tests, with one
  explicitly inapplicable case per suite and no unavailable cases. Tests ran as
  uid 1000 with zero effective/permitted/ambient capabilities on task-owned
  native, cross-volume, and exhaustion filesystems.
- Ordinary Linux/Rust 1.89/Git 2.39.0 passed 82 executed tests per full suite;
  37/21 native scenarios were explicitly unavailable. Native success is proved
  by the separate Btrfs runs. The final filter regression executed in all eight
  full configurations.
- Formatting, required lint checks, and production builds passed. Both final
  production binaries ignored all 95 test-control values. Four small benchmark
  combinations passed on each native platform.
- All twelve 8 GB APFS trials passed: 36 worktrees, 84 independent full-content
  reads, three repetitions per layout/method, and all allocation/RSS counters.
  Median many-file creation added 0.160 GB for thin versus 24.204 GB for ordinary
  checkout, but took 589.25 versus 241.76 seconds. Large-file creation also took
  longer (167.84 versus 61.09 seconds). Net allocation counters include metadata
  and deferred frees; negative deltas are retained and explained in
  [measured results](measurements.md).

See [readiness evidence](readiness.md) for the R1–R12 coverage ledger, exact
candidate/binary identities, retained logs, measurements, and remaining limits.
Hosted CI has not run on this local branch. No PR, push, merge, tag, release,
package publication, or repository visibility change has been performed.

Scope validation remains a final-state check. The lock coordinates git-thin
instances; arbitrary concurrent external Git cleanup is outside the contract.
Process-interruption tests do not establish hardware power-loss durability.
Allocation observations include metadata and do not identify exact shared extents.
