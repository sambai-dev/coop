# Deterministic v1 vector

This fixture freezes the cross-implementation wire contract. It uses the
test-only Ed25519 seed:

```text
000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
```

The key is public test material and must never be used outside tests.

`subject.json` is the exact result artifact, including its final LF. The JSON
fixture files are repository-friendly and have a final LF; `statement.json`
and `envelope.json` represent wire bytes with that one fixture LF removed.
The test regenerates both byte sequences and verifies the envelope against the
public key and exact subject digest.
