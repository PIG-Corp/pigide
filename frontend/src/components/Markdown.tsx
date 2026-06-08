import React, { useMemo } from "react";
import { sanitizeUrl } from "./markdownSanitize";

// Escape HTML utility to prevent XSS
function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

// B-12.4: max blockquote recursion depth — past this we render text
// inline instead of recursing.
const MAX_BLOCKQUOTE_DEPTH = 6;

// Parse inline markdown to HTML string (safely, after escaping raw HTML)
function parseInline(text: string): string {
  let html = escapeHtml(text);

  // Bold: **text** or __text__
  html = html.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  html = html.replace(/__([^_]+)__/g, "<strong>$1</strong>");

  // Italic: *text* or _text_
  html = html.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  html = html.replace(/_([^_]+)_/g, "<em>$1</em>");

  // Inline code: `code`
  html = html.replace(/`([^`]+)`/g, "<code>$1</code>");

  // Links: [label](url) — sanitize the URL; unsafe schemes render as text.
  html = html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, label: string, url: string) => {
    const safe = sanitizeUrl(url);
    if (safe === null) {
      return `${label} (${url})`;
    }
    return `<a href="${safe}" target="_blank" rel="noopener noreferrer">${label}</a>`;
  });

  return html;
}

export function Markdown({ content, depth = 0 }: { content: string; depth?: number }) {
  const blocks = useMemo(() => {
    if (!content) return [];
    
    const lines = content.split(/\r?\n/);
    const parsedBlocks: React.ReactNode[] = [];
    
    let i = 0;
    while (i < lines.length) {
      const line = lines[i];
      
      // 1. Fenced Code Block
      if (line.trim().startsWith("```")) {
        const lang = line.trim().slice(3).trim();
        const codeLines: string[] = [];
        i++;
        while (i < lines.length && !lines[i].trim().startsWith("```")) {
          codeLines.push(lines[i]);
          i++;
        }
        // Skip the closing ```
        if (i < lines.length) {
          i++;
        }
        parsedBlocks.push(
          <pre key={`code-${i}`}>
            <code className={lang ? `language-${lang}` : ""}>
              {codeLines.join("\n")}
            </code>
          </pre>
        );
        continue;
      }
      
      // 2. Blockquote
      if (line.startsWith(">")) {
        const quoteLines: string[] = [];
        while (i < lines.length && lines[i].startsWith(">")) {
          quoteLines.push(lines[i].slice(1).trim());
          i++;
        }
        parsedBlocks.push(
          // B-12.4: cap blockquote nesting depth. A pathological body like
          // `>>>>>>>>>>` (100 levels) would otherwise recurse and overflow
          // the stack. Deeper levels are flattened to plain paragraphs.
          depth >= MAX_BLOCKQUOTE_DEPTH ? (
            <blockquote key={`quote-${i}`}>
              {quoteLines.join("\n")}
            </blockquote>
          ) : (
            <blockquote key={`quote-${i}`}>
              <Markdown content={quoteLines.join("\n")} depth={depth + 1} />
            </blockquote>
          )
        );
        continue;
      }
      
      // 3. Unordered list
      if (line.trim().startsWith("- ") || line.trim().startsWith("* ")) {
        const items: string[] = [];
        while (i < lines.length && (lines[i].trim().startsWith("- ") || lines[i].trim().startsWith("* "))) {
          items.push(lines[i].trim().slice(2));
          i++;
        }
        parsedBlocks.push(
          <ul key={`ul-${i}`}>
            {items.map((item, idx) => (
              <li
                key={idx}
                dangerouslySetInnerHTML={{ __html: parseInline(item) }}
              />
            ))}
          </ul>
        );
        continue;
      }
      
      // 4. Ordered list
      if (/^\s*\d+\.\s+/.test(line)) {
        const items: string[] = [];
        while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
          const itemText = lines[i].trim().replace(/^\d+\.\s+/, "");
          items.push(itemText);
          i++;
        }
        parsedBlocks.push(
          <ol key={`ol-${i}`}>
            {items.map((item, idx) => (
              <li
                key={idx}
                dangerouslySetInnerHTML={{ __html: parseInline(item) }}
              />
            ))}
          </ol>
        );
        continue;
      }
      
      // 5. Table
      if (line.trim().startsWith("|")) {
        const rows: string[][] = [];
        while (i < lines.length && lines[i].trim().startsWith("|")) {
          const rowText = lines[i].trim();
          const cells = rowText
            .split("|")
            .map((c) => c.trim())
            .filter((_, idx, arr) => idx > 0 && idx < arr.length - 1);
          rows.push(cells);
          i++;
        }
        
        let hasHeader = false;
        let dataRows = rows;
        let headerRow: string[] = [];
        
        if (rows.length > 1 && rows[1].every((cell) => /^[:\s-]*$/.test(cell))) {
          hasHeader = true;
          headerRow = rows[0];
          dataRows = rows.slice(2);
        }
        
        parsedBlocks.push(
          <table key={`table-${i}`}>
            {hasHeader && (
              <thead>
                <tr>
                  {headerRow.map((cell, idx) => (
                    <th
                      key={idx}
                      dangerouslySetInnerHTML={{ __html: parseInline(cell) }}
                    />
                  ))}
                </tr>
              </thead>
            )}
            <tbody>
              {dataRows.map((row, rIdx) => (
                <tr key={rIdx}>
                  {row.map((cell, cIdx) => (
                    <td
                      key={cIdx}
                      dangerouslySetInnerHTML={{ __html: parseInline(cell) }}
                    />
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        );
        continue;
      }
      
      // 6. Headers
      const headerMatch = line.match(/^(#{1,6})\s+(.*)$/);
      if (headerMatch) {
        const level = headerMatch[1].length;
        const text = headerMatch[2];
        const html = { __html: parseInline(text) };
        const key = `h-${i}`;
        
        switch (level) {
          case 1:
            parsedBlocks.push(<h1 key={key} dangerouslySetInnerHTML={html} />);
            break;
          case 2:
            parsedBlocks.push(<h2 key={key} dangerouslySetInnerHTML={html} />);
            break;
          case 3:
            parsedBlocks.push(<h3 key={key} dangerouslySetInnerHTML={html} />);
            break;
          case 4:
            parsedBlocks.push(<h4 key={key} dangerouslySetInnerHTML={html} />);
            break;
          case 5:
            parsedBlocks.push(<h5 key={key} dangerouslySetInnerHTML={html} />);
            break;
          default:
            parsedBlocks.push(<h6 key={key} dangerouslySetInnerHTML={html} />);
            break;
        }
        i++;
        continue;
      }
      
      // 7. Horizontal Rule
      if (/^(\-{3,}|\*{3,})$/.test(line.trim())) {
        parsedBlocks.push(<hr key={`hr-${i}`} />);
        i++;
        continue;
      }
      
      // 8. Paragraph (combine consecutive plain text lines)
      if (line.trim() !== "") {
        const paraLines: string[] = [];
        while (
          i < lines.length &&
          lines[i].trim() !== "" &&
          !lines[i].trim().startsWith("```") &&
          !lines[i].startsWith(">") &&
          !lines[i].trim().startsWith("- ") &&
          !lines[i].trim().startsWith("* ") &&
          !/^\s*\d+\.\s+/.test(lines[i]) &&
          !lines[i].trim().startsWith("|") &&
          !/^(#{1,6})\s+/.test(lines[i]) &&
          !/^(\-{3,}|\*{3,})$/.test(lines[i].trim())
        ) {
          paraLines.push(lines[i]);
          i++;
        }
        parsedBlocks.push(
          <p
            key={`p-${i}`}
            dangerouslySetInnerHTML={{ __html: parseInline(paraLines.join(" ")) }}
          />
        );
        continue;
      }
      
      i++;
    }
    
    return parsedBlocks;
  }, [content]);

  return <div className="markdown-body">{blocks}</div>;
}
