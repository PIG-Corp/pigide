import { useEffect, useRef, useState } from "react";
import { THEMES } from "../themes/catalog";
import { useTheme } from "../themes/useTheme";

/**
 * ThemePicker — tabbed dialog showing every catalog theme with a live
 * swatch. Clicking a swatch instantly applies the theme; the choice is
 * persisted by `useTheme().setTheme`.
 */
export function ThemePicker({ onClose }: { onClose: () => void }) {
  const { theme, setTheme } = useTheme();
  const [filter, setFilter] = useState<"all" | "dark" | "light">("all");
  const dialogRef = useRef<HTMLDivElement>(null);

  const visible = THEMES.filter((t) => filter === "all" || t.kind === filter);

  // Focus first focusable element on open
  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    const focusable = el.querySelector<HTMLElement>(
      'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    focusable?.focus();
  }, []);

  // Close on Escape, trap Tab
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "Tab") {
      const el = dialogRef.current;
      if (!el) return;
      const focusable = Array.from(
        el.querySelectorAll<HTMLElement>(
          'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
        )
      ).filter((n) => !n.hasAttribute("disabled"));
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey) {
        if (document.activeElement === first) {
          e.preventDefault();
          last.focus();
        }
      } else {
        if (document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    }
  };

  return (
    <div className="theme-picker-backdrop" onClick={onClose}>
      <div
        ref={dialogRef}
        className="theme-picker"
        role="dialog"
        aria-modal="true"
        aria-label="Choose Theme"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={handleKeyDown}
      >
        <div className="theme-picker-head">
          <span className="theme-picker-title">Themes</span>
          <div className="theme-picker-filters">
            <button
              className={filter === "all" ? "active" : ""}
              onClick={() => setFilter("all")}
            >
              All
            </button>
            <button
              className={filter === "dark" ? "active" : ""}
              onClick={() => setFilter("dark")}
            >
              Dark
            </button>
            <button
              className={filter === "light" ? "active" : ""}
              onClick={() => setFilter("light")}
            >
              Light
            </button>
          </div>
          <button className="theme-picker-close" onClick={onClose} aria-label="Close theme picker">
            ×
          </button>
        </div>

        <div className="theme-picker-grid">
          {visible.map((t) => (
            <button
              key={t.id}
              className={`theme-card ${theme.id === t.id ? "active" : ""}`}
              onClick={() => setTheme(t.id)}
              aria-pressed={theme.id === t.id}
              aria-label={`${t.name} (${t.kind})`}
              style={{
                background: t.css.bgPanel,
                borderColor:
                  theme.id === t.id ? t.css.accent : t.css.border,
              }}
            >
              <span
                className="theme-card-swatch"
                style={{
                  background: `linear-gradient(135deg, ${t.css.bg} 0% 50%, ${t.css.bgRaised} 50% 100%)`,
                  borderColor: t.css.borderStrong,
                }}
              >
                <span
                  className="theme-card-dot"
                  style={{ background: t.css.accent }}
                />
                <span
                  className="theme-card-dot"
                  style={{ background: t.css.success }}
                />
                <span
                  className="theme-card-dot"
                  style={{ background: t.css.danger }}
                />
              </span>
              <span
                className="theme-card-name"
                style={{ color: t.css.fg }}
              >
                {t.name}
              </span>
              <span
                className="theme-card-kind"
                style={{ color: t.css.fgMuted }}
              >
                {t.kind}
              </span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
