# Threat model

Rookhold assumes job code can be malicious. The strong deployment keeps the
service credential outside the model, denies network inside each workload,
uses a private root filesystem, applies cgroup and rlimit ceilings, removes
capabilities, bounds output, and records the controls that became effective.

The outer service remains privileged and belongs on a dedicated VM. Read the
complete [security boundary](../security-boundary.md) and [SECURITY.md](https://github.com/sambai-dev/rookhold/blob/main/SECURITY.md).
