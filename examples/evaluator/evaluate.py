import json

from rookhold import Rookhold

candidate = """
def solve(value):
    return value * value
"""
hidden_cases = [{"input": 2, "want": 4}, {"input": -3, "want": 9}]
runner = candidate + """
import json, sys
cases = json.load(sys.stdin)
checks = [{"ok": solve(case["input"]) == case["want"]} for case in cases]
print(json.dumps({"passed": sum(check["ok"] for check in checks), "checks": checks}))
"""

result = Rookhold.from_env().run_json("python", runner, input=hidden_cases)
print(json.dumps(result.json_value, indent=2))
