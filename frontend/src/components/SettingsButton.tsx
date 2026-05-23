import { useCallback, useEffect, useRef, useState } from "react";
import { OrchestratorPanel } from "./OrchestratorPanel";
import { MemoryPanel } from "./MemoryPanel";
import { SkillsPanel } from "./SkillsPanel";
import { VoicePanel } from "./voice/VoicePanel";
import { BrowserPanel } from "./BrowserPanel";
import { FilesPanel } from "./FilesPanel";
import { PromptsPanel } from "./PromptsPanel";
import { AgentConfigPanel } from "./AgentConfigPanel";
import { SshPresetsPanel } from "./SshPresetsPanel";
import { ArchitectPanel } from "./ArchitectPanel";
import { TransmissionLog } from "./TransmissionLog";

const STORAGE_KEY = "pigide-settings-last-panel";

type PanelId =
  | "chat"
  | "architect"
  | "memory"
  | "skills"
  | "voice"
  | "browser"
  | "files"
  | "prompts"
  | "agents"
  | "ssh"
  | "transmission";

interface PanelDef {
  id: PanelId;
  label: string;
}

const PANELS: PanelDef[] = [
  { id: "architect", label: "Architect" },
  { id: "memory", label: "Memory" },
  { id: "skills", label: "Skills" },
  { id: "voice", label: "Voice" },
  { id: "browser", label: "Web" },
  { id: "files", label: "Files" },
  { id: "prompts", label: "Prompts" },
  { id: "agents", label: "Agents" },
  { id: "ssh", label: "SSH" },
  { id: "transmission", label: "Transmission Log" },
];

function getLastPanel(): PanelId | null {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v && PANELS.some((p) => p.id === v)) return v as PanelId;
  } catch {}
  return null;
}

function GearIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

export function SettingsButton() {
  const [menuOpen, setMenuOpen] = useState(false);
  const [activePanel, setActivePanel] = useState<PanelId | null>(null);
  const [focusIdx, setFocusIdx] = useState(-1);
  const btnRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const itemsRef = useRef<(HTMLButtonElement | null)[]>([]);

  const lastPanel = getLastPanel();

  const openMenu = useCallback(() => {
    setMenuOpen(true);
    setFocusIdx(0);
  }, []);

  const closeMenu = useCallback(() => {
    setMenuOpen(false);
    setFocusIdx(-1);
    btnRef.current?.focus();
  }, []);

  const selectPanel = useCallback((id: PanelId) => {
    setActivePanel(id);
    setMenuOpen(false);
    setFocusIdx(-1);
    try {
      localStorage.setItem(STORAGE_KEY, id);
    } catch {}
  }, []);

  const closePanel = useCallback(() => {
    setActivePanel(null);
    btnRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!menuOpen) return;
    const item = itemsRef.current[focusIdx];
    item?.focus();
  }, [focusIdx, menuOpen]);

  useEffect(() => {
    if (!menuOpen) return;
    function handleClick(e: MouseEvent) {
      if (
        menuRef.current &&
        !menuRef.current.contains(e.target as Node) &&
        btnRef.current &&
        !btnRef.current.contains(e.target as Node)
      ) {
        closeMenu();
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [menuOpen, closeMenu]);

  const handleMenuKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case "Escape":
          e.preventDefault();
          closeMenu();
          break;
        case "ArrowDown":
          e.preventDefault();
          setFocusIdx((i) => (i + 1) % PANELS.length);
          break;
        case "ArrowUp":
          e.preventDefault();
          setFocusIdx((i) => (i - 1 + PANELS.length) % PANELS.length);
          break;
        case "Home":
          e.preventDefault();
          setFocusIdx(0);
          break;
        case "End":
          e.preventDefault();
          setFocusIdx(PANELS.length - 1);
          break;
        case "Tab":
          e.preventDefault();
          closeMenu();
          break;
      }
    },
    [closeMenu]
  );

  const handleBtnKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowUp" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        if (!menuOpen) openMenu();
      }
    },
    [menuOpen, openMenu]
  );

  const handlePanelKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closePanel();
      } else if (e.key === "Tab") {
        const el = panelRef.current;
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
    },
    [closePanel]
  );

  useEffect(() => {
    if (!activePanel) return;
    const el = panelRef.current;
    if (!el) return;
    const focusable = el.querySelector<HTMLElement>(
      'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    focusable?.focus();
  }, [activePanel]);

  return (
    <>
      <button
        ref={btnRef}
        className="settings-trigger btn--icon"
        aria-label="Settings"
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={() => (menuOpen ? closeMenu() : openMenu())}
        onKeyDown={handleBtnKeyDown}
      >
        <GearIcon />
      </button>

      {menuOpen && (
        <div
          ref={menuRef}
          className="settings-popover"
          role="menu"
          aria-label="Settings panels"
          onKeyDown={handleMenuKeyDown}
        >
          {PANELS.map((p, i) => (
            <button
              key={p.id}
              ref={(el) => { itemsRef.current[i] = el; }}
              role="menuitem"
              className={`settings-menu-item ${p.id === lastPanel ? "last-used" : ""}`}
              tabIndex={focusIdx === i ? 0 : -1}
              onClick={() => selectPanel(p.id)}
            >
              {p.label}
            </button>
          ))}
        </div>
      )}

      {activePanel && (
        <div
          ref={panelRef}
          className="settings-panel-overlay"
          role="dialog"
          aria-modal="true"
          aria-labelledby="settings-panel-title"
          onKeyDown={handlePanelKeyDown}
        >
          <div className="settings-panel-overlay__header">
            <span className="settings-panel-overlay__title" id="settings-panel-title">
              {PANELS.find((p) => p.id === activePanel)?.label}
            </span>
            <button
              className="settings-panel-overlay__close"
              onClick={closePanel}
              aria-label="Close panel"
            >
              ×
            </button>
          </div>
          <div className="settings-panel-overlay__body">
            {activePanel === "chat" && <OrchestratorPanel />}
            {activePanel === "architect" && <ArchitectPanel />}
            {activePanel === "memory" && <MemoryPanel />}
            {activePanel === "skills" && <SkillsPanel />}
            {activePanel === "voice" && <VoicePanel />}
            {activePanel === "browser" && <BrowserPanel />}
            {activePanel === "files" && <FilesPanel />}
            {activePanel === "prompts" && <PromptsPanel />}
            {activePanel === "agents" && <AgentConfigPanel />}
            {activePanel === "ssh" && <SshPresetsPanel />}
            {activePanel === "transmission" && <TransmissionLog />}
          </div>
        </div>
      )}
    </>
  );
}
