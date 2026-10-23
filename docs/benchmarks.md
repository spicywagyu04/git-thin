# Reproducible storage benchmarks

No storage-saving or performance result has been published by this repository.
The harness records observations from a real filesystem. An APFS run is required
before making claims about macOS storage savings. A successful Linux run cannot
substitute for that evidence.

## Prepare an isolated filesystem

Use a dedicated APFS volume, ideally in a dedicated container, on an otherwise
idle machine. A disposable APFS disk image is useful for repeatability, but its
host file allocation, caching, encryption and storage stack affect results.
Record that setup in any published report. Do not benchmark on a production
volume containing valuable data. A volume in a shared container is not isolated
from writes to its sibling volumes.

Provide at least 24 GB of free space for the default thin run, and more for
metadata, snapshots and filesystem reserve. An ordinary-checkout control with
three worktrees requires at least 48 GB. The source fixture and Git's object
database each consume substantial storage; neither is hidden from measurements.
The fixture defaults to **8,000,000,000 logical bytes (8 GB)**. Pass
`--bytes 8589934592` for an explicitly labelled 8 GiB trial.

Record the OS version, filesystem format/options, hardware, available space,
snapshot state, Git and git-thin versions, encryption/compression settings,
and background services. Stop unrelated writers and exclude the disposable
benchmark volume from indexing and backup tools as appropriate. Do not remove
existing snapshots on a user's normal data volume to make measurements cleaner.

## Run both fixture layouts

Build the binary from the checkout and resolve its absolute path:

```sh
cargo build --release --locked
THIN_BIN="$(pwd)/target/release/git-thin"
```

Use a new directory for every trial. The parent directory must already exist.
Keep the JSON reports on another volume when possible, outside the measured
directory. Python 3.9 or later is required. These commands allocate real data;
the `--run` flag is deliberately required.

```sh
python3 scripts/benchmark.py /Volumes/ThinBench/large-thin-1 \
  --layout large --git-thin "$THIN_BIN" \
  --output /tmp/large-thin-1.json --run

python3 scripts/benchmark.py /Volumes/ThinBench/small-thin-1 \
  --layout small --git-thin "$THIN_BIN" \
  --output /tmp/small-thin-1.json --run
```

The large fixture is one 8 GB file. The small fixture consists of 64 KiB files,
sharded into directories of at most 1,000 entries, with a smaller final file.
Bytes come from a seeded pseudorandom generator and are deliberately difficult
to compress. The script records the seed and Python version; use the same Python
version, parameters and Git object format for cross-run reproducibility. It sets
repository-local compression to zero and disables automatic Git GC and signing,
and ignores the caller's global/system Git configuration. It does not change
global Git configuration.

Each trial creates three independent worktrees, checks their complete file
contents against Git blob object IDs, then writes up to 16 MiB in place in each
worktree. Earlier edits must leave the next worktree and the source unchanged.
The edits are contiguous for the large-file fixture and span enough small files
to reach the same requested byte count. The report records the actual count.
This models in-place writes, not editors that replace whole files on save.

For a conventional checkout control, repeat on a fresh equivalent filesystem:

```sh
python3 scripts/benchmark.py /Volumes/ThinBench/large-checkout-1 \
  --layout large --method checkout \
  --output /tmp/large-checkout-1.json --run
```

Repeat the control with `--layout small`. Randomize trial order and run at least
three repetitions. Report medians and ranges rather than a single best result.
Do not silently treat warm-cache measurements as cold-cache performance. The
harness reads each file during verification, so later operations are likely to
benefit from cache. If testing cold caches, document a reproducible machine
restart or supported cache-management procedure and its effect on both methods.

A quick harness smoke test can use `--bytes 1048576 --worktrees 2
--edit-bytes 4096`; it is not an 8 GB benchmark and is not evidence of large-tree
performance.

## Read the report

The JSON includes:

| Field | Meaning |
| --- | --- |
| `source_logical_bytes`, `source_file_count` | Fixture data size and file count, excluding Git metadata |
| `creation_wall_seconds` | Complete `add` or ordinary Git checkout elapsed time |
| `creation_result` | Unmodified structured git-thin output, including phase timings when provided |
| `clone_ms`, `verification_ms` | git-thin's own clone and creation-verification phase durations |
| `independent_full_content_verification_seconds` | Separate full read/hash of the completed worktree |
| `scope_check_wall_seconds` | Final-state scope check after edits; distinct from creation verification |
| `measurements` | Synchronized allocation-counter snapshots before source creation, after commit, after each worktree, and after edits |
| `actual_edit_bytes` | Number of bytes written by the controlled editing pass |
| `source_independence_verification_seconds` | Full hash verification that source content remained unchanged |

Creation time includes git-thin's own mandatory content verification. The
independent verification measurement is an additional pass; do not subtract it
from creation time or call the scope check a full creation verification.

Compare the `source_committed` snapshot with `created_1`, `created_2`, and
`created_3` to observe incremental allocation. Compare `created_3` with each
`edited_N` snapshot to observe storage growth after writes. Compare those deltas
with fresh ordinary-checkout controls to estimate savings, with uncertainty.
Subtract counters only when they refer to the same volume/container and clearly
label whether the result includes metadata. Preserve raw counters in reports.

On macOS the harness resolves the directory's device with `df -P`, queries
`diskutil info -plist` for that device and `diskutil apfs list -plist`, and records
the matching volume's `CapacityInUse`, container capacity/free
space, and sibling-volume counters when available. All platforms also record
`statvfs` filesystem counters. Measurement errors are recorded; a missing APFS
counter must not be presented as a measured APFS result. Synchronization happens
outside timed creation and verification sections.

These counters are **controlled filesystem observations, not exact accounting of
the shared data extents belonging to one worktree**. APFS snapshots, metadata,
copy-on-write granularity, deferred frees, reserve accounting, background writes
and shared-container activity can affect them. `sync` does not eliminate all
measurement noise. Sum of file lengths is logical size. Summed `st_blocks` or
`du` output may count shared extents repeatedly and must not be labelled exact
APFS savings. Disk-image host file size is a different measurement from guest
filesystem allocation.

Do not infer that a one-line edit allocates only that line's bytes. Report
allocation growth for the specific write pattern and filesystem. Read/hash
verification makes creation cost depend on total data and file count.

## Cleanup and failure evidence

The harness never automatically deletes its fixture, worktrees, branches or
reports, including after interruption or failure. On a command failure it writes
an incomplete report with the error when possible. Review that evidence before
rerunning. A forced process kill or machine failure can prevent the final report
from being written; the remaining directories are still retained.

The edit phase intentionally leaves dirty worktrees. Commit those synthetic
edits if using `git thin remove`, which refuses to discard work. Removal preserves
branches and commits. For a disposable dedicated disk image, detach it and
delete only the image you created after retaining the reports. Do not point a
generic recursive deletion command at a shared volume. Failed thin creations
can be inspected with `git thin recover --dry-run` before recovery.

The small smoke test, real APFS integration tests, failure tests and 8 GB storage
measurements are different forms of evidence. Publish which were actually run;
do not mark the first release's measured-savings requirement complete from
simulation or unexecuted instructions.
