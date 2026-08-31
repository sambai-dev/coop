import json
from pathlib import Path

from rookhold import Rookhold

client = Rookhold.from_env()
result = client.run("python", "print('bounded')")
path = Path(f"receipt-{result.job_id}.json")
path.write_text(json.dumps(result.receipt, indent=2), encoding="utf-8")
print(f"saved {path}")
print("For signed evidence, download the attestation and verify it with rookhold-verify.")
