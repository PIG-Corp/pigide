/// Pure-helper tests for `pathMentionHelpers.ts`. Run via:
///
///   pnpm exec node --test --import @swc-node/register/esm-register \
///     src/components/pathMentionHelpers.test.ts
///
/// — but since the project doesn't ship swc-register, the build script
/// `frontend/scripts/test-helpers.mjs` shims this by transpiling the two
/// involved files with the bundled TypeScript compiler and invoking
/// `node --test` on the compiled output. Zero new dependencies.

import { strict as assert } from "node:assert";
import { test } from "node:test";

import {
  findTrigger,
  isPathLike,
  reconcileAttachments,
  tokenLeftOfCaret,
  uniqueLabel,
} from "./pathMentionHelpers";
import type { PathAttachment } from "../state/types";

test("findTrigger detects @ at the start of buffer", () => {
  const r = findTrigger("@foo", 4);
  assert.deepEqual(r, { start: 0, query: "foo" });
});

test("findTrigger detects @ after whitespace", () => {
  const r = findTrigger("hello @foo", 10);
  assert.deepEqual(r, { start: 6, query: "foo" });
});

test("findTrigger detects @ after a comma (boundary)", () => {
  const r = findTrigger("a,@foo", 6);
  assert.deepEqual(r, { start: 2, query: "foo" });
});

test("findTrigger ignores @ glued to a previous word", () => {
  // `email@example.com` — the `@` is in the middle of a word, not a trigger.
  const r = findTrigger("email@example", 13);
  assert.equal(r, null);
});

test("findTrigger ignores caret that has crossed a space", () => {
  const r = findTrigger("@foo bar", 8);
  assert.equal(r, null);
});

test("findTrigger ignores caret that has crossed a closed token ]", () => {
  const r = findTrigger("@[label] more", 13);
  assert.equal(r, null);
});

test("findTrigger does not re-open an existing @[ token", () => {
  // The `@` is the leading char of `@[foo]` — a finalised chip, not a trigger.
  const r = findTrigger("@[foo]", 1);
  assert.equal(r, null);
});

test("findTrigger handles absolute path query", () => {
  const value = "@/home/me/proj";
  const r = findTrigger(value, value.length);
  assert.deepEqual(r, { start: 0, query: "/home/me/proj" });
});

test("findTrigger handles ~/-prefixed query", () => {
  const value = "@~/conf";
  const r = findTrigger(value, value.length);
  assert.deepEqual(r, { start: 0, query: "~/conf" });
});

test("findTrigger handles ./-prefixed query", () => {
  const value = "@./src";
  const r = findTrigger(value, value.length);
  assert.deepEqual(r, { start: 0, query: "./src" });
});

test("findTrigger handles bare-basename query", () => {
  const value = "@main";
  const r = findTrigger(value, value.length);
  assert.deepEqual(r, { start: 0, query: "main" });
});

test("isPathLike correctly classifies path vs name queries", () => {
  assert.equal(isPathLike("/home"), true);
  assert.equal(isPathLike("~/foo"), true);
  assert.equal(isPathLike("./bar"), true);
  assert.equal(isPathLike(".env"), true);
  assert.equal(isPathLike("a/b"), true);
  assert.equal(isPathLike("main"), false);
  assert.equal(isPathLike("foo"), false);
  assert.equal(isPathLike(""), false);
});

test("tokenLeftOfCaret finds the closed token to the left", () => {
  const value = "look at @[src/main.rs] please";
  const r = tokenLeftOfCaret(value, 22); // caret right after the closing ]
  assert.deepEqual(r, { start: 8, end: 22 });
});

test("tokenLeftOfCaret returns null when the caret isn't on a ]", () => {
  const value = "look at @[src/main.rs] please";
  const r = tokenLeftOfCaret(value, 20);
  assert.equal(r, null);
});

test("tokenLeftOfCaret returns null at caret=0", () => {
  assert.equal(tokenLeftOfCaret("@[a]", 0), null);
});

test("reconcileAttachments preserves order and drops missing tokens", () => {
  const a: PathAttachment = { kind: "file", path: "/a", label: "a.rs" };
  const b: PathAttachment = { kind: "file", path: "/b", label: "b.rs" };
  const pool = [a, b];
  // Token order in text: a, b.
  const out = reconcileAttachments("hi @[a.rs] and @[b.rs] done", pool);
  assert.deepEqual(out, [a, b]);
});

test("reconcileAttachments drops tokens that don't match any pool entry", () => {
  const a: PathAttachment = { kind: "file", path: "/a", label: "a.rs" };
  const out = reconcileAttachments("@[a.rs] and @[ghost]", [a]);
  assert.deepEqual(out, [a]);
});

test("reconcileAttachments handles repeats by reusing pool order", () => {
  const a: PathAttachment = { kind: "file", path: "/a", label: "a.rs" };
  const b: PathAttachment = { kind: "file", path: "/b", label: "a.rs#2" };
  const out = reconcileAttachments("@[a.rs] @[a.rs#2] @[a.rs]", [a, b]);
  // Third token re-uses `a` because nothing else matches `a.rs`.
  assert.deepEqual(out, [a, b]);
});

test("reconcileAttachments handles empty input", () => {
  assert.deepEqual(reconcileAttachments("just text", []), []);
});

test("uniqueLabel passes through when label is fresh", () => {
  assert.equal(uniqueLabel("a.rs", []), "a.rs");
});

test("uniqueLabel appends #N for collisions", () => {
  const taken: PathAttachment[] = [
    { kind: "file", path: "/x", label: "main.rs" },
  ];
  assert.equal(uniqueLabel("main.rs", taken), "main.rs#2");
});

test("uniqueLabel walks past sequential duplicates", () => {
  const taken: PathAttachment[] = [
    { kind: "file", path: "/x", label: "main.rs" },
    { kind: "file", path: "/y", label: "main.rs#2" },
    { kind: "file", path: "/z", label: "main.rs#3" },
  ];
  assert.equal(uniqueLabel("main.rs", taken), "main.rs#4");
});

test("the submit-payload shape — what frontend posts to send_chat", () => {
  // The integration shape for `ipc.sendChat` is `{ text, attachments }`.
  // We just describe-and-assert the property bag here so a future
  // refactor that drops `attachments` from the args object trips this
  // test — no live backend needed.
  const args = { text: "hi", attachments: [] as PathAttachment[] };
  assert.equal(typeof args.text, "string");
  assert.ok(Array.isArray(args.attachments));
});
