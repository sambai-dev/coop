# Installation

The `rookhold` registry packages are prepared for v0.8.0 but are not public
yet. Until the release is tagged, install from a checkout.

## Python from a checkout

```bash
python -m pip install ./sdks/python
```

## TypeScript from a checkout

```bash
cd sdks/typescript
npm ci
npm pack
```

After v0.8.0 publishes, use `pip install rookhold` or `npm install rookhold`.

## Command and service

The unified v0.8 archive is not public yet. The current stable archive is the
[v0.7.1 release](https://github.com/sambai-dev/rookhold/releases/tag/v0.7.1).
The archive contains `rookhold`, the MCP adapter, and the offline verifier.
Verify `SHA256SUMS` and GitHub provenance before allowing an unsigned community
binary to run.
