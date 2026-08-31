# First secure Linux deployment

Rookhold's strong boundary requires a dedicated Linux x86_64 VM, cgroup v2,
the pinned gVisor runtime, a private root filesystem, and a scoped credential.

```bash
ROOKHOLD_PRODUCTION_VM_ACKNOWLEDGED=true scripts/bootstrap-production.sh
```

The acknowledgement does not make a general-purpose Docker host safe. The
outer service is privileged so it can create one isolated gVisor workload per
job. Follow the complete [deployment guide](../deployment.md) and do not accept
untrusted work until every production check passes.
