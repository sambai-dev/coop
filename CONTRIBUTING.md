# Contributing to Rookhold

Thanks for helping make agent code execution safer and easier to audit.

## Before you start

- Use [GitHub Issues](https://github.com/sambai-dev/rookhold/issues) for reproducible bugs and scoped feature requests.
- For security vulnerabilities, follow [SECURITY.md](SECURITY.md) instead of opening a public issue.
- For large changes, open an issue first so the approach and containment tradeoffs can be agreed before implementation.

Good contribution areas include:

- sandbox hardening and hostile-job coverage
- SDK ergonomics and examples
- observability and replay tooling
- documentation for real deployment environments
- narrowly scoped items from the README's project direction

## Local development

Install Rust with `rustup`; the repository's `rust-toolchain.toml` selects the
supported toolchain. Then run the same workspace checks as CI:

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
```

On macOS or Windows, Rookhold uses the development subprocess backend. Start the
server explicitly in that mode. For macOS or another POSIX shell:

```bash
ROOKHOLD_SANDBOX=off ROOKHOLD_API_KEYS="local:dev-key" cargo run -p coop-server
```

For PowerShell:

```powershell
$env:ROOKHOLD_SANDBOX = "off"
$env:ROOKHOLD_API_KEYS = "local:dev-key"
cargo run -p coop-server
```

The real containment suite requires a supported Linux x86_64 kernel, cgroup v2,
namespaces, the matching sandbox helper, a trusted private rootfs, and elevated
privileges. Follow the complete setup and command in the
[README](README.md#verification). Do not run the hostile-job suite on a workstation
that contains important unsaved work. A dedicated Linux VM is the preferred
environment.

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
