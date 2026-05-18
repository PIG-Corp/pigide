import type { ITheme } from "@xterm/xterm";

/**
 * Theme catalog (BridgeSpace gap #11).
 *
 * Each theme defines:
 *   - CSS variables applied to <html data-theme="…">
 *   - an xterm ITheme used by every PTY tile.
 *
 * Themes are flat presets — no inheritance — so adding/removing a single one
 * never affects the rest. The picker iterates over `THEMES` in declaration
 * order, so this file controls the visual order in the UI as well.
 */

export interface Theme {
  id: string;
  name: string;
  kind: "dark" | "light";
  css: {
    bg: string;
    bgPanel: string;
    bgRaised: string;
    fg: string;
    fgMuted: string;
    border: string;
    borderStrong: string;
    accent: string;
    accentFg: string;
    danger: string;
    success: string;
    warn: string;
    info: string;
    selection: string;
  };
  xterm: ITheme;
}

const t = (
  id: string,
  name: string,
  kind: "dark" | "light",
  css: Theme["css"],
  xterm: ITheme,
): Theme => ({ id, name, kind, css, xterm });

// ─── Dark ────────────────────────────────────────────────
export const VOID = t("void", "Void", "dark", {
  bg: "#0a0b0e", bgPanel: "#0e0f12", bgRaised: "#14161b",
  fg: "#d6d8dd", fgMuted: "#8a8f99",
  border: "#1f2229", borderStrong: "#2a2d34",
  accent: "#5d7ff5", accentFg: "#0a0b0e",
  danger: "#ef4444", success: "#22c55e", warn: "#f59e0b", info: "#38bdf8",
  selection: "#2f4f76",
}, {
  background: "#0a0b0e", foreground: "#d6d8dd",
  cursor: "#d6d8dd", selectionBackground: "#2f4f76",
});

const NEON_TOKYO = t("neon-tokyo", "Neon Tokyo", "dark", {
  bg: "#0c0d1a", bgPanel: "#10122a", bgRaised: "#181b3a",
  fg: "#e9eaff", fgMuted: "#8b8fc7",
  border: "#262a55", borderStrong: "#3a3f7a",
  accent: "#ff5cd0", accentFg: "#0c0d1a",
  danger: "#ff5470", success: "#5cffb3", warn: "#ffc857", info: "#5cd0ff",
  selection: "#3b2c63",
}, {
  background: "#0c0d1a", foreground: "#e9eaff",
  cursor: "#ff5cd0", selectionBackground: "#3b2c63",
  black: "#10122a", red: "#ff5470", green: "#5cffb3", yellow: "#ffc857",
  blue: "#5d7ff5", magenta: "#ff5cd0", cyan: "#5cd0ff", white: "#e9eaff",
});

const DRACULA = t("dracula", "Dracula", "dark", {
  bg: "#282a36", bgPanel: "#21222c", bgRaised: "#343746",
  fg: "#f8f8f2", fgMuted: "#a39da3",
  border: "#3a3d4f", borderStrong: "#525468",
  accent: "#bd93f9", accentFg: "#282a36",
  danger: "#ff5555", success: "#50fa7b", warn: "#f1fa8c", info: "#8be9fd",
  selection: "#44475a",
}, {
  background: "#282a36", foreground: "#f8f8f2",
  cursor: "#bd93f9", selectionBackground: "#44475a",
  black: "#21222c", red: "#ff5555", green: "#50fa7b", yellow: "#f1fa8c",
  blue: "#bd93f9", magenta: "#ff79c6", cyan: "#8be9fd", white: "#f8f8f2",
});

const NORD = t("nord", "Nord", "dark", {
  bg: "#2e3440", bgPanel: "#3b4252", bgRaised: "#434c5e",
  fg: "#eceff4", fgMuted: "#a3acba",
  border: "#4c566a", borderStrong: "#5e81ac",
  accent: "#88c0d0", accentFg: "#2e3440",
  danger: "#bf616a", success: "#a3be8c", warn: "#ebcb8b", info: "#81a1c1",
  selection: "#434c5e",
}, {
  background: "#2e3440", foreground: "#eceff4",
  cursor: "#88c0d0", selectionBackground: "#434c5e",
  black: "#3b4252", red: "#bf616a", green: "#a3be8c", yellow: "#ebcb8b",
  blue: "#81a1c1", magenta: "#b48ead", cyan: "#88c0d0", white: "#e5e9f0",
});

const ONE_DARK = t("one-dark", "One Dark", "dark", {
  bg: "#282c34", bgPanel: "#21252b", bgRaised: "#2c313c",
  fg: "#abb2bf", fgMuted: "#5c6370",
  border: "#3e4451", borderStrong: "#4b5263",
  accent: "#61afef", accentFg: "#282c34",
  danger: "#e06c75", success: "#98c379", warn: "#e5c07b", info: "#56b6c2",
  selection: "#3e4451",
}, {
  background: "#282c34", foreground: "#abb2bf",
  cursor: "#61afef", selectionBackground: "#3e4451",
  black: "#21252b", red: "#e06c75", green: "#98c379", yellow: "#e5c07b",
  blue: "#61afef", magenta: "#c678dd", cyan: "#56b6c2", white: "#abb2bf",
});

const MONOKAI = t("monokai", "Monokai", "dark", {
  bg: "#272822", bgPanel: "#1e1f1a", bgRaised: "#3e3d32",
  fg: "#f8f8f2", fgMuted: "#75715e",
  border: "#3e3d32", borderStrong: "#75715e",
  accent: "#a6e22e", accentFg: "#272822",
  danger: "#f92672", success: "#a6e22e", warn: "#fd971f", info: "#66d9ef",
  selection: "#49483e",
}, {
  background: "#272822", foreground: "#f8f8f2",
  cursor: "#f8f8f2", selectionBackground: "#49483e",
  black: "#1e1f1a", red: "#f92672", green: "#a6e22e", yellow: "#fd971f",
  blue: "#66d9ef", magenta: "#ae81ff", cyan: "#a1efe4", white: "#f8f8f2",
});

const SOLARIZED_DARK = t("solarized-dark", "Solarized Dark", "dark", {
  bg: "#002b36", bgPanel: "#073642", bgRaised: "#0a4250",
  fg: "#93a1a1", fgMuted: "#586e75",
  border: "#073642", borderStrong: "#586e75",
  accent: "#268bd2", accentFg: "#002b36",
  danger: "#dc322f", success: "#859900", warn: "#b58900", info: "#2aa198",
  selection: "#073642",
}, {
  background: "#002b36", foreground: "#93a1a1",
  cursor: "#93a1a1", selectionBackground: "#073642",
  black: "#073642", red: "#dc322f", green: "#859900", yellow: "#b58900",
  blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#eee8d5",
});

const GRUVBOX_DARK = t("gruvbox-dark", "Gruvbox Dark", "dark", {
  bg: "#282828", bgPanel: "#1d2021", bgRaised: "#3c3836",
  fg: "#ebdbb2", fgMuted: "#928374",
  border: "#3c3836", borderStrong: "#504945",
  accent: "#fabd2f", accentFg: "#282828",
  danger: "#fb4934", success: "#b8bb26", warn: "#fabd2f", info: "#83a598",
  selection: "#504945",
}, {
  background: "#282828", foreground: "#ebdbb2",
  cursor: "#ebdbb2", selectionBackground: "#504945",
  black: "#1d2021", red: "#fb4934", green: "#b8bb26", yellow: "#fabd2f",
  blue: "#83a598", magenta: "#d3869b", cyan: "#8ec07c", white: "#ebdbb2",
});

const TOKYO_NIGHT = t("tokyo-night", "Tokyo Night", "dark", {
  bg: "#1a1b26", bgPanel: "#16161e", bgRaised: "#24283b",
  fg: "#a9b1d6", fgMuted: "#565f89",
  border: "#2f334d", borderStrong: "#414868",
  accent: "#7aa2f7", accentFg: "#1a1b26",
  danger: "#f7768e", success: "#9ece6a", warn: "#e0af68", info: "#7dcfff",
  selection: "#33467c",
}, {
  background: "#1a1b26", foreground: "#a9b1d6",
  cursor: "#7aa2f7", selectionBackground: "#33467c",
  black: "#16161e", red: "#f7768e", green: "#9ece6a", yellow: "#e0af68",
  blue: "#7aa2f7", magenta: "#bb9af7", cyan: "#7dcfff", white: "#a9b1d6",
});

const CATPPUCCIN_MOCHA = t("catppuccin-mocha", "Catppuccin Mocha", "dark", {
  bg: "#1e1e2e", bgPanel: "#181825", bgRaised: "#313244",
  fg: "#cdd6f4", fgMuted: "#a6adc8",
  border: "#313244", borderStrong: "#45475a",
  accent: "#cba6f7", accentFg: "#1e1e2e",
  danger: "#f38ba8", success: "#a6e3a1", warn: "#f9e2af", info: "#74c7ec",
  selection: "#45475a",
}, {
  background: "#1e1e2e", foreground: "#cdd6f4",
  cursor: "#f5e0dc", selectionBackground: "#45475a",
  black: "#181825", red: "#f38ba8", green: "#a6e3a1", yellow: "#f9e2af",
  blue: "#89b4fa", magenta: "#f5c2e7", cyan: "#94e2d5", white: "#cdd6f4",
});

const ROSE_PINE = t("rose-pine", "Rosé Pine", "dark", {
  bg: "#191724", bgPanel: "#1f1d2e", bgRaised: "#26233a",
  fg: "#e0def4", fgMuted: "#908caa",
  border: "#26233a", borderStrong: "#403d52",
  accent: "#c4a7e7", accentFg: "#191724",
  danger: "#eb6f92", success: "#9ccfd8", warn: "#f6c177", info: "#31748f",
  selection: "#403d52",
}, {
  background: "#191724", foreground: "#e0def4",
  cursor: "#c4a7e7", selectionBackground: "#403d52",
  black: "#26233a", red: "#eb6f92", green: "#31748f", yellow: "#f6c177",
  blue: "#9ccfd8", magenta: "#c4a7e7", cyan: "#ebbcba", white: "#e0def4",
});

const KANAGAWA = t("kanagawa", "Kanagawa", "dark", {
  bg: "#1f1f28", bgPanel: "#16161d", bgRaised: "#2a2a37",
  fg: "#dcd7ba", fgMuted: "#727169",
  border: "#2a2a37", borderStrong: "#54546d",
  accent: "#7e9cd8", accentFg: "#1f1f28",
  danger: "#c34043", success: "#76946a", warn: "#dca561", info: "#6a9589",
  selection: "#2d4f67",
}, {
  background: "#1f1f28", foreground: "#dcd7ba",
  cursor: "#c8c093", selectionBackground: "#2d4f67",
  black: "#16161d", red: "#c34043", green: "#76946a", yellow: "#dca561",
  blue: "#7e9cd8", magenta: "#957fb8", cyan: "#6a9589", white: "#dcd7ba",
});

const NIGHT_OWL = t("night-owl", "Night Owl", "dark", {
  bg: "#011627", bgPanel: "#01111d", bgRaised: "#0e293f",
  fg: "#d6deeb", fgMuted: "#8badc1",
  border: "#0e293f", borderStrong: "#5f7e97",
  accent: "#82aaff", accentFg: "#011627",
  danger: "#ef5350", success: "#22da6e", warn: "#ffeb95", info: "#7fdbca",
  selection: "#1d3b53",
}, {
  background: "#011627", foreground: "#d6deeb",
  cursor: "#80a4c2", selectionBackground: "#1d3b53",
  black: "#01111d", red: "#ef5350", green: "#22da6e", yellow: "#ffeb95",
  blue: "#82aaff", magenta: "#c792ea", cyan: "#7fdbca", white: "#d6deeb",
});

const PALENIGHT = t("palenight", "Palenight", "dark", {
  bg: "#292d3e", bgPanel: "#1b1f31", bgRaised: "#3a3f58",
  fg: "#a6accd", fgMuted: "#676e95",
  border: "#3a3f58", borderStrong: "#4a4f6b",
  accent: "#c792ea", accentFg: "#292d3e",
  danger: "#f07178", success: "#c3e88d", warn: "#ffcb6b", info: "#82aaff",
  selection: "#3a3f58",
}, {
  background: "#292d3e", foreground: "#a6accd",
  cursor: "#ffcc00", selectionBackground: "#3a3f58",
  black: "#1b1f31", red: "#f07178", green: "#c3e88d", yellow: "#ffcb6b",
  blue: "#82aaff", magenta: "#c792ea", cyan: "#89ddff", white: "#a6accd",
});

const SYNTHWAVE_84 = t("synthwave-84", "Synthwave '84", "dark", {
  bg: "#262335", bgPanel: "#1e1c2b", bgRaised: "#2c2640",
  fg: "#f4eeff", fgMuted: "#7e7791",
  border: "#34294f", borderStrong: "#5b3e7a",
  accent: "#ff7edb", accentFg: "#262335",
  danger: "#fe4450", success: "#72f1b8", warn: "#ffe261", info: "#36f9f6",
  selection: "#3a3056",
}, {
  background: "#262335", foreground: "#f4eeff",
  cursor: "#ff7edb", selectionBackground: "#3a3056",
  black: "#1e1c2b", red: "#fe4450", green: "#72f1b8", yellow: "#ffe261",
  blue: "#36f9f6", magenta: "#ff7edb", cyan: "#03edf9", white: "#f4eeff",
});

const CYBERPUNK = t("cyberpunk", "Cyberpunk", "dark", {
  bg: "#000b1e", bgPanel: "#001129", bgRaised: "#012444",
  fg: "#0abdc6", fgMuted: "#0a4a5a",
  border: "#012444", borderStrong: "#0a4a5a",
  accent: "#ea00d9", accentFg: "#000b1e",
  danger: "#ff003c", success: "#00ff9f", warn: "#fff200", info: "#00bfff",
  selection: "#063766",
}, {
  background: "#000b1e", foreground: "#0abdc6",
  cursor: "#ea00d9", selectionBackground: "#063766",
  black: "#001129", red: "#ff003c", green: "#00ff9f", yellow: "#fff200",
  blue: "#00bfff", magenta: "#ea00d9", cyan: "#0abdc6", white: "#d4d4d8",
});

const GITHUB_DARK = t("github-dark", "GitHub Dark", "dark", {
  bg: "#0d1117", bgPanel: "#010409", bgRaised: "#161b22",
  fg: "#c9d1d9", fgMuted: "#8b949e",
  border: "#21262d", borderStrong: "#30363d",
  accent: "#58a6ff", accentFg: "#0d1117",
  danger: "#f85149", success: "#3fb950", warn: "#d29922", info: "#79c0ff",
  selection: "#1f6feb",
}, {
  background: "#0d1117", foreground: "#c9d1d9",
  cursor: "#58a6ff", selectionBackground: "#1f6feb",
  black: "#010409", red: "#f85149", green: "#3fb950", yellow: "#d29922",
  blue: "#58a6ff", magenta: "#bc8cff", cyan: "#79c0ff", white: "#c9d1d9",
});

const AYU_DARK = t("ayu-dark", "Ayu Dark", "dark", {
  bg: "#0a0e14", bgPanel: "#01060e", bgRaised: "#0d1017",
  fg: "#b3b1ad", fgMuted: "#5c6773",
  border: "#1c1f24", borderStrong: "#33363b",
  accent: "#ffb454", accentFg: "#0a0e14",
  danger: "#f07178", success: "#c2d94c", warn: "#ff8f40", info: "#39bae6",
  selection: "#253340",
}, {
  background: "#0a0e14", foreground: "#b3b1ad",
  cursor: "#e6b450", selectionBackground: "#253340",
  black: "#01060e", red: "#f07178", green: "#c2d94c", yellow: "#ff8f40",
  blue: "#59c2ff", magenta: "#d2a6ff", cyan: "#39bae6", white: "#b3b1ad",
});

const HORIZON = t("horizon", "Horizon", "dark", {
  bg: "#1c1e26", bgPanel: "#16161c", bgRaised: "#232530",
  fg: "#e0e0e0", fgMuted: "#6f6e85",
  border: "#232530", borderStrong: "#3b3d4a",
  accent: "#e95678", accentFg: "#1c1e26",
  danger: "#e95678", success: "#29d398", warn: "#fab795", info: "#26bbd9",
  selection: "#3b3d4a",
}, {
  background: "#1c1e26", foreground: "#e0e0e0",
  cursor: "#e95678", selectionBackground: "#3b3d4a",
  black: "#16161c", red: "#e95678", green: "#29d398", yellow: "#fab795",
  blue: "#26bbd9", magenta: "#ee64ac", cyan: "#59e1e3", white: "#e0e0e0",
});

const MATERIAL_DEEP = t("material-deep", "Material Deep Ocean", "dark", {
  bg: "#0f111a", bgPanel: "#090b10", bgRaised: "#1a1c25",
  fg: "#a6accd", fgMuted: "#717cb4",
  border: "#1a1c25", borderStrong: "#3b3f56",
  accent: "#82aaff", accentFg: "#0f111a",
  danger: "#ff5370", success: "#c3e88d", warn: "#ffcb6b", info: "#89ddff",
  selection: "#1f2233",
}, {
  background: "#0f111a", foreground: "#a6accd",
  cursor: "#82aaff", selectionBackground: "#1f2233",
  black: "#090b10", red: "#ff5370", green: "#c3e88d", yellow: "#ffcb6b",
  blue: "#82aaff", magenta: "#c792ea", cyan: "#89ddff", white: "#a6accd",
});

const HACKER = t("hacker", "Hacker", "dark", {
  bg: "#000000", bgPanel: "#020602", bgRaised: "#0a140a",
  fg: "#33ff33", fgMuted: "#1c8a1c",
  border: "#0a140a", borderStrong: "#155515",
  accent: "#33ff33", accentFg: "#000000",
  danger: "#ff5555", success: "#55ff55", warn: "#ffff55", info: "#55ffff",
  selection: "#0d3a0d",
}, {
  background: "#000000", foreground: "#33ff33",
  cursor: "#33ff33", selectionBackground: "#0d3a0d",
  black: "#000000", red: "#ff5555", green: "#33ff33", yellow: "#ffff55",
  blue: "#55ffff", magenta: "#ff55ff", cyan: "#55ffff", white: "#bbffbb",
});

// ─── Light ──────────────────────────────────────────────
const PAPER = t("paper", "Paper", "light", {
  bg: "#fafafa", bgPanel: "#ffffff", bgRaised: "#f1f3f5",
  fg: "#1d2026", fgMuted: "#5c6270",
  border: "#dde2ea", borderStrong: "#c5ccd6",
  accent: "#3367d6", accentFg: "#ffffff",
  danger: "#c63838", success: "#1f8c2c", warn: "#a06200", info: "#0967a3",
  selection: "#cde2f8",
}, {
  background: "#fafafa", foreground: "#1d2026",
  cursor: "#3367d6", selectionBackground: "#cde2f8",
  black: "#1d2026", red: "#c63838", green: "#1f8c2c", yellow: "#a06200",
  blue: "#3367d6", magenta: "#a64ca6", cyan: "#0a8fa6", white: "#fafafa",
});

const SOLARIZED_LIGHT = t("solarized-light", "Solarized Light", "light", {
  bg: "#fdf6e3", bgPanel: "#eee8d5", bgRaised: "#ece4cf",
  fg: "#586e75", fgMuted: "#93a1a1",
  border: "#eee8d5", borderStrong: "#93a1a1",
  accent: "#268bd2", accentFg: "#fdf6e3",
  danger: "#dc322f", success: "#859900", warn: "#b58900", info: "#2aa198",
  selection: "#eee8d5",
}, {
  background: "#fdf6e3", foreground: "#586e75",
  cursor: "#586e75", selectionBackground: "#eee8d5",
  black: "#073642", red: "#dc322f", green: "#859900", yellow: "#b58900",
  blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#fdf6e3",
});

const GITHUB_LIGHT = t("github-light", "GitHub Light", "light", {
  bg: "#ffffff", bgPanel: "#f6f8fa", bgRaised: "#eaeef2",
  fg: "#1f2328", fgMuted: "#656d76",
  border: "#d0d7de", borderStrong: "#afb8c1",
  accent: "#0969da", accentFg: "#ffffff",
  danger: "#cf222e", success: "#1a7f37", warn: "#9a6700", info: "#0969da",
  selection: "#cae5ff",
}, {
  background: "#ffffff", foreground: "#1f2328",
  cursor: "#0969da", selectionBackground: "#cae5ff",
  black: "#24292f", red: "#cf222e", green: "#1a7f37", yellow: "#9a6700",
  blue: "#0969da", magenta: "#8250df", cyan: "#1f6feb", white: "#1f2328",
});

const ATOM_LIGHT = t("atom-light", "Atom Light", "light", {
  bg: "#fafafa", bgPanel: "#f0f0f0", bgRaised: "#e8e8e8",
  fg: "#383a42", fgMuted: "#a0a1a7",
  border: "#dadce0", borderStrong: "#a0a1a7",
  accent: "#4078f2", accentFg: "#fafafa",
  danger: "#e45649", success: "#50a14f", warn: "#c18401", info: "#0184bc",
  selection: "#dde2eb",
}, {
  background: "#fafafa", foreground: "#383a42",
  cursor: "#4078f2", selectionBackground: "#dde2eb",
  black: "#383a42", red: "#e45649", green: "#50a14f", yellow: "#c18401",
  blue: "#4078f2", magenta: "#a626a4", cyan: "#0184bc", white: "#fafafa",
});

const ROSE_PINE_DAWN = t("rose-pine-dawn", "Rosé Pine Dawn", "light", {
  bg: "#faf4ed", bgPanel: "#fffaf3", bgRaised: "#f2e9e1",
  fg: "#575279", fgMuted: "#9893a5",
  border: "#dfdad9", borderStrong: "#cecacd",
  accent: "#907aa9", accentFg: "#faf4ed",
  danger: "#b4637a", success: "#286983", warn: "#ea9d34", info: "#56949f",
  selection: "#dfdad9",
}, {
  background: "#faf4ed", foreground: "#575279",
  cursor: "#907aa9", selectionBackground: "#dfdad9",
  black: "#f2e9e1", red: "#b4637a", green: "#286983", yellow: "#ea9d34",
  blue: "#56949f", magenta: "#907aa9", cyan: "#d7827e", white: "#575279",
});

const BRIDGEMIND = t("bridgemind", "Bridgemind", "dark", {
  bg: "#121212", bgPanel: "#161616", bgRaised: "#1E1E1E",
  fg: "#E0E0E0", fgMuted: "#8A8A8A",
  border: "#2A2A2A", borderStrong: "#3A3A3A",
  accent: "#E89A4A", accentFg: "#121212",
  danger: "#EF4444", success: "#4ADE80", warn: "#E89A4A", info: "#60A5FA",
  selection: "#1E3A5F",
}, {
  background: "#121212", foreground: "#E0E0E0",
  cursor: "#E89A4A", selectionBackground: "#1E3A5F",
  black: "#161616", red: "#EF4444", green: "#4ADE80", yellow: "#E89A4A",
  blue: "#60A5FA", magenta: "#C084FC", cyan: "#22D3EE", white: "#E0E0E0",
});

const HIGH_CONTRAST = t("high-contrast", "High Contrast", "dark", {
  bg: "#000000", bgPanel: "#000000", bgRaised: "#0d0d0d",
  fg: "#ffffff", fgMuted: "#cccccc",
  border: "#444444", borderStrong: "#888888",
  accent: "#ffff00", accentFg: "#000000",
  danger: "#ff0000", success: "#00ff00", warn: "#ffaa00", info: "#00ffff",
  selection: "#3a3d41",
}, {
  background: "#000000", foreground: "#ffffff",
  cursor: "#ffff00", selectionBackground: "#3a3d41",
  black: "#000000", red: "#ff0000", green: "#00ff00", yellow: "#ffff00",
  blue: "#00aaff", magenta: "#ff00ff", cyan: "#00ffff", white: "#ffffff",
});

export const THEMES: Theme[] = [
  VOID,
  NEON_TOKYO,
  DRACULA,
  NORD,
  ONE_DARK,
  MONOKAI,
  SOLARIZED_DARK,
  GRUVBOX_DARK,
  TOKYO_NIGHT,
  CATPPUCCIN_MOCHA,
  ROSE_PINE,
  KANAGAWA,
  NIGHT_OWL,
  PALENIGHT,
  SYNTHWAVE_84,
  CYBERPUNK,
  GITHUB_DARK,
  AYU_DARK,
  HORIZON,
  MATERIAL_DEEP,
  HACKER,
  BRIDGEMIND,
  PAPER,
  SOLARIZED_LIGHT,
  GITHUB_LIGHT,
  ATOM_LIGHT,
  ROSE_PINE_DAWN,
  HIGH_CONTRAST,
];

export const DEFAULT_THEME_ID = BRIDGEMIND.id;

export function getTheme(id: string | null | undefined): Theme {
  if (!id) return VOID;
  return THEMES.find((t) => t.id === id) ?? VOID;
}

/**
 * Apply CSS variables of `theme` to `document.documentElement`. xterm
 * tiles read the theme via `useTheme().xterm` and rebuild themselves.
 *
 * Beyond the raw palette, this also derives a small set of secondary
 * tokens used across components — soft accent/status fills, focus ring,
 * scrollbar thumb, hover surfaces, and an overlay tint — so component
 * CSS only needs to reference variables and never hardcodes per-theme
 * colours.
 */
export function applyThemeToDom(theme: Theme): void {
  const root = document.documentElement;
  root.dataset.theme = theme.id;
  root.dataset.themeKind = theme.kind;
  const c = theme.css;

  const set = (name: string, value: string) =>
    root.style.setProperty(name, value);

  // ── Raw palette ─────────────────────────────────────────
  set("--bg", c.bg);
  set("--bg-panel", c.bgPanel);
  set("--bg-raised", c.bgRaised);
  set("--fg", c.fg);
  set("--fg-muted", c.fgMuted);
  set("--border", c.border);
  set("--border-strong", c.borderStrong);
  set("--accent", c.accent);
  set("--accent-fg", c.accentFg);
  set("--danger", c.danger);
  set("--success", c.success);
  set("--warn", c.warn);
  set("--info", c.info);
  set("--selection", c.selection);

  // ── Derived: soft fills for chips/badges/status backgrounds ─
  const accentRgb = hexToRgb(c.accent);
  const dangerRgb = hexToRgb(c.danger);
  const successRgb = hexToRgb(c.success);
  const warnRgb = hexToRgb(c.warn);
  const infoRgb = hexToRgb(c.info);

  set("--accent-soft", rgba(accentRgb, 0.16));
  set("--accent-strong", rgba(accentRgb, 0.55));
  set("--danger-soft", rgba(dangerRgb, 0.16));
  set("--success-soft", rgba(successRgb, 0.16));
  set("--warn-soft", rgba(warnRgb, 0.16));
  set("--info-soft", rgba(infoRgb, 0.16));

  // ── Derived: focus ring (accent at ~45% alpha) ──────────
  set("--ring", rgba(accentRgb, 0.45));

  // ── Derived: hover/active surface mixers ────────────────
  // Use a translucent layer in the foreground colour so it works on any
  // theme without picking another raw colour. Keep alpha small so it
  // stacks gracefully on already-darker rows.
  const fgRgb = hexToRgb(c.fg);
  const isLight = theme.kind === "light";
  const hoverAlpha = isLight ? 0.045 : 0.05;
  const hoverStrongAlpha = isLight ? 0.075 : 0.08;
  const activeAlpha = isLight ? 0.11 : 0.11;
  set("--hover", rgba(fgRgb, hoverAlpha));
  set("--hover-strong", rgba(fgRgb, hoverStrongAlpha));
  set("--active", rgba(fgRgb, activeAlpha));

  // Scrollbar thumb tracks foreground so it works on light + dark.
  set("--scrollbar-thumb", rgba(fgRgb, isLight ? 0.22 : 0.16));
  set("--scrollbar-thumb-hover", rgba(fgRgb, isLight ? 0.36 : 0.28));

  // Backdrop tint on top of <body bg>. Lighter on light themes.
  const bgRgb = hexToRgb(c.bg);
  set("--bg-overlay", rgba(bgRgb, isLight ? 0.55 : 0.7));

  // Subtle on top of fgMuted for "even quieter" labels.
  const fgMutedRgb = hexToRgb(c.fgMuted);
  // 75% mix toward bg for a softer caption colour.
  const subtle = mix(fgMutedRgb, bgRgb, 0.45);
  set("--fg-subtle", rgbToCss(subtle));
}

// ─── tiny colour helpers (no external deps) ──────────────
function hexToRgb(hex: string): [number, number, number] {
  let h = hex.trim().replace(/^#/, "");
  if (h.length === 3) {
    h = h
      .split("")
      .map((ch) => ch + ch)
      .join("");
  }
  const n = parseInt(h, 16);
  if (Number.isNaN(n) || h.length !== 6) return [128, 128, 128];
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function rgba([r, g, b]: [number, number, number], a: number): string {
  return `rgba(${r}, ${g}, ${b}, ${a})`;
}

function rgbToCss([r, g, b]: [number, number, number]): string {
  return `rgb(${r}, ${g}, ${b})`;
}

// Linear-mix `a` toward `b` by t ∈ [0..1].
function mix(
  a: [number, number, number],
  b: [number, number, number],
  t: number,
): [number, number, number] {
  return [
    Math.round(a[0] * (1 - t) + b[0] * t),
    Math.round(a[1] * (1 - t) + b[1] * t),
    Math.round(a[2] * (1 - t) + b[2] * t),
  ];
}
