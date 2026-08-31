# Python

```python
from rookhold import Rookhold

result = Rookhold.from_env().run("python", "print(sum(range(10)))")
print(result.raise_for_status().stdout)
```

For trusted local code when the server executable is installed:

```python
from rookhold import Rookhold

with Rookhold.local() as rookhold:
    print(rookhold.run("python", "print(6 * 7)").stdout)
```

See the [Python SDK reference](../sdks.md#python).
