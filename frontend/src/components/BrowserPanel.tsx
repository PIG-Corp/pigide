import { useEffect, useRef, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import { ArrowLeft, ArrowRight, RotateCw, X } from "./icons";

function isSafeUrl(url: string): boolean {
  try {
    const u = new URL(url);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

const DEFAULT_BOOKMARKS = [
  { name: "MDN", url: "https://developer.mozilla.org/" },
  { name: "Tauri", url: "https://tauri.app/" },
  { name: "React", url: "https://react.dev/" },
];

/**
 * BrowserPanel — minimal in-app webview using <iframe>. Stores the last URL
 * + bookmarks in the settings KV so they survive across restarts.
 *
 * Note: this is a renderable preview, not a full browser. CORS-locked sites
 * may refuse to embed (X-Frame-Options DENY) — surface a hint when that
 * happens.
 */
export function BrowserPanel({ onClose }: { onClose?: () => void }) {
  const [url, setUrl] = useState("");
  const [draft, setDraft] = useState("");
  const [bookmarks, setBookmarks] = useState(DEFAULT_BOOKMARKS);
  const [showBookmarks, setShowBookmarks] = useState(false);
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const pushToast = useStore((s) => s.pushToast);

  useEffect(() => {
    (async () => {
      try {
        const last = await ipc.getSetting("browser.last_url");
        if (last) {
          setUrl(last);
          setDraft(last);
        }
        const bm = await ipc.getSetting("browser.bookmarks");
        if (bm) {
          try {
            setBookmarks(JSON.parse(bm));
          } catch {
            /* fall back to defaults */
          }
        }
      } catch {
        /* ignore */
      }
    })();
  }, []);

  const navigate = (target: string) => {
    const trimmed = target.trim();
    if (!trimmed) return;
    const withScheme = trimmed.startsWith("http") ? trimmed : `https://${trimmed}`;
    if (!isSafeUrl(withScheme)) {
      pushToast({ text: "Only http: and https: URLs are allowed.", kind: "error" });
      return;
    }
    setUrl(withScheme);
    setDraft(withScheme);
    ipc.setSetting("browser.last_url", withScheme).catch(() => undefined);
  };

  const back = () => {
    try {
      iframeRef.current?.contentWindow?.history.back();
    } catch {
      /* cross-origin */
    }
  };
  const forward = () => {
    try {
      iframeRef.current?.contentWindow?.history.forward();
    } catch {
      /* cross-origin */
    }
  };
  const reload = () => {
    if (iframeRef.current) iframeRef.current.src = url;
  };

  const addBookmark = async () => {
    if (!url) return;
    const name = prompt("Bookmark name?", url) ?? url;
    const next = [...bookmarks, { name, url }];
    setBookmarks(next);
    try {
      await ipc.setSetting("browser.bookmarks", JSON.stringify(next));
    } catch (err) {
      pushToast({ text: `save bookmark: ${err}`, kind: "error" });
    }
  };

  return (
    <div className="browser-panel">
      <div className="browser-toolbar">
        <button onClick={back} title="Back" aria-label="Back">
          <ArrowLeft size={12} />
        </button>
        <button onClick={forward} title="Forward" aria-label="Forward">
          <ArrowRight size={12} />
        </button>
        <button onClick={reload} title="Reload" disabled={!url} aria-label="Reload">
          <RotateCw size={12} />
        </button>
        <input
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") navigate(draft);
          }}
          placeholder="https://…"
        />
        <button onClick={() => navigate(draft)} disabled={!draft.trim()}>
          Go
        </button>
        <button
          onClick={() => setShowBookmarks((v) => !v)}
          title="Bookmarks"
          aria-label="Bookmarks"
          aria-expanded={showBookmarks}
        >
          ★
        </button>
        <button onClick={addBookmark} disabled={!url} title="Add bookmark" aria-label="Add bookmark">
          +
        </button>
        {onClose ? (
          <button
            className="btn--icon"
            onClick={onClose}
            title="Close"
            aria-label="Close browser"
          >
            <X size={12} />
          </button>
        ) : null}
      </div>

      {showBookmarks ? (
        <div className="browser-bookmarks">
          {bookmarks.map((b, i) => (
            <button
              key={i}
              className="browser-bookmark"
              onClick={() => {
                navigate(b.url);
                setShowBookmarks(false);
              }}
              title={b.url}
            >
              {b.name}
            </button>
          ))}
        </div>
      ) : null}

      <div className="browser-frame-wrap">
        {url ? (
          <iframe
            ref={iframeRef}
            src={url}
            // B-6.6: tightened sandbox. The previous flags granted the
            // iframe same-origin access (cookies / localStorage on the
            // Tauri origin) and arbitrary popups + downloads. We keep
            // scripts + forms (so the user can log into most sites) and
            // explicitly drop allow-same-origin / allow-popups /
            // allow-top-navigation. Downloads remain on so the user can
            // save a file when a site offers it.
            sandbox="allow-scripts allow-forms allow-downloads allow-presentation"
            referrerPolicy="no-referrer"
            title="browser"
          />
        ) : (
          <div className="empty-state">
            Enter a URL or pick a bookmark.
          </div>
        )}
      </div>
    </div>
  );
}
