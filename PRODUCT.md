# Product

<!-- impeccable:product-schema 1 -->

## Platform

web, terminal

## Users

Rookhold is primarily for operators and platform engineers who need to run short agent-generated or user-supplied programs behind a separately operated policy boundary. SDK and MCP integrators are a secondary audience; they need the execution contract to stay predictable without exposing credentials or policy controls to a model.

## Product Purpose

Rookhold authenticates a caller, clamps a short Python, Node.js, or Bash job to configured policy, runs it through the selected execution provider, bounds its output, and records durable evidence of what happened. Success means an operator can quickly submit or inspect a run and answer: what ran, who submitted it, which controls became effective, what output or violations were observed, and how the run ended.

## Positioning

Rookhold is a small self-hosted execution control plane, not an LLM, agent framework, general-purpose IDE, or replacement for a harness workspace. Its differentiator is the combination of an independently operated API boundary, per-job execution policy, bounded live output, durable receipts, and portable signed evidence.

## Operating Context

- Trusted applications, agent harnesses, and MCP adapters submit short stateless jobs over HTTP.
- Operators use `rookhold-cli` or the embedded dashboard to connect a tenant credential, create a run, monitor its ordered event stream, cancel eligible work, and inspect result, policy, receipt, and attestation artifacts.
- Claude Code, OpenCode, Codex, Hermes, OpenClaw, and other MCP hosts launch the same `rookhold-mcp` stdio adapter; the host owns the conversation UI while Rookhold owns execution policy and evidence.
- Local development may use the explicitly unisolated subprocess provider. The guarded Linux x86_64 deployment uses one pinned gVisor workload per job on a dedicated VM.
- The API and persisted store remain the source of truth; the terminal client, MCP adapter, and dashboard are views over those contracts.

## Capabilities and Constraints

- Preserve the existing submit, list, filter, inspect, cancel, live-output, event, result, record, exact artifact download, and copy workflows.
- Preserve scoped identity and tenant context, the six-class isolation contract, requested-versus-effective policy, output truncation evidence, receipt integrity metadata, and explicit trust warnings.
- The dashboard is a single dependency-free HTML/CSS/JavaScript document embedded in the Rust server and protected by exact Content Security Policy hashes.
- The Python package's `rookhold-cli` and `rookhold-mcp` commands use only the standard library and share the typed client contract.
- No dashboard state may become a competing source of truth or imply that server-provided evidence was independently verified.
- The operator surface must work with narrow screens, keyboard navigation, zoom/reflow, reduced motion, and high-contrast preferences.

## Brand Commitments

Keep the name Rookhold and its plainspoken, technically honest voice. Security posture must be legible without theatrics: never imply stronger isolation, attestation, or verification than the server actually reports. The replacement interface should feel like a current production control plane rather than a terminal-themed demo or a collection of decorative cards.

## Evidence on Hand

- Product and operating contract: `README.md`, `SECURITY.md`, and `docs/`.
- Embedded operator surface and behavior: `crates/coop-server/src/dashboard.html`.
- API, CSP, and dashboard contract tests: `crates/coop-server/src/routes.rs`.
- Canonical visual system and responsive rules: `DESIGN.md`.
- Current release captures: `docs/assets/console-v0.6.png` and `docs/assets/rookhold-cli-mcp-demo.gif`.
- No customer logos, testimonials, usage claims, or external brand assets are available and none may be fabricated.

## Product Principles

1. Show operational state before decoration.
2. Keep requested policy, observed posture, and independently verifiable evidence distinct.
3. Let one workflow move naturally from submit to monitor to prove.
4. Make dense technical information scannable without hiding detail.
5. Fail closed in behavior and speak plainly about uncertainty in the interface.

## Accessibility & Inclusion

Preserve semantic landmarks, labels, focus order, visible focus, keyboard operation, status announcements, responsive reflow, zoom resilience, reduced-motion handling, and contrast-safe status communication. Visual status must never rely on color alone.
