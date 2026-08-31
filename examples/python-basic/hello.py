from rookhold import Rookhold

result = Rookhold.from_env().run("python", "print(6 * 7)")
print(result.stdout)
print(f"job={result.job_id} isolation={result.isolation}")
