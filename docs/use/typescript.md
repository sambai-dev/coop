# TypeScript

```ts
import { Rookhold } from "rookhold";

const result = await Rookhold.fromEnv().run({
  language: "python",
  code: "print(6 * 7)",
});

console.log(result.raiseForStatus().stdout);
```

Use `submit`, `stream`, `result`, and `cancel` when an application needs
separate lifecycle control. See the [full SDK reference](../sdks.md#typescript).
