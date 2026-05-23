// Wikilink-aware CodeMirror editor for PigMemory.
//
// Hosts CodeMirror 6 with markdown syntax + a custom decoration plugin that
// styles `[[wikilinks]]` (resolved/unresolved) and `#tags`, plus an
// autocomplete source that suggests existing notes when the user starts
// typing inside `[[…`.

import { useEffect, useRef } from "react";
import { EditorState, Compartment, RangeSetBuilder } from "@codemirror/state";
import {
  EditorView,
  keymap,
  highlightActiveLine,
  Decoration,
  type DecorationSet,
  ViewPlugin,
  type ViewUpdate,
} from "@codemirror/view";
import {
  defaultKeymap,
  historyKeymap,
  history,
  indentWithTab,
} from "@codemirror/commands";
import { searchKeymap } from "@codemirror/search";
import {
  autocompletion,
  completionKeymap,
  type CompletionContext,
} from "@codemirror/autocomplete";
import {
  bracketMatching,
  indentOnInput,
  syntaxHighlighting,
  defaultHighlightStyle,
} from "@codemirror/language";
import { markdown } from "@codemirror/lang-markdown";
import type { NoteSummary } from "../../state/types";
import { resolveWikilink } from "./wikilink";

const editorTheme = EditorView.theme(
  {
    "&": {
      height: "100%",
      fontSize: "14px",
      backgroundColor: "transparent",
      color: "var(--fg)",
    },
    ".cm-content": {
      padding: "20px 28px 80px 28px",
      caretColor: "var(--accent)",
      fontFamily: 'var(--font-sans)',
      lineHeight: "1.65",
    },
    ".cm-line": {
      padding: "0",
    },
    ".cm-scroller": {
      fontFamily: 'var(--font-sans)',
      lineHeight: "1.65",
      overflow: "auto",
    },
    ".cm-activeLine": {
      backgroundColor: "transparent",
    },
    ".cm-cursor": { borderLeftColor: "var(--accent)" },
    ".cm-selectionBackground, ::selection": {
      backgroundColor: "var(--selection)",
    },
    ".cm-focused": { outline: "none" },
    ".pigmem-cm-wikilink": {
      color: "var(--accent)",
      backgroundColor: "var(--accent-soft)",
      borderRadius: "3px",
      padding: "0 2px",
    },
    ".pigmem-cm-wikilink-unresolved": {
      color: "var(--warn)",
      backgroundColor: "var(--warn-soft)",
      borderRadius: "3px",
      padding: "0 2px",
      textDecoration: "underline dashed",
      textUnderlineOffset: "3px",
    },
    ".pigmem-cm-hashtag": {
      color: "var(--info)",
      fontWeight: "500",
    },
    ".cm-tooltip-autocomplete": {
      backgroundColor: "var(--bg-raised)",
      border: "1px solid var(--border-strong)",
      borderRadius: "var(--radius-lg)",
      boxShadow: "var(--shadow-3)",
      fontFamily: "var(--font-sans)",
    },
    ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
      backgroundColor: "var(--accent-soft)",
      color: "var(--fg)",
    },
  },
  { dark: true },
);

function buildHighlightPlugin(notesRef: { current: NoteSummary[] }) {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = this.build(view);
      }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged) {
          this.decorations = this.build(u.view);
        }
      }
      build(view: EditorView): DecorationSet {
        const builder = new RangeSetBuilder<Decoration>();
        const text = view.state.doc.toString();
        const wikiRe = /\[\[([^\]\n]+)\]\]/g;
        let m: RegExpExecArray | null;
        while ((m = wikiRe.exec(text)) !== null) {
          const inner = m[1];
          const pipe = inner.indexOf("|");
          const target = (pipe >= 0 ? inner.slice(0, pipe) : inner).trim();
          const resolved = resolveWikilink(target, notesRef.current);
          builder.add(
            m.index,
            m.index + m[0].length,
            Decoration.mark({
              class: resolved
                ? "pigmem-cm-wikilink"
                : "pigmem-cm-wikilink-unresolved",
            }),
          );
        }
        const tagRe = /(^|\s)#([\p{L}\p{N}][\p{L}\p{N}_-]*)/gu;
        while ((m = tagRe.exec(text)) !== null) {
          const lead = m[1] ?? "";
          const start = m.index + lead.length;
          builder.add(
            start,
            start + 1 + m[2].length,
            Decoration.mark({ class: "pigmem-cm-hashtag" }),
          );
        }
        return builder.finish();
      }
    },
    {
      decorations: (v) => v.decorations,
    },
  );
}

function makeWikilinkCompletion(notesRef: { current: NoteSummary[] }) {
  return (ctx: CompletionContext) => {
    // Inside `[[…`?
    const before = ctx.state.doc.sliceString(0, ctx.pos);
    const open = before.lastIndexOf("[[");
    if (open < 0) return null;
    const newline = before.lastIndexOf("\n");
    if (newline > open) return null;
    const close = before.indexOf("]]", open);
    if (close >= 0 && close < ctx.pos) return null;
    const prefix = before.slice(open + 2);
    const lower = prefix.toLowerCase();
    const matches = notesRef.current
      .filter(
        (n) =>
          n.title.toLowerCase().includes(lower) ||
          n.slug.toLowerCase().includes(lower),
      )
      .slice(0, 30);
    return {
      from: open + 2,
      options: matches.map((n) => ({
        label: n.title,
        detail: n.slug,
        apply: `${n.title}]]`,
      })),
      validFor: /^[\p{L}\p{N}\s_-]*$/u,
    };
  };
}

export interface PigMemoryEditorProps {
  initial: string;
  onChange: (next: string) => void;
  onSave: () => void;
  notes: NoteSummary[];
  /** When this changes, the editor swaps content (different note opened). */
  noteId: string;
}

export function PigMemoryEditor({
  initial,
  onChange,
  onSave,
  notes,
  noteId,
}: PigMemoryEditorProps) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const onChangeRef = useRef(onChange);
  const onSaveRef = useRef(onSave);
  const notesRef = useRef<NoteSummary[]>(notes);
  const completionCompartment = useRef(new Compartment());

  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);
  useEffect(() => {
    onSaveRef.current = onSave;
  }, [onSave]);
  useEffect(() => {
    notesRef.current = notes;
  }, [notes]);

  useEffect(() => {
    if (!hostRef.current) return;
    const completion = autocompletion({
      override: [makeWikilinkCompletion(notesRef)],
      activateOnTyping: true,
    });
    const saveKey = keymap.of([
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
        history(),
        bracketMatching(),
        indentOnInput(),
        highlightActiveLine(),
        markdown(),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        keymap.of([
          ...defaultKeymap,
          ...historyKeymap,
          ...searchKeymap,
          ...completionKeymap,
          indentWithTab,
        ]),
        saveKey,
        editorTheme,
        EditorView.lineWrapping,
        buildHighlightPlugin(notesRef),
        completionCompartment.current.of(completion),
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Swap content when noteId changes.
  useEffect(() => {
    const v = viewRef.current;
    if (!v) return;
    const cur = v.state.doc.toString();
    if (cur !== initial) {
      v.dispatch({
        changes: { from: 0, to: cur.length, insert: initial },
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [noteId]);

  return <div ref={hostRef} className="pigmem-editor-host" />;
}
