# Isolation levels

Rookhold reports the boundary it observed, not the one an operator hoped to
configure. Process-style boundaries form a chain from `none` through Linux
shared-kernel, gVisor, hardware VM, and confidential VM. Wasm capability
isolation is a separate branch.

`none` means the subprocess shares the host's trust boundary and network. It
is useful for demonstrating the API with trusted code and nothing more.
