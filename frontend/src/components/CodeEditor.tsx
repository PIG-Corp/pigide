import { useEffect, useRef } from "react";
import { EditorState, Compartment } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, historyKeymap, history, indentWithTab } from "@codemirror/commands";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import { autocompletion, completionKeymap } from "@codemirror/autocomplete";
import { bracketMatching, indentOnInput, syntaxHighlighting, defaultHighlightStyle } from "@codemirror/language";
import { javascript } from "@codemirror/lang-javascript";
import { rust } from "@codemirror/lang-rust";
import { python } from "@codemirror/lang-python";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";

/**
 * Resolve a CodeMirror language extension from a file path. Falls back to
 * an empty array (no language) for unrecognised extensions so the editor
 * still works as a plain-text editor.
 */
function languageFor(path: string) {
  const ext = path.toLowerCase().split(".").pop() ?? "";
  switch (ext) {
    case "ts":
    case "tsx":
      return [javascript({ jsx: true, typescript: true })];
    case "js":
    case "jsx":
    case "mjs":
    case "cjs":
      return [javascript({ jsx: true })];
    case "rs":
      return [rust()];
    case "py":
      return [python()];
    case "json":
      return [json()];
    case "md":
    case "markdown":
      return [markdown()];
    case "html":
    case "htm":
      return [html()];
    case "css":
      return [css()];
    default:
      return [];
  }
}

const editorTheme = EditorView.theme(
  {
    "&": {
      height: "100%",
      fontSize: "12px",
      backgroundColor: "var(--bg, #0a0b0e)",
      color: "var(--fg, #d6d8dd)",
    },
    ".cm-scroller": {
      fontFamily: 'ui-monospace, "Fira Code", Menlo, Consolas, monospace',
      lineHeight: "1.5",
    },
    ".cm-gutters": {
      backgroundColor: "var(--bg-panel, #0e0f12)",
      color: "var(--fg-muted, #5c6270)",
      borderRight: "1px solid var(--border, #1f2229)",
    },
    ".cm-activeLine": { backgroundColor: "rgba(125, 161, 255, 0.06)" },
    ".cm-selectionBackground, ::selection": {
      backgroundColor: "var(--selection, #2f4f76)",
    },
    ".cm-cursor": { borderLeftColor: "var(--accent, #5d7ff5)" },
  },
  { dark: true },
);

export interface CodeEditorProps {
  path: string;
  initial: string;
  onChange: (next: string) => void;
  onSave?: () => void;
}

/**
 * CodeEditor — CodeMirror 6 wrapper used by FilesPanel (BridgeSpace gap #2).
 *
 * Lifecycle:
 *   - mount: build EditorView once with the language matching `path`.
 *   - changing `path`: dispatch a transaction that swaps both content and
 *     language, instead of remounting (so undo history stays intact while
 *     switching tabs of the same file is a no-op).
 */
export function CodeEditor({ path, initial, onChange, onSave }: CodeEditorProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const langCompartment = useRef(new Compartment());
  const onChangeRef = useRef(onChange);
  const onSaveRef = useRef(onSave);

  // Keep refs fresh so the editor's stable callback always sees the
  // latest onChange/onSave without us having to recreate the editor.
  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);
  useEffect(() => {
    onSaveRef.current = onSave;
  }, [onSave]);

  useEffect(() => {
    if (!hostRef.current) return;
    const saveKeymap = keymap.of([
      {
        key: "Mod-s",
        preventDefault: true,
        run: () => {
          onSaveRef.current?.();
          return true;
        },
      },
    ]);
    const state = EditorState.create({
      doc: initial,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        history(),
        bracketMatching(),
        indentOnInput(),
        autocompletion(),
        highlightSelectionMatches(),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        keymap.of([
          ...defaultKeymap,
          ...historyKeymap,
          ...searchKeymap,
          ...completionKeymap,
          indentWithTab,
        ]),
        saveKeymap,
        editorTheme,
        langCompartment.current.of(languageFor(path)),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) {
            onChangeRef.current(u.state.doc.toString());
          }
        }),
      ],
    });
    const view = new EditorView({ state, parent: hostRef.current });
    viewRef.current = view;
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // We deliberately omit `initial` from deps: re-initialising would erase
    // undo history. Use the second effect for content/path swaps instead.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Swap document + language when `path` changes.
  useEffect(() => {
    const v = viewRef.current;
    if (!v) return;
    const cur = v.state.doc.toString();
    if (cur !== initial) {
      v.dispatch({
        changes: { from: 0, to: cur.length, insert: initial },
        effects: langCompartment.current.reconfigure(languageFor(path)),
      });
    } else {
      v.dispatch({
        effects: langCompartment.current.reconfigure(languageFor(path)),
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path]);

  return <div ref={hostRef} className="code-editor-host" />;
}
