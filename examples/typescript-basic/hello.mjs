import { Rookhold } from "rookhold";

const result = await Rookhold.fromEnv().run({
  language: "python",
  code: "print(6 * 7)",
});

console.log(result.stdout);
console.log(`job=${result.jobId} isolation=${result.isolation}`);
