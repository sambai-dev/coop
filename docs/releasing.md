# Releasing

A source checkout does not authorize a tag or publication. The release workflow is intentionally fail closed until repository governance is configured and the changelog is finalized.

## One-time repository controls

Configure these controls in GitHub before setting the repository variable `ROOKHOLD_RELEASE_GOVERNANCE=enabled`:

1. Protect `main` with a ruleset that requires changes through a pull request, blocks force pushes/deletion, and requires every CI job, including the container and hostile-containment gates. Require an approving review when the repository has a second trusted maintainer; a solo maintainer must still use a PR and may bypass only through that PR after all required checks pass.
2. Protect `v*` tags with a ruleset that restricts tag creation, update, and deletion to designated release maintainers or automation. Do not permit a tag to be moved after publication.
3. Create a `release` environment restricted to `v*` tags and require explicit reviewer approval. Use an independent trusted reviewer and prevent self-review whenever a second trusted maintainer is available. A solo-maintainer repository may temporarily designate its owner, must record the explicit approval, and should switch to independent approval as soon as another maintainer is established.
4. Enable private vulnerability reporting and verify the contact in [SECURITY.md](https://github.com/sambai-dev/rookhold/blob/main/SECURITY.md).
5. Enable GitHub immutable releases so a published tag or asset cannot be silently replaced. The workflow's draft-staging step remains editable until publication; immutability applies to the published release.
6. Review Actions access so only required, SHA-pinned actions are allowed. Keep the default workflow token read-only; only the final publish job receives contents, attestation, and OIDC write permissions.
7. On PyPI, configure a pending trusted publisher for project `rookhold`, owner
   `sambai-dev`, repository `rookhold`, workflow `release.yml`, and environment
   `release`. A pending publisher does not reserve the name until the first
   publish, so complete the release promptly after this check.
8. npm does not offer PyPI-style pending publishers. Before the final tag,
   bootstrap the unclaimed `rookhold` package through the maintainer's
   2FA-protected npm account (a prerelease is preferred), then configure its
   GitHub Actions trusted publisher for owner `sambai-dev`, repository
   `rookhold`, workflow `release.yml`, environment `release`, and `npm publish`.
   Confirm the package's repository URL resolves exactly to this repository.
   The final `0.8.0` publication must still come from the tagged OIDC workflow.

The repository variables are acknowledgements, not proof. Recheck the settings
after ownership, plan, or organization-policy changes. Set
`ROOKHOLD_RELEASE_GOVERNANCE`, `ROOKHOLD_PYPI_TRUSTED_PUBLISHER`, and
`ROOKHOLD_NPM_TRUSTED_PUBLISHER` to `enabled` only after the corresponding
checks. An unset variable stops the tag workflow before any build or draft
release is created.

## Per-release checklist

1. Start from a reviewed commit on protected `main` with a clean lockfile.
2. Confirm all CI jobs pass, including locked Rust/SDK tests, RustSec, the final-image language canaries, and the non-skipping hostile suite on Linux x86_64.
3. Review dependency, base-image, Debian-snapshot, rootfs, toolchain, and GitHub Action changes. Re-verify every external action SHA against its upstream release.
4. Run `python scripts/check-release-surface.py` and the verification commands in [README.md](https://github.com/sambai-dev/rookhold/blob/main/README.md).
5. Replace `unreleased` in the matching [CHANGELOG.md](https://github.com/sambai-dev/rookhold/blob/main/CHANGELOG.md) heading with the UTC release date. The tag workflow rejects an unfinalized heading.
6. Make the [README](https://github.com/sambai-dev/rookhold/blob/main/README.md) current-release link match `vVERSION` and mark the matching `MAJOR.MINOR.x` line as supported in [SECURITY.md](https://github.com/sambai-dev/rookhold/blob/main/SECURITY.md). The tag workflow checks both dynamically.
7. Confirm the workspace, lockfile, SDK, Docker, Compose, changelog, and tag versions all match exactly.
8. Re-read [AUDIT.md](https://github.com/sambai-dev/rookhold/blob/main/AUDIT.md), the [security boundary](security-boundary.md), and residual risks. Stop if the code enforces a weaker posture than the documents.
9. Reproduce the schema-v3-to-v4 migration, signed restart backfill, offline
   `rookhold-verify` vectors, and real pinned-runsc lifecycle without skips. Confirm
   the archive and image contain the matching `rookhold-verify`/init binaries.
10. Create and push the protected `vVERSION` tag from the reviewed commit. The tag push starts the release workflow; do not move or reuse the tag.
11. When the publish job reaches the protected `release` environment, obtain its required approval only after reviewing the completed gates and intended eleven-asset set.

No release command is run by this documentation. Do not reuse or move a published tag to correct a mistake; increment the version and publish a superseding release. The workflow verifies that the remote tag exists and that the checkout matches the event commit; it does **not** cryptographically verify a signed Git tag. Tag trust currently comes from protected refs, release-environment approval, immutable releases, and workflow OIDC attestations.

## Automated publication contract

For a matching, finalized tag the workflow:

1. re-runs formatting, Clippy, locked tests, RustSec, SDK packaging, Docker canaries, and hostile containment;
2. builds only the declared platform artifacts, packages `rookhold-verify` on every platform, and packages both execution init helpers with Linux;
3. downloads only four exactly named workflow artifacts, then requires exactly three platform archives and three SDK packages;
4. safely extracts and inventories those nine payloads into a fresh staging tree and produces one combined, artifact-scoped SPDX JSON SBOM;
5. binds the SPDX predicate to all nine payload digests, writes `SHA256SUMS` for the other ten public assets, and records SLSA provenance for the exact eleven-asset set through OIDC;
6. uploads only those eight names to a draft release and reconciles every remote name, size, and SHA-256 digest against the local files; and
7. publishes that draft only after every upload, attestation, and readback succeeds and the protected `release` environment approves the job.

The public contract is exactly eleven assets: three complete platform archives, three direct single-file CLI downloads, the Python wheel and source distribution, the npm tarball, the combined SPDX JSON document, and `SHA256SUMS`. Every platform archive contains the native service and verifier plus standalone `rookhold-cli` and `rookhold-mcp` apps that do not require Python. The checksum manifest covers the other ten assets and cannot include its own digest; its release attestation and constrained workflow provenance authenticate the manifest itself. GitHub-generated source archives are outside this asset set.

If publication fails, keep the release as a draft while investigating. Because draft uploads do not overwrite existing names, remove a failed draft only after preserving the evidence and before an intentional clean rerun. Verify asset hashes, attestations, archive contents, release notes, and fresh-install canaries before announcing it. After publication, monitor installation reports and preserve the workflow run, artifact/SBOM attestations, dependency inputs, and release approval as the repository release record. The workflow builds and gates an ephemeral container/private rootfs but does not publish that image or its rootfs manifest; each deployment must preserve its separately generated rootfs manifest, digest, package inventory, and verification output with the deployment record.

## Compromised or faulty release

Do not overwrite assets or move the tag. Mark the affected release clearly, rotate exposed credentials or signing authority, preserve evidence, and publish a new patched version. If a security boundary is affected, update `SECURITY.md`, notify known operators through the project's established channel, and provide explicit upgrade or containment instructions.
