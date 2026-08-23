# Contributing to Coop

Thanks for helping make agent code execution safer and easier to audit.

## Before you start

- Use [GitHub Issues](https://github.com/sambai-dev/coop/issues) for reproducible bugs and scoped feature requests.
- For security vulnerabilities, follow [SECURITY.md](SECURITY.md) instead of opening a public issue.
- For large changes, open an issue first so the approach and containment tradeoffs can be agreed before implementation.

Good contribution areas include:

- sandbox hardening and hostile-job coverage
- SDK ergonomics and examples
- observability and replay tooling
- documentation for real deployment environments
- narrowly scoped roadmap items from the README

## Local development

Install a current stable Rust toolchain, then run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

On macOS or Windows, Coop uses the development subprocess backend. Start the server explicitly in that mode with:

```bash
COOP_SANDBOX=off COOP_API_KEYS="local:dev-key" cargo run -p coop-server
```

The real containment suite requires Linux, cgroup v2, namespaces, and elevated privileges:

```bash
sudo cargo test -p coop-server --test hostile -- --ignored --nocapture
```

Do not run the hostile-job suite on a workstation that contains important unsaved work. A dedicated Linux VM is the preferred environment.

## Pull requests

Keep each pull request focused and include:

1. the problem and threat or operator impact
2. the design choice and important tradeoffs
3. tests or other evidence that prove the change
4. any deployment, compatibility, or security implications

Before requesting review, run the checks above and `git diff --check`. Documentation-only changes only require the checks relevant to the files changed.

If a commit was genuinely produced by more than one person, attribute each contributor with GitHub's standard `Co-authored-by:` trailer. Do not add attribution for review, tooling, or work the named person did not perform.

## Design principles

- Fail closed when a requested safety boundary cannot be established.
- Report the active isolation mode honestly; never present a subprocess fallback as a sandbox.
- Keep tenant boundaries explicit in storage, routing, and logs.
- Preserve replayability: every execution outcome should be explainable from persisted events.
- Prefer small, auditable mechanisms over broad abstractions in security-sensitive paths.

