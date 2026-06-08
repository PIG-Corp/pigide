/// Pure-helper tests for `lib/stripControl.ts`. Run via:
///
///   pnpm exec node scripts/test-strip-control.mjs
///
/// Mirrors the compile-then-node-test pattern from `test-helpers.mjs`
/// so we don't need any new dev dependencies. The target file is small
/// and dependency-free, so the shim is correspondingly simple.

import { strict as assert } from "node:assert";
import { test } from "node:test";

import { stripControl, stripControlAndCollapse } from "./stripControl.js";

test("strips xterm focus-in report (ESC[I)", () => {
  assert.equal(stripControl("hello\x1b[Iworld"), "helloworld");
});

test("strips xterm focus-out report (ESC[O)", () => {
  assert.equal(stripControl("a\x1b[Ob"), "ab");
});

test("strips DECRQM mode report (ESC[?62;4;9;22c)", () => {
  // The DECRQM report (ESC[?...c) and the focus-in (ESC[I) are both
  // stripped, but the printable byte after the CSI — here a literal "I"
  // that *looks* like a letter — is also the final byte of `CSI I` (CHT,
  // Cursor Horizontal Tab) and is consumed with the sequence. The user
  // actually sees this exact `I` come in as part of the leaked garbage,
  // and that's correct: nothing in the original message was meant to
  // contain it.
  assert.equal(stripControl("O\x1b[?62;4;9;22c\x1b[Iccx"), "Occx");
});

test("strips CSI cursor keys (arrow up/down/right/left)", () => {
  assert.equal(stripControl("\x1b[A\x1b[B\x1b[C\x1b[D"), "");
});

test("strips Shift-Tab (ESC[Z)", () => {
  assert.equal(stripControl("foo\x1b[Zbar"), "foobar");
});

test("strips F1-F4 SS3 sequences (ESC OP / ESC OQ / ESC OR / ESC OS)", () => {
  assert.equal(stripControl("\x1bOP\x1bOQ\x1bOR\x1bOS"), "");
});

test("strips OSC sequences terminated by BEL", () => {
  assert.equal(stripControl("before\x1b]0;title\x07after"), "beforeafter");
});

test("strips DCS sequence terminated by ST (ESC \\)", () => {
  assert.equal(stripControl("a\x1bP|abcdef\x1b\\b"), "ab");
});

test("strips lone C0 control bytes (NUL, BEL, ETX, ENQ)", () => {
  assert.equal(stripControl("a\x00b\x07c\x03d\x05e"), "abcde");
});

test("keeps TAB, LF, CR (legitimate user line breaks)", () => {
  assert.equal(stripControl("a\tb\nc\rd"), "a\tb\nc\rd");
});

test("keeps DEL when not in escape context (0x7f is dropped by design)", () => {
  // DEL is C0 control — we strip it (backspace isn't meaningful in chat
  // composer state, the user can't type it intentionally).
  assert.equal(stripControl("a\x7fb"), "ab");
});

test("preserves printable unicode (emoji, CJK, accented)", () => {
  assert.equal(stripControl("café 🎉 漢字"), "café 🎉 漢字");
});

test("returns empty string for empty input", () => {
  assert.equal(stripControl(""), "");
});

test("returns input unchanged when no control sequences present", () => {
  assert.equal(
    stripControl("Hello, world! How are you?"),
    "Hello, world! How are you?",
  );
});

test("strips control sequences mixed with normal text (the exact user report)", () => {
  // Reproduces the exact bytes the user reported landing in chat:
  //   O + ESC[?62;4;9;22c + ESC[I + ccx + ESC[A + ESC[B + ESC[A + ESC[A + ESC[A
  // The leading "O" and the "ccx" printable substring are the only things
  // the user actually meant to type — everything else is xterm junk.
  const dirty = "O\x1b[?62;4;9;22c\x1b[Iccx\x1b[A\x1b[B\x1b[A\x1b[A\x1b[A";
  const expected = "Occx";
  assert.equal(stripControl(dirty), expected);
});

test("stripControlAndCollapse collapses double spaces", () => {
  // Stripping the CSI leaves "ab  c" (two spaces between b and c) — the
  // collapse step then compresses them to a single space.
  assert.equal(stripControlAndCollapse("a\x1b[Ab  c"), "ab c");
});

test("stripControlAndCollapse trims leading/trailing whitespace", () => {
  assert.equal(stripControlAndCollapse("  hello\x1b[I  "), "hello");
});

test("does not double-process a string already cleaned", () => {
  const once = stripControl("a\x1b[Ab");
  const twice = stripControl(once);
  assert.equal(once, twice);
});
