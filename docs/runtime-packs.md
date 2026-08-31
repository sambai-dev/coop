# Runtime packs

Rookhold exposes a deliberately small set of versioned packs for isolated
Linux execution:

- `python:bookworm-20260826-stdlib`
- `node:bookworm-20260826-base`
- `bash:bookworm-20260826-core`

The pack name is not the proof. The receipt also records the interpreter,
rootfs, runtime, and OCI configuration digests. Jobs cannot install arbitrary
packages. New packs require pinned contents, a support window, and containment
tests.
