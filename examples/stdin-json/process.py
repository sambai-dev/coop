from rookhold import Rookhold

code = """
import json, sys
numbers = json.load(sys.stdin)["numbers"]
print(json.dumps({"count": len(numbers), "total": sum(numbers)}))
"""

result = Rookhold.from_env().run_json(
    "python", code, input={"numbers": [3, 5, 8, 13]}
)
print(result.json_value)
