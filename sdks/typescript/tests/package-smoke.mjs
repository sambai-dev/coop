import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const scratch = await mkdtemp(path.join(tmpdir(), "coop-sdk-pack-smoke-"));

function npm(args, cwd) {
  const npmCli = process.env.npm_execpath;
  const command = npmCli ? process.execPath : process.platform === "win32" ? "npm.cmd" : "npm";
  const commandArgs = npmCli ? [npmCli, ...args] : args;
  const result = spawnSync(command, commandArgs, {
    cwd,
    encoding: "utf8",
    env: process.env,
  });
  assert.equal(
    result.status,
    0,
    `npm ${args.join(" ")} failed\n${result.stdout}\n${result.stderr}`,
  );
}

try {
  npm(["pack", "--pack-destination", scratch, "--ignore-scripts=false"], packageDir);
  const tarballs = (await readdir(scratch)).filter((name) => name.endsWith(".tgz"));
  assert.equal(tarballs.length, 1, "npm pack must produce exactly one tarball");

  await writeFile(
    path.join(scratch, "package.json"),
    JSON.stringify({ private: true, type: "module" }),
  );
  await writeFile(
    path.join(scratch, "smoke.mjs"),
    'import { Coop, CoopError, ISOLATION_CLASSES, JOB_STATUSES, isolationSatisfies } from "coop-sdk";\n' +
      'if (typeof Coop !== "function" || typeof CoopError !== "function" || !Array.isArray(JOB_STATUSES) || !Array.isArray(ISOLATION_CLASSES) || !isolationSatisfies("gvisor-application-kernel", "linux-shared-kernel")) process.exit(1);\n',
  );
  // Install with lifecycle scripts disabled so this verifies the contents of
  // the tarball rather than rebuilding missing files in the consumer.
  npm(
    [
      "install",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--no-package-lock",
      "--prefix",
      scratch,
      path.join(scratch, tarballs[0]),
    ],
    scratch,
  );
  const runtime = spawnSync(process.execPath, [path.join(scratch, "smoke.mjs")], {
    cwd: scratch,
    encoding: "utf8",
  });
  assert.equal(runtime.status, 0, `packed SDK import failed\n${runtime.stdout}\n${runtime.stderr}`);
} finally {
  await rm(scratch, { recursive: true, force: true });
}
