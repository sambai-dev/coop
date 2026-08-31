from rookhold import Rookhold

code = """
import json, sys
payload = json.load(sys.stdin)
print(json.dumps({"names": [row["name"].strip().title() for row in payload["rows"]]}))
"""

result = Rookhold.from_env().run_json(
    "python",
    code,
    input={"rows": [{"name": " ada "}, {"name": "grace"}]},
)
print(result.json_value)
