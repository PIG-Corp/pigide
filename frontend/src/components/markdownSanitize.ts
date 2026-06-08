// URL sanitizer for the inline-markdown link renderer. Pure, no React/DOM
// imports, so it can be unit-tested with the bundled tsc + node --test.
//
// Security-first: any URL carrying a scheme is allowed ONLY when that
// scheme is explicitly safe (http, https, mailto). Everything else with a
// scheme — javascript:, data:, vbscript:, file:, tel:, ftp:, ... — is
// rejected. URLs without a scheme (relative paths, anchors, protocol-less)
// are allowed. Input may already be HTML-escaped by the caller, so we
// decode the handful of entities escapeHtml() produces, and strip embedded
// whitespace (browsers ignore tab/newline/CR inside a scheme) before the
// scheme is inspected. Returns null when unsafe — caller renders plain text.
const SAFE_SCHEMES = new Set(["http", "https", "mailto"]);

export function sanitizeUrl(url: string): string | null {
  const decoded = url
    .replace(/&amp;/gi, "&")
    .replace(/&#039;/g, "'")
    .replace(/&quot;/gi, '"')
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">");
  const stripped = decoded.replace(/\s+/g, "").toLowerCase();
  const schemeMatch = stripped.match(/^([a-z][a-z0-9+.-]*):/i);
  if (schemeMatch) {
    return SAFE_SCHEMES.has(schemeMatch[1].toLowerCase()) ? url : null;
  }
  // No scheme: relative path, anchor, or protocol-less host — safe to keep.
  return url;
}
