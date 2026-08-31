# Receipts

Every terminal job has a receipt. It records the outcome, duration, requested
and effective limits, actual isolation, network state, runtime and rootfs
digests, output hashes, event-chain head, and input/output artifact hashes.

A receipt is the service's durable execution record. A signed DSSE envelope
adds proof that a configured Rookhold key asserted the receipt and exact result
artifact. It does not prove trusted hardware, deterministic re-execution, or
independent key distribution.

```bash
rookhold verify \
  --envelope job.dsse.json \
  --subject job-result.json \
  --public-key rookhold-attestation.pub.pem \
  --tenant acme
```
