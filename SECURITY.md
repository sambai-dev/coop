# Security Policy

## Reporting a vulnerability

Open a **private GitHub security advisory** (Security → Report a vulnerability) on this repository. Please do not open public issues for suspected vulnerabilities.

We aim to acknowledge reports within 72 hours and will credit reporters in the changelog unless anonymity is requested.

## Supported versions

| Version | Supported |
|---|---|
| latest `main` | yes |
| tagged releases (`v0.x`) | yes |
| older tags | best effort |

## Security model, in one page

Coop runs untrusted code inside layered Linux isolation:

- namespaces: mount (read-only bind of `/`, fresh tmpfs `/tmp`, private), PID, network (no interfaces by default), IPC, UTS
- cgroup v2: `memory.max`, `memory.swap.max=0`, `cpu.max`, `pids.max`; attach is fail-closed
- rlimits: `CPU`, `AS`, `NPROC`, `NOFILE`, `FSIZE`
- privilege drop to `nobody` when run as root; fresh `/proc`; minimal environment

**Defends against**: fork bombs, memory exhaustion, CPU hogs, disk fill, network access, host filesystem tampering/reads, wall-clock runaway jobs.

**Does not defend against**: kernel exploits, side channels, malicious interpreter CVEs. Run Coop on a dedicated VM; see the threat-model tables in `README.md` and the full audit in `AUDIT.md`.

## Operational requirements

1. Set real keys: `COOP_API_KEYS="tenant:key,…"` — never ship the dev default.
2. One key per agent/tenant for blast-radius isolation.
3. Keep `COOP_DB` and `COOP_JOBS_ROOT` on persistent storage you control.
4. Firewall host egress; sandboxed jobs already have none.
5. Monitor `/healthz` — it truthfully reports `"sandbox": "off"` when kernel isolation is unavailable.
