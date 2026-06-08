import { test } from "node:test";
import assert from "node:assert/strict";
import { sanitizeUrl } from "../src/components/markdownSanitize.js";

test("allows http and https", () => {
  assert.equal(sanitizeUrl("http://example.com"), "http://example.com");
  assert.equal(sanitizeUrl("https://example.com/x?a=1"), "https://example.com/x?a=1");
});

test("allows mailto, relative, and anchor links", () => {
  assert.equal(sanitizeUrl("mailto:a@b.com"), "mailto:a@b.com");
  assert.equal(sanitizeUrl("/local/path"), "/local/path");
  assert.equal(sanitizeUrl("#section"), "#section");
  assert.equal(sanitizeUrl("./rel.md"), "./rel.md");
  assert.equal(sanitizeUrl("example.com/page"), "example.com/page");
});

test("blocks javascript: scheme", () => {
  assert.equal(sanitizeUrl("javascript:alert(1)"), null);
  assert.equal(sanitizeUrl("JaVaScRiPt:alert(1)"), null);
});

test("blocks data:, vbscript:, file: schemes", () => {
  assert.equal(sanitizeUrl("data:text/html,<script>alert(1)</script>"), null);
  assert.equal(sanitizeUrl("vbscript:msgbox(1)"), null);
  assert.equal(sanitizeUrl("file:///etc/passwd"), null);
});

test("blocks whitespace/control-char obfuscated javascript:", () => {
  assert.equal(sanitizeUrl("java\tscript:alert(1)"), null);
  assert.equal(sanitizeUrl("java\nscript:alert(1)"), null);
  assert.equal(sanitizeUrl(" javascript:alert(1)"), null);
});

test("blocks HTML-entity-escaped javascript:", () => {
  // escapeHtml runs before sanitizeUrl, so the colon survives but the
  // scheme is still detectable after entity-decoding.
  assert.equal(sanitizeUrl("javascript:alert(&#039;x&#039;)"), null);
});

test("blocks unrecognised schemes", () => {
  assert.equal(sanitizeUrl("ftp://example.com"), null);
  assert.equal(sanitizeUrl("tel:+100"), null);
});
