import { useState } from "react";
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

  const visible = THEMES.filter((t) => filter === "all" || t.kind === filter);

  return (
    <div className="theme-picker-backdrop" onClick={onClose}>
      <div
        className="theme-picker"
        onClick={(e) => e.stopPropagation()}
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
          <button className="theme-picker-close" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="theme-picker-grid">
          {visible.map((t) => (
            <button
              key={t.id}
              className={`theme-card ${theme.id === t.id ? "active" : ""}`}
              onClick={() => setTheme(t.id)}
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
