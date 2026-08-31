# Contributing to Rookhold

Thanks for helping make agent code execution safer and easier to audit.

## Before you start

- Use [GitHub Issues](https://github.com/sambai-dev/rookhold/issues) for reproducible bugs and scoped feature requests.
- For security vulnerabilities, follow [SECURITY.md](SECURITY.md) instead of opening a public issue.
- For large changes, open an issue first so the approach, declared scope, and containment tradeoffs can be agreed before implementation.
- Read the [contribution lifecycle](docs/contribution-lifecycle.md) before calling a change complete.

Good contribution areas include sandbox hardening, hostile-job coverage, SDK ergonomics, observability, replay tooling, deployment documentation, and narrowly scoped items from the README's project direction.

## Local development

Install Rust with `rustup`; the repository's `rust-toolchain.toml` selects the supported toolchain. Run the same workspace checks as CI:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
```

On macOS or Windows, Rookhold uses the development subprocess backend. Start the server explicitly in that mode. For macOS or another POSIX shell:

```bash
ROOKHOLD_SANDBOX=off ROOKHOLD_API_KEYS="local:dev-key" cargo run -p coop-server
```

For PowerShell:

```powershell
$env:ROOKHOLD_SANDBOX = "off"
$env:ROOKHOLD_API_KEYS = "local:dev-key"
cargo run -p coop-server
```

The real containment suite requires a supported Linux x86_64 kernel, cgroup v2, namespaces, the matching sandbox helper, a trusted private rootfs, and elevated privileges. Follow the complete setup and command in the [README](README.md#verification). Do not run the hostile-job suite on a workstation that contains important unsaved work. A dedicated Linux VM is preferred.

## Definition of technically done

A mergeable branch or green focused test is not sufficient. Technical completion requires all applicable conditions below.

1. **Declared behavior is complete.** Every promise maps to implementation, regression coverage, and direct evidence. A deliberately partial solution names its independently useful boundary and records remaining work.
2. **The regression proves RED.** The new test fails on unchanged source for the reported reason, then passes after the fix. Record the command and failure; do not weaken assertions or rely on a mock artifact.
3. **Adversarial cases are considered.** Check missing/null-like and annotated inputs, alternate valid layouts, zero/partial/all-state boundaries, retry/fallback paths, sibling call sites, classifier fallthrough, and platform behavior where relevant.
4. **The root invariant is repaired.** Do not mask symptoms with broad exception handling, silent defaults, premature skipping, optional state suppression, disabled tests, or weaker assertions.
5. **The final exact diff is rescanned.** Re-read the declared scope, issue, review comments, final diff, affected sibling paths, and evidence after the last code change.
6. **Exact-head integration evidence exists.** Run focused tests, the affected suite, lint/type/format checks, required full gates, and platform verification. CI must be attached to the current head; fork-only evidence is described as locally validated with trusted integration pending.

## Pull requests

The pull request template is enforced in CI. Complete every section with concrete evidence:

- declared scope and non-goals;
- the root invariant;
- reproduction and RED evidence, or a reason it does not apply;
- adversarial coverage;
- exact final-head SHA and checks;
- safety, compatibility, and rollback implications; and
- separate implementation, validation, review, and integration states.

Before requesting review, run the applicable checks above plus `git diff --check`. Documentation-only changes run the checks relevant to their files, but must still declare scope and exact-head evidence.

If a commit was genuinely produced by more than one person, use GitHub's standard `Co-authored-by:` trailer. Do not add attribution for review, tooling, or work the named person did not perform.

## Status language

Use these terms precisely:

- **Implementation incomplete:** valid cases within the declared promise remain mishandled.
- **Validation incomplete:** implementation may be correct, but exact-head or trusted-platform evidence is missing.
- **Review incomplete:** implementation and validation are complete, but approval is pending or changes are requested.
- **Integration incomplete:** the contribution is ready but has not entered the protected merge/release path.
- **Scoped complete:** one explicit, independently useful part of a broader issue is complete; remaining work is recorded and not overclaimed.
- **Stale/unverified:** the base, relevance, merge result, or current CI is no longer established; reproduce and rebuild before presenting it as ready.

Do not update an approved, green PR merely because it is behind the base when intervening commits do not overlap or affect its invariant. Conversely, matching file contents do not prove correct ancestry: simulate the merge or rebase on the intended base when history changes Git's integration result.

## Design principles

- Fail closed when a requested safety boundary cannot be established.
- Report the active isolation mode honestly; never present a subprocess fallback as a sandbox.
- Keep tenant boundaries explicit in storage, routing, and logs.
- Preserve replayability: every execution outcome should be explainable from persisted events.
- Prefer small, auditable mechanisms over broad abstractions in security-sensitive paths.
