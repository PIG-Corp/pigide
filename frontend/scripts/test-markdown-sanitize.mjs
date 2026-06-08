// Compile-then-run the markdownSanitize tests using ONLY tools the project
// already ships (`tsc` + `node --test`). Zero new deps. markdownSanitize.ts
// is pure (no DOM/React imports), so unlike the pathMentionHelpers runner we
// don't need to rewrite any imports — just rewrite the test's relative import
// to an emitted `.js` path and compile both files together.

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const out = mkdtempSync(join(tmpdir(), "pigide-md-sanitize-test-"));

try {
  const srcTs = readFileSync(join(root, "src/components/markdownSanitize.ts"), "utf8");
  const testTs = readFileSync(join(root, "scripts/markdownSanitize.test.ts"), "utf8")
    .replace(
      /from "\.\.\/src\/components\/markdownSanitize\.js"/g,
      'from "./markdownSanitize.js"',
    );
  mkdirSync(join(out, "src"), { recursive: true });
  writeFileSync(join(out, "src/markdownSanitize.ts"), srcTs);
  writeFileSync(join(out, "src/markdownSanitize.test.ts"), testTs);
  if (process.env.HELPER_TEST_DEBUG) {
    console.error("temp dir:", out);
  }
  writeFileSync(
    join(out, "tsconfig.json"),
    JSON.stringify(
      {
        compilerOptions: {
          target: "es2023",
          module: "node16",
          moduleResolution: "node16",
          esModuleInterop: true,
          strict: true,
          skipLibCheck: true,
          typeRoots: [join(root, "node_modules/@types")],
          types: ["node"],
          outDir: "./dist",
          rootDir: "./src",
        },
        include: ["src"],
      },
      null,
      2,
    ),
  );
  writeFileSync(
    join(out, "package.json"),
    JSON.stringify({ name: "md-sanitize-tests", type: "module", private: true }),
  );

  const tsc = join(root, "node_modules/typescript/bin/tsc");
  const tscRun = spawnSync("node", [tsc, "-p", join(out, "tsconfig.json")], {
    stdio: "inherit",
    env: { ...process.env, NODE_PATH: join(root, "node_modules") },
  });
  if (tscRun.status !== 0) {
    console.error("tsc failed (status=" + tscRun.status + ")");
    process.exit(tscRun.status ?? 1);
  }

  const compiledTest = join(out, "dist/markdownSanitize.test.js");
  const testRun = spawnSync("node", ["--test", compiledTest], { stdio: "inherit" });
  process.exit(testRun.status ?? 1);
} finally {
  if (!process.env.HELPER_TEST_DEBUG) {
    try {
      rmSync(out, { recursive: true, force: true });
    } catch {
      // ignore
    }
  }
}
