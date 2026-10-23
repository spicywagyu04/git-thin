# Release preparation

The repository includes a tag-triggered packaging workflow. This is release infrastructure, not evidence that a release has been published or that every platform has been validated.

Before the first publication:

1. Check current GitHub repository names, executable names, and package registries for conflicts with the working name `git-thin`. Select and document an actual owner and release destination. Do not invent an installation URL.
2. Run the complete suite on a real APFS filesystem and on a supported Linux reflink filesystem. Record any unavailable destructive or capacity-constrained scenarios as unverified.
3. Run the controlled 8 GB benchmark for both large-file and many-small-file fixtures. Publish measured results and their limits, including logical size, physical allocation deltas, creation time, and content-verification time.
4. Review the supported configurations and scope policy. Confirm the release does not promise strict write enforcement.
5. Update the Cargo package version, commit the lockfile, and verify the intended version with `cargo metadata --no-deps --format-version 1`.
6. Tag the reviewed commit with `v` followed by its package version. Pushing that tag to a configured GitHub repository triggers the release workflow.

The workflow builds `git-thin` for macOS Apple silicon, macOS Intel, and Linux x86-64, packages the executable with the license and README, and produces a `SHA256SUMS` manifest alongside versioned archives. It creates a draft GitHub release for review. Publishing the draft is a separate maintainer action.

After downloading a real release's archives and checksum manifest, verify them with `shasum -a 256 -c SHA256SUMS` on macOS or `sha256sum -c SHA256SUMS` on Linux. Only then unpack the matching archive and place its binary on `PATH`. Checksums detect accidental corruption; release provenance still depends on the source and distribution channel.

Release binaries are not currently signed or notarized. The workflow is a repeatable build recipe, not a claim of byte-for-byte reproducible builds or a supply-chain attestation.
