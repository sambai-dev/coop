# Releasing

A source checkout does not authorize a tag or publication. The release workflow is intentionally fail closed until repository governance is configured and the changelog is finalized.

## One-time repository controls

Configure these controls in GitHub before setting the repository variable `COOP_RELEASE_GOVERNANCE=enabled`:

1. Protect `main` with a ruleset that requires changes through a pull request, blocks force pushes/deletion, and requires every CI job, including the container and hostile-containment gates. Require an approving review when the repository has a second trusted maintainer; a solo maintainer must still use a PR and may bypass only through that PR after all required checks pass.
2. Protect `v*` tags with a ruleset that restricts tag creation, update, and deletion to designated release maintainers or automation. Do not permit a tag to be moved after publication.
3. Create a `release` environment restricted to `v*` tags and require explicit reviewer approval. Use an independent trusted reviewer and prevent self-review whenever a second trusted maintainer is available. A solo-maintainer repository may temporarily designate its owner, must record the explicit approval, and should switch to independent approval as soon as another maintainer is established.
4. Enable private vulnerability reporting and verify the contact in [SECURITY.md](../SECURITY.md).
5. Enable GitHub immutable releases so a published tag or asset cannot be silently replaced. The workflow's draft-staging step remains editable until publication; immutability applies to the published release.
6. Review Actions access so only required, SHA-pinned actions are allowed. Keep the default workflow token read-only; only the final publish job receives contents, attestation, and OIDC write permissions.

The repository variable is an acknowledgement, not proof. Recheck the settings after ownership, plan, or organization-policy changes. An unset variable stops the tag workflow before any build or draft release is created.

## Per-release checklist

1. Start from a reviewed commit on protected `main` with a clean lockfile.
2. Confirm all CI jobs pass, including locked Rust/SDK tests, RustSec, the final-image language canaries, and the non-skipping hostile suite on Linux x86_64.
3. Review dependency, base-image, Debian-snapshot, rootfs, toolchain, and GitHub Action changes. Re-verify every external action SHA against its upstream release.
4. Run `python scripts/check-release-surface.py` and the verification commands in [README.md](../README.md).
5. Replace `unreleased` in the matching [CHANGELOG.md](../CHANGELOG.md) heading with the UTC release date. The tag workflow rejects an unfinalized heading.
6. Make the [README](../README.md) current-release link match `vVERSION` and mark the matching `MAJOR.MINOR.x` line as supported in [SECURITY.md](../SECURITY.md). The tag workflow checks both dynamically.
7. Confirm the workspace, lockfile, SDK, Docker, Compose, changelog, and tag versions all match exactly.
8. Re-read [AUDIT.md](../AUDIT.md), the [security boundary](security-boundary.md), and residual risks. Stop if the code enforces a weaker posture than the documents.
9. Obtain the release approval required by the protected environment, then create and push an immutable `vVERSION` tag from the reviewed commit according to the project's signing policy.

No release command is run by this documentation. Do not reuse or move a published tag to correct a mistake; increment the version and publish a superseding release.

## Automated publication contract

For a matching, finalized tag the workflow:

1. re-runs formatting, Clippy, locked tests, RustSec, SDK packaging, Docker canaries, and hostile containment;
2. builds only the declared platform artifacts and packages the Linux helper with the Linux server;
3. merges platform archives and SDK packages into one artifact set;
4. produces an SPDX JSON SBOM and SHA-256 manifest covering every asset;
5. records GitHub artifact attestations through OIDC;
6. uploads the complete set to a draft release; and
7. publishes that draft only after every upload and attestation succeeds and the protected `release` environment approves the job.

If publication fails, keep the release as a draft while investigating. Verify asset hashes, attestations, archive contents, release notes, and fresh-install canaries before announcing it. After publication, monitor installation reports and preserve the workflow run, artifact attestations, dependency evidence, rootfs manifest, and release approval as the release record.

## Compromised or faulty release

Do not overwrite assets or move the tag. Mark the affected release clearly, rotate exposed credentials or signing authority, preserve evidence, and publish a new patched version. If a security boundary is affected, update `SECURITY.md`, notify known operators through the project's established channel, and provide explicit upgrade or containment instructions.
