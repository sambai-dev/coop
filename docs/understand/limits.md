# Limits

The request may ask for wall time, CPU time, memory, process count, file size,
and output ceilings. The server clamps every value. The receipt distinguishes:

- what the caller requested;
- what the server allowed;
- what the executor actually enforced; and
- what could not be observed because a workload never became ready.

Input and output artifacts also have file-count, per-file, total-byte, and safe
path limits. Inputs live under `input/`; returned files must be named under
`output/`. Absolute paths, traversal, backslashes, and symlinks are rejected.
