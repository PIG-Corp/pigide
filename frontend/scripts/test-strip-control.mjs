// Compile-then-run the stripControl tests using ONLY tools the project
// already ships (`tsc` + `node --test`). Zero new deps.
//
// Mirrors test-helpers.mjs but for the smaller lib/stripControl module
// (no React / DOM / Tauri dependencies, so no rewrite gymnastics needed).

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const out = mkdtempSync(join(tmpdir(), "pigide-strip-test-"));

try {
  mkdirSync(join(out, "src/lib"), { recursive: true });
  mkdirSync(join(out, "scripts"), { recursive: true });

  // Inline the source + test verbatim. Co-locate both files under
  // `src/lib/` so the test's relative import resolves under node16
  // module resolution (rootDir = "./src"). The test imports the helper
  // with a `.js` suffix (required at runtime under ESM); TypeScript is
  // happy because tsc emits to dist/lib/ and the .js sibling exists.
  const testSrc = readFileSync(join(root, "scripts/stripControl.test.ts"), "utf8")
    .replace(/from "\.\/stripControl\.js"/g, 'from "./stripControl.js"');
  writeFileSync(
    join(out, "src/lib/stripControl.ts"),
    readFileSync(join(root, "src/lib/stripControl.ts"), "utf8"),
  );
  writeFileSync(join(out, "src/lib/stripControl.test.ts"), testSrc);

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
    JSON.stringify({ name: "strip-tests", type: "module", private: true }),
  );

  const tsc = join(root, "node_modules/typescript/bin/tsc");
  const tscRun = spawnSync("node", [tsc, "-p", join(out, "tsconfig.json")], {
    stdio: "inherit",
    env: {
      ...process.env,
      NODE_PATH: join(root, "node_modules"),
    },
  });
  if (tscRun.status !== 0) {
    console.error("tsc failed (status=" + tscRun.status + ")");
    process.exit(tscRun.status ?? 1);
  }

  const compiledTest = join(out, "dist/lib/stripControl.test.js");
  const testRun = spawnSync("node", ["--test", compiledTest], { stdio: "inherit" });
  process.exit(testRun.status ?? 1);
} finally {
  if (!process.env.STRIP_TEST_DEBUG) {
    try {
      rmSync(out, { recursive: true, force: true });
    } catch {
      /* ignore */
    }
  }
}
