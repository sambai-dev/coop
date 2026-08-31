"""Treat `generated_code` as the small function returned by a model."""

from rookhold import Limits, Rookhold

generated_code = """
def answer(values):
    return sum(value * value for value in values)

print(answer([2, 3, 4]))
"""

result = Rookhold.from_env().run(
    "python",
    generated_code,
    limits=Limits(wall_seconds=2, mem_mb=128),
    requirements={"minimum_isolation": "gvisor-application-kernel"},
)
print(result.raise_for_status().stdout)
