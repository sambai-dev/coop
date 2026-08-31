from rookhold import Limits, Rookhold

result = Rookhold.from_env().run(
    "python",
    "while True: pass",
    limits=Limits(wall_seconds=2, cpu_seconds=2),
)

print(f"status={result.status}")
print(f"duration={result.duration}s")
print(f"receipt={result.receipt is not None}")
