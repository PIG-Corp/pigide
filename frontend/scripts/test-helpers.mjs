// Compile-then-run the pathMentionHelpers tests using ONLY tools the
// project already ships (`tsc` + `node --test`). Zero new deps.
//
// Strategy: snapshot the helper module + its test into a temp dir,
// inline the `PathAttachment` type so we don't drag in DOM-tied
// `state/types.ts`, rewrite the relative import to a `.js` ESM path,
// invoke the bundled tsc, then run `node --test` on the emit.

import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const out = mkdtempSync(join(tmpdir(), "pigide-helper-test-"));

const PATH_ATTACHMENT_INLINE = `
type PathAttachmentKind = "file" | "dir";
type PathAttachment = { kind: PathAttachmentKind; path: string; label: string };
`;

function rewrite(src) {
  // Strip the type-only import and inline a minimal PathAttachment.
  const stripped = src.replace(
    /^import type \{[^}]*\} from "\.\.\/state\/types";\s*$/m,
    PATH_ATTACHMENT_INLINE,
  );
  // Force ESM-style explicit `.js` for relative imports under node16
  // module resolution.
  return stripped.replace(
    /from "\.\/pathMentionHelpers"/g,
    'from "./pathMentionHelpers.js"',
  );
}

try {
  const helpersTs = readFileSync(
    join(root, "src/components/pathMentionHelpers.ts"),
    "utf8",
  );
  const testTs = readFileSync(
    join(root, "scripts/pathMentionHelpers.test.ts"),
    "utf8",
  );
  mkdirSync(join(out, "src/components"), { recursive: true });
  writeFileSync(join(out, "src/components/pathMentionHelpers.ts"), rewrite(helpersTs));
  writeFileSync(join(out, "src/components/pathMentionHelpers.test.ts"), rewrite(testTs));
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
  // Mark the temp project as ESM so the emitted `.js` runs as a module.
  writeFileSync(
    join(out, "package.json"),
    JSON.stringify({ name: "helper-tests", type: "module", private: true }),
  );

  const tsc = join(root, "node_modules/typescript/bin/tsc");
  const tscRun = spawnSync("node", [tsc, "-p", join(out, "tsconfig.json")], {
    stdio: "inherit",
    env: {
      ...process.env,
      // Resolve `@types/node` from the project's node_modules.
      NODE_PATH: join(root, "node_modules"),
    },
  });
  if (tscRun.status !== 0) {
    console.error("tsc failed (status=" + tscRun.status + ")");
    process.exit(tscRun.status ?? 1);
  }

  const compiledTest = join(out, "dist/components/pathMentionHelpers.test.js");
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
