# Functional readiness evidence

Status recorded October 24, 2026: code candidate
`d75269a618336306c98a95f8a6c8d07ac6d7ea64` is committed and frozen. All eight full
validation configurations passed, including both APFS case modes, native Btrfs,
and exact minimum Rust/Git. Both native production binaries passed 95 control
cases. **Local functional validation is complete, including the audited twelve-trial
APFS benchmark matrix. Matching hosted CI has not run.** The final scope result
and documentation-only handoff commit are recorded in `final-candidate/handoff.json`.
These local results do not authorize or establish public-release readiness.

## Candidate and evidence identity

| Item | Identity |
| --- | --- |
| Retrieved baseline | `df398dc7d111fb1b22af4fa0dc5b82f2426d8a1e` |
| Implementation branch | `codex/functional-readiness` |
| Final code candidate | `d75269a618336306c98a95f8a6c8d07ac6d7ea64` |
| Final source proof | `final-candidate/{candidate.txt,source-tree.txt,source-proof.json}`; all 36 tracked files match clean HEAD |
| macOS production binary | `final-candidate/git-thin-d75269a-macos-arm64`; SHA-256 `0eacff702ceb21ebb11c5fe6cc1fb7449fdf2713fb8cfb4c5df2df6663b869bd`; Rust 1.89.0, no `fault-injection` |
| Linux production binary | `linux-native-final/git-thin-linux-aarch64-production`; SHA-256 `517196a220dd22aceb9b6275bd9669aa07bded6d87c74c6cbb6855e69eb968a2`; Rust 1.99.1, no `fault-injection` |
| Final test accounting | `final-candidate/test-accounting.json` and `.md`: named tests, raw/executed/unavailable/inapplicable counts, completion checks and log hashes |
| Native Linux index | `linux-native-final/index.json`; SHA-256 `846f2f665c42b3a48ca31bfc30daab419fb9e9c62a488358ac93ddf2ce009ffb` |
| Full APFS matrix | `full-apfs/summary.json`; SHA-256 `18df314af963b412c2b90b0e81e82c774c8222c0aadea1bc07aa2f9974fd2b4d`; 12/12 trials, independently reconciled in `full-apfs/audit.json` |
| Matching hosted CI | **Not run.** Configured jobs and local execution are not a completed hosted CI run. |

Work is isolated in the managed git-thin worktree at
`/Users/kevinshao/Desktop/Projects/sailfish/readiness`; the original parent
checkout's uncommitted seed was preserved. No release, tag, package publication,
repository-visibility change, global Git configuration change or hook installation
is part of this work.

The durable local evidence bundle is
`/Users/kevinshao/Desktop/Projects/sailfish/readiness-evidence`. Its `SHA256SUMS.json`
records every other file’s byte count and SHA-256 hash. Filenames below refer to
bundle-relative paths. Original execution paths remain in the raw reports as
provenance. Large benchmark fixtures do not belong in Git.

Earlier candidates `1886b8fa1436271483280605c4a56d2f79fa366c`,
`976b3e95c1d9bb70971c76365ef318b783e6e5ee` and
`92bb70342c21eb4869dd76bcf400c89af20d0384` retain their exact test/binary identities.
Their results are not relabeled as final. The final candidate fixes the last
review finding: source validation checks attributes for HEAD and current-index
paths before status can execute a clean filter, including newly staged and
intent-to-add paths. The sentinel regression executed in all eight final runs.

## Actual final validation

Counts are Rust test functions, not internal loop scenarios. A Rust `ok` after an
unavailable or inapplicable return is not an executed pass. Compiled-out functions
are absent, not unavailable. All eight runs below use the final candidate.

| Platform and toolchain | Features | Raw `ok` | Executed | Unavailable | Inapplicable | Failed |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| macOS 15.7.7 arm64, ordinary APFS, Rust 1.89.0, Apple Git 2.50.1 | All | 121 | 118 | 0 | 3 | 0 |
| Same | Default | 105 | 102 | 0 | 3 | 0 |
| macOS case-sensitive APFS, Rust 1.99.1, Apple Git 2.50.1 | All | 121 | 118 | 0 | 3 | 0 |
| Same | Default | 105 | 102 | 0 | 3 | 0 |
| LinuxKit 6.10.14 aarch64, Btrfs, Rust 1.99.1, Git 2.56.0 | All | 119 | 118 | 0 | 1 | 0 |
| Same | Default | 103 | 102 | 0 | 1 | 0 |
| Ordinary Linux overlayfs, Rust 1.89.0, exact Git 2.39.0 | All | 119 | 82 | 37 | 0 | 0 |
| Same | Default | 103 | 82 | 21 | 0 | 0 |

All ten expected test binaries completed in every run; zero ignored, measured or
filtered-out tests. Ordinary APFS default also completed an empty doctest suite.
APFS logs are `final-candidate/{all-features,default,case-sensitive-all-features,case-sensitive-default}.log`.
Linux logs are `linux-native-final/{all-features,production-tests,minimum-all-features,minimum-default}.log`.
Formatting, all-feature/default clippy and production builds passed in their
recorded lanes. Case-sensitive full suites used stable Rust; their source trees
before and after match the final 36-file record.

APFS has three inapplicable functions: two invalid-UTF-8 filename tests (APFS
rejects those names before tool inspection) and unsupported-clone rejection on a
clone-capable filesystem. Btrfs has only the latter; invalid-UTF-8 assertions execute
there. Ordinary overlayfs executes unsupported-clone rejection but cannot execute
native clone/volume scenarios. Its unavailable groups are not native evidence.

Both APFS case configurations treat composed/decomposed Unicode names as aliases;
only the case-sensitive volume distinguishes case. Actual colliding Git-tree
regressions executed in both configurations. Btrfs distinguishes case and Unicode
normalization variants. Capability JSON preserves those observed facts.

Native tests used distinct source/cross/exhaustion filesystems. APFS exhaustion
capacity was 134,176,768 bytes. Btrfs volumes were 2 GiB/256 MiB/128 MiB; direct
FICLONE proved byte identity, distinct inodes and source independence after edits.
Actual cross-volume rejection, startup/post-partial-clone ENOSPC and unprivileged
EACCES executed on both native platforms. APFS exhaustion runs were serialized.
Linux tests ran as uid 1000 with effective/permitted/inheritable/ambient capabilities
zero; user-authorized privilege provisioned disposable mounts only. Native Linux
containers and loop associations were removed after evidence and fixture metadata
were retained. The index records completed cleanup. All twelve completed APFS
trial images and the four additional task-owned APFS test images were detached
and removed after evidence retention; `apfs-cleanup.json` records cleanup.

Both archived binaries were independently hashed and passed all 95 production
controls with zero unavailable cases (`final-candidate/production-smoke.json`,
`linux-native-final/production-smoke.json`). Linux source was copied without `.git`;
raw benchmark Git-identity fields remain unavailable. Its 36-file SHA-256/Git-blob
manifest and matching measured binary hash establish provenance instead.

## Requirement-to-test ledger

All test files are repository-relative. Results below are final-candidate results
unless a baseline reproducer is explicitly named.

| ID | Concrete tests or experiments | Observed outcome and limit |
| --- | --- | --- |
| R1: complete linked checkout; independent HEAD/index/file identities | `tests/integration.rs`: `independent_contents_indexes_and_ordinary_git_commits`, `three_native_siblings_remain_independent_across_realistic_save_patterns`; native empty-tree/linked-source compatibility cases | Passed on native APFS and Btrfs. Source plus three siblings cover in-place/replacement writes, append/truncate, rename, executable changes and deletion. |
| R2: native cloning, no copy fallback, same filesystem | Unsupported-clone rejection; actual cross-volume rejection; backend no-replace/identity assertions; Btrfs direct FICLONE proof | Native success and cross-volume failure passed on APFS/Btrfs. Ordinary overlayfs exercised the unsupported path with no fallback. |
| R3: preserve source/destination/refs/configuration/hooks | Destination/ref sentinels; hook/fsmonitor/external-diff suppression; source-index filter regression; concurrency/recovery snapshots | Native preservation passed. New staged/intent-to-add filter paths reject before `doctor`, `add --dry-run` or `add` can run filters; fixture-local configuration remains unchanged. |
| R4: verify actual bytes and executable modes | Source mutation at clone boundary, restored timestamps/replacements/chmod/link/parent changes; `git::hashing_tests`; SHA-256 linked-source test | Passed on native platforms. Streamed SHA-1/SHA-256 Git blob hashes match Git; length/read failures do not produce trusted digests. The separate matched 1,024-file baseline/final probe measured creation 31.395 → 1.365 s, internal verification 17.429 → 0.207 s and scope checking 13.245 → 0.316 s. This single observation has uncontrolled cache/background conditions and does not establish a general speedup; it is distinct from the thin-versus-checkout 8 GB matrix. |
| R5: fixed baseline and HEAD/index/worktree union, intent-to-add, exit codes | Unit/CLI intent-to-add, deleted working files and empty-baseline staged deletion; committed/staged/rename/ignored-path tests | Baseline failures retained in `scope-before.log` and `scope-cli-before.log`; final unit/CLI suites pass everywhere. Both visible and invisible intent-to-add comparisons preserve violations and input snapshots. |
| R6: literal safe paths, exact/recursive scopes | Future/no-write/root scopes; replacements/cross-boundary renames; literal/dangling/cyclic links; actual case/Unicode collision trees | Passed all applicable native cases. Invalid-UTF-8 rejection executes on Linux; filesystem-impossible APFS cases remain inapplicable. |
| R7: conservative removal and retained commits/branches | Three cleanup tests covering staged-only/flags/stat-cache, switched/renamed branches and locked worktrees; detached commits and removal recovery | All pass across full native/default/minimum runs. Unreferenced detached commits and user work are retained until explicitly rescued/cleaned. |
| R8: durable phases and conservative repeatable recovery | Staged-deletion/edit/flag/schema/index-transition matrix; inaccessible/moved paths; moved-removal after rename/detachment; lifecycle faults; real ENOSPC | Final 23 recovery and 9 native lifecycle tests pass. Baseline staged-deletion failures are retained in `recovery-before.log`. Journals and staged/unstaged/untracked/ignored work survive ambiguity. No hardware power-loss claim. |
| R9: process-level repository locking | Four concurrency tests; explicit ready/continue handshakes; competing destination/branch, independent repositories, killed holder and partial-clone SIGKILL | Passed on both native platforms. Lock release/recovery preserves metadata and refs. Arbitrary concurrent external Git cleanup remains outside the coordination contract. |
| R10: normal Git workflows and immutable creation baseline | Merge/rebase/conflicts, SHA-1/SHA-256, native linked-source/empty-tree and incompatible-history cases | Passed on native APFS/Btrfs and applicable minimum-version portable cases. Baseline is not silently replaced after branch/history changes. |
| R11: unsupported sources, CLI/JSON, compatibility floors, production | Source state/config rejection, all-subcommand JSON/usage/dry-run; exact Rust 1.89/Git 2.39.0; Rust 1.99.1/Git 2.56.0; production controls | All applicable final checks pass. Both native production binaries built without `fault-injection` ignore 95 tested controls. Matching hosted CI is still unrun. |
| R12: measured allocation/performance and limits | Current APFS/Btrfs four-way smoke; final 1,024-file scaling probe; missing-counter/old-report helpers; full randomized matrix | Both native four-trial smoke matrices and all twelve final-candidate 8 GB APFS trials pass. The audit reconciles 36 worktrees, 84 independent full-content reads and every required RSS/APFS counter. Thin creation allocated fewer net bytes but took longer than checkout in these observations. Small Linux probes do not support full quantitative Linux claims. |

## Lifecycle boundary evidence

Feature-enabled controls cover deterministic returned errors and abrupt
`exit(86)`. Separate tests exercise actual SIGKILL, unprivileged EACCES and real
ENOSPC; those categories are not interchangeable.

| Boundary | Preservation evidence |
| --- | --- |
| Reservation / first journal | Transition/replacement tests retain unjournaled or replaced directories rather than guessing ownership. |
| Registration / partial clone | Boundary matrix and actual SIGKILL after the first clone retain branches and permit conservative recovery. |
| Index intent / initialization / journal transition | Known pre-index state is distinguished from ambiguous mid-index state and complete verified index; staged deletions and intent-to-add flags survive. |
| Verification / publication | Before/after verification, publication intent, exclusive rename, repair and ready journal are exercised; rollback does not delete published destinations. |
| Removal intent / Git removal / journal deletion | Retry is conservative and repeatable; relocated/orphaned owned registrations retain their journal after branch rename or detachment. |
| Serialize / write / file sync / persist / directory sync | Every metadata stage has fault placement; malformed/unknown records block all-record cleanup. This does not simulate hardware power loss. |
| ENOSPC / EACCES / EIO | Injected errors establish placement; real failures establish OS behavior. Dedicated APFS/Btrfs fills are bounded to 256 MiB and 65,536 empty inodes, with 1 MiB sync intervals and release before assertions. |

See [architecture and journal compatibility](architecture.md#journal-compatibility-and-index-ownership)
for schema-2 ordering and retained ambiguous schema-1 journals. Schema-1 ready
worktrees remain usable; old/mid-index state is not reclassified as disposable.

## Measurements and retained negative evidence

**Full final-candidate matrix: COMPLETE.** All twelve trials completed: large/small
layouts × thin/checkout × three repetitions, in a seeded randomized order. Each
trial used 8,000,000,000 logical source bytes, three worktrees and 16 MiB of
in-place edits per worktree. Small layout contained 122,071 files; large layout
contained one file. `full-apfs/audit.json` independently reconciles the raw
reports and summary: 36 worktrees, 84 independent full-content reads, 576 MiB of
actual edits, a clean measured source checkout for every trial, and all required
RSS and APFS counters available. See [measured results](measurements.md) for the
full report, also retained as `benchmark-results-final.md` in the evidence bundle.

Medians below summarize three repetitions; creation time covers all three
worktrees, allocation uses decimal GB, and edit deltas use MiB.

| Layout / method | Creation seconds | APFS creation delta, GB | APFS edit delta, MiB |
| --- | ---: | ---: | ---: |
| Small / thin | 589.25 | 0.160 | 48.08 |
| Small / checkout | 241.76 | 24.204 | -1.85 |
| Large / thin | 167.84 | -0.007 | 48.00 |
| Large / checkout | 61.09 | 24.035 | 0.00 |

Thin creation took longer than checkout in both layouts despite lower observed
net allocation. Negative/zero volume deltas can reflect metadata, deferred frees and
background activity: they do not mean negative bytes written or identify exact
shared extents. No fixed storage percentage or general speedup is promised.

Creation wall time includes source validation, clone/index setup, mandatory
internal verification and publication. The harness's post-creation, pre-edit and
final-source full reads are additional work reported separately. Internal clone
and verification timings truncate to whole milliseconds; a recorded zero is not
proof of zero work. All final tests/builds finished before the serial matrix;
no test/build/benchmark workload overlapped its trials. Fresh unencrypted 64 GiB
APFS images were used, with seeded data and Git compression disabled; caches were
warm/unspecified, host encryption and desktop activity were not controlled.
These conditions and all ranges are retained in the full report.

RSS is the platform command high-water mark, not a sum of simultaneous
parent/child memory or the Python verifier's memory. Every applicable command
must supply a counter before a trial peak is accepted. Separate phase timings,
RSS, initial allocation and edited allocation remain in the raw reports. Only
in-place editing was measured for allocation; replacement-write correctness tests
do not imply a measured replacement-write allocation result.

`final-candidate/apfs-smoke/summary.json` and `linux-native-final/smoke-summary.json`
contain the final four-way small probes. The matched 1,024-file optimization probe
uses 1 MiB total data, one worktree, 4 KiB edits and the same fixture seed. Baseline
`df398dc` and final `d75269a` creation measured 31.395 and 1.365 seconds;
internal verification measured 17.429 and 0.207 seconds; scope checking measured
13.245 and 0.316 seconds. Raw reports are `baseline-scaling-1024.json` and
`final-candidate/scaling-1024.json`. `baseline-binary-source-proof.json` links the
separately built baseline binary to its actual clean source; the baseline raw
report's harness-checkout identity is not its binary provenance. This one-shot
comparison supports the observed reduction from per-file Git processes to
streamed hashing while preserving byte verification, with uncontrolled cache and
background activity. It does not replace the full matrix or support a universal
speedup claim. Full quantitative Linux performance remains unmeasured.

`full-apfs-superseded-1886b8f` and `full-apfs-superseded-92bb703` retain earlier
completed/partial experiments. The two `92bb703` trials completed around a pause
for user-authorized native Linux work; their pause record establishes no timed
overlap. Both were superseded after source-filter review. None of those values
may be pooled with final-candidate trials. Earlier small probes retain their own
embedded hashes/parameters and historical accounting remains untouched.

Negative evidence is preserved: original intent-to-add/staged-deletion failures,
locked-worktree/moved-removal/filter review regressions, and early ENOSPC harness
attempts. The first fill attempt timed out with 64 KiB sync intervals; the second
showed that data-only filling can leave enough APFS metadata for creation. The
corrected bounded data-and-metadata fill passes actual failures on both native
platforms. Earlier failed harness attempts are not successful exhaustion evidence.

The first instrumented smoke failed after Git completed when the sandbox denied
`time`'s `kern.clockrate` query. The harness now probes instrumentation before
mutating commands and records unavailable counters without retrying a possibly
completed creation. Missing-counter helpers and parameter-mismatch checks prevent
partial memory readings or tiny reports being presented as full measurements.

## Phase and final acceptance gates

| Phase | State and remaining work |
| --- | --- |
| A: baseline / isolation / capabilities | Recorded; original seed preserved. Baseline APFS: 63 raw entries = 58 executed + 3 inapplicable + 2 unavailable. Source inspection established the old silently returned unsupported-clone case; earlier inflated intermediate counts are superseded. |
| B: intent-to-add | Reproduced and fixed, including empty-baseline deletion; all final scope/CLI suites passed. |
| C: staged deletion / journal compatibility | Reproduced and fixed; final 23 recovery tests passed in all eight configurations. |
| D: lifecycle failures | Nine native lifecycle tests, real permissions, SIGKILL and real ENOSPC passed on APFS and Btrfs. |
| E: concurrency / preservation | Four deterministic native process tests passed in both APFS case modes and Btrfs. |
| F: compatibility / editing / paths / CLI | Eight full final configurations completed, including exact minimum versions and final filter-execution regression. |
| G: enforceable CI | Required native/minimum/production lanes implemented and corresponding local environments exercised. Matching hosted CI remains unrun. |
| H: measurement | Final native small probes, helper checks and all twelve APFS trials passed; raw reports, allocation/RSS/timing counters and independent audit are complete. Full quantitative Linux claims are not made. |
| I: review / handoff | Final candidate, full tests, native production binaries, source proofs and audited matrix recorded. The durable bundle manifest covers retained evidence; `final-candidate/handoff.json` records the documentation commit, clean checkout and scope result. No publication performed. |

Required native lanes set `GIT_THIN_REQUIRE_CLONE=1`, put `TMPDIR` on the intended
filesystem and validate distinct cross/exhaustion roots with
`scripts/validate_environment.py`. See [contributing](../CONTRIBUTING.md) and
[benchmarks](benchmarks.md). A retained all-at-once full matrix budgets 432 GB plus
reserves; a dedicated image may be reset between selected trials only after
preserving reports. Record image/cache/snapshot/compression/encryption and
background-activity conditions alongside all performance claims.
