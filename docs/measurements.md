# Measurements on APFS

Code candidate `d75269a618336306c98a95f8a6c8d07ac6d7ea64`, using the
frozen Rust 1.89.0 production binary. See [readiness evidence](readiness.md)
for its exact binary hash, source proof, validation results and artifact location.

Thin worktrees used much less additional allocation in these trials, but
creation was slower: median totals were 167.84 versus 61.09 seconds for
large files and 589.25 versus 241.76 seconds for small files. The many-file
thin median added 0.160 GB for three worktrees, compared with 24.204 GB
for ordinary checkout. Mandatory verification was a substantial measured
part of thin creation. These observations support a storage tradeoff, not
a creation-speed claim.

## Full APFS matrix

Twelve trials completed on the clean code candidate: three repetitions of each
layout/method combination, in a seeded randomized order. Each trial used
8,000,000,000 logical source bytes, three worktrees, and 16 MiB of in-place
edits in each worktree (48 MiB total). The large layout has one file; the
small layout has 122,071 files of at most 64 KiB. Values below are medians
with [minimum–maximum] across three trials, not best-case selections.

### Creation and scope timing

All times are seconds, summed across the three worktrees in a trial.
Creation includes the tool’s preflight and mandatory destination verification.
The internal verification phase includes destination byte/mode checks and
Git status. Source preflight, index setup, persistence and publication are
included in total creation but are not individually timed.
Ordinary Git checkout does not report comparable clone/verification phases.
Clone phase counters truncate to whole milliseconds per worktree; a recorded
zero means less than one millisecond, not literally zero elapsed time.

| Layout / method | Total creation | Clone phase | Internal verification | Scope checks |
| --- | ---: | ---: | ---: | ---: |
| large / thin | 167.84 [163.39–167.96] | 0.000 [0.000–0.000] | 111.97 [111.58–115.91] | 51.48 [51.34–55.27] |
| large / checkout | 61.09 [60.30–67.90] | — | — | — |
| small / thin | 589.25 [568.56–604.81] | 121.384 [116.438–122.781] | 304.65 [291.57–317.50] | 167.53 [167.11–172.10] |
| small / checkout | 241.76 [237.93–244.17] | — | — | — |

### Independent verification and command memory

Independent verification is additional harness work: three post-creation
passes, three pre-edit independence passes, and one final source pass. These
seven passes are excluded from creation time. Peak RSS is the largest command
RSS in a trial, in MiB; it is not aggregate simultaneous process-tree memory.

| Layout / method | All independent checks (s) | Creation peak RSS (MiB) | Scope peak RSS (MiB) |
| --- | ---: | ---: | ---: |
| large / thin | 30.60 [30.08–33.51] | 4.91 [4.88–5.03] | 4.78 [4.75–4.80] |
| large / checkout | 31.79 [30.80–33.93] | 7634.73 [6391.19–7635.75] | — |
| small / thin | 236.52 [229.82–242.48] | 112.88 [103.56–114.86] | 116.19 [113.27–117.11] |
| small / checkout | 236.06 [235.41–273.50] | 31.05 [30.48–31.56] | — |

### Allocation observations

Creation deltas subtract the committed source snapshot from the snapshot
after all three worktrees. Edit deltas compare that snapshot with the final
edited snapshot. These are APFS volume counters including metadata, not
exact accounting of shared data extents. Negative or zero net deltas can reflect
metadata changes and deferred frees; they are not negative bytes written or
exact shared-extent savings. GB below means 1,000,000,000 bytes;
MiB means 1,048,576 bytes. Raw statvfs and APFS counters are retained.

| Layout / method | Creation allocation delta (GB) | In-place edit delta (MiB) |
| --- | ---: | ---: |
| large / thin | -0.007 [-0.007–-0.005] | 48.00 [48.00–48.00] |
| large / checkout | 24.035 [24.027–24.043] | 0.00 [0.00–0.00] |
| small / thin | 0.160 [0.160–0.162] | 48.08 [48.07–48.08] |
| small / checkout | 24.204 [24.203–24.205] | -1.85 [-1.86–-0.93] |

Every report completed full-content and source/sibling independence checks.
All applicable command memory and APFS allocation counters were available.
The write pattern is in-place overwrite; atomic-save correctness is established
by editing tests and has no separate allocation claim here.

Conditions: Apple M1 Pro (8 cores), 32 GiB RAM, macOS 15.7.7 (24G720),
Apple Git 2.50.1, Rust 1.89.0 production binary, Python 3.11.0. Each trial
used a fresh, unencrypted 64 GiB case-insensitive APFS sparse image with no
intentionally created snapshots. Host-volume encryption and desktop services
were not changed or controlled. Data was seeded pseudorandom and Git
compression disabled. Caches were warm/unspecified; no reboot or cache flush
occurred. All test/build work finished before this final matrix; trials ran
sequentially without overlapping builds, test suites, or other benchmark runs.

Disk-image backing, caching, deferred allocation/frees, metadata, and unrelated
desktop activity limit generalization. These measurements do not promise a
fixed speedup or storage percentage, and do not establish Linux performance.
Completed trial images were reset only after retaining their raw reports.

Raw reports and the checked aggregate are in `full-apfs/`;
`hardware.json`, `final-candidate/release-identity.json`, and each trial’s environment
notes record provenance. See [benchmark procedure](benchmarks.md) to reproduce.

## File-count optimization probe

These are separate single observations using 1 MiB split into 1,024 files
of 1 KiB, one worktree and 4 KiB of in-place edits, with the same fixture seed.
They measure the change from one Git hashing process per file to bounded
streamed Git-blob hashing, retaining byte verification and SHA-1 collision
detection. Caches and desktop background activity were not controlled.

| Source | Creation (s) | Clone phase (s) | Destination verification (s) | Scope check (s) |
| --- | ---: | ---: | ---: | ---: |
| Audited baseline `df398dc` | 31.395 | 0.330 | 17.429 | 13.245 |
| Final candidate `d75269a` | 1.365 | 0.292 | 0.207 | 0.316 |

These one-shot probes are not the twelve-trial matrix and do not establish
a general speedup. The older baseline report captured the harness checkout
identity; its measured binary was built separately from the clean audited
baseline. `baseline-binary-source-proof.json` links that binary hash to its
actual source. Final-probe source and binary identity are recorded directly.

## Linux evidence boundary

Native Btrfs correctness, recovery and all four 1 MiB smoke combinations
passed on the final candidate. The smoke runs validate execution and counters.
A full Linux performance/allocation matrix was not measured; none of these
APFS quantitative results are transferred to Linux.
