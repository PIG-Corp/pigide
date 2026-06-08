import { useEffect, useMemo, useRef, useState } from "react";
import {
  ipc,
  onSkillsError,
  onSkillsReloaded,
  type ClaudeImportReport,
  type SkillFull,
  type SkillsTraceRow,
  type SkillView,
} from "../state/ipc";
import { useStore } from "../state/store";

type Tab = "list" | "trace";

/**
 * SkillsPanel — manage the orchestrator's prompt-skills.
 *
 * Three things to surface:
 *   1) The list of discovered skills (with source + shadowing + enable toggle).
 *   2) An inspector for the selected skill (read-only; users edit via the file).
 *   3) A "Last Turn" trace so users can see what fired and what didn't.
 */
export function SkillsPanel() {
  const pushToast = useStore((s) => s.pushToast);

  const [tab, setTab] = useState<Tab>("list");
  const [list, setList] = useState<SkillView[]>([]);
  const [openId, setOpenId] = useState<string | null>(null);
  const [open, setOpen] = useState<SkillFull | null>(null);
  const [trace, setTrace] = useState<SkillsTraceRow | null>(null);
  const [createId, setCreateId] = useState("");
  const [createName, setCreateName] = useState("");
  const [importing, setImporting] = useState(false);
  const [lastImport, setLastImport] = useState<ClaudeImportReport | null>(null);
  const pendingToggles = useRef<Set<string>>(new Set());

  async function refresh() {
    try {
      setList(await ipc.listSkills());
    } catch (e) {
      pushToast({ kind: "error", text: String(e) });
    }
  }

  async function refreshTrace() {
    try {
      setTrace(await ipc.lastSkillsTrace());
    } catch (e) {
      pushToast({ kind: "error", text: String(e) });
    }
  }

  useEffect(() => {
    // B-1.2: hold a `dead` flag so a listener that resolves AFTER unmount
    // (e.g. StrictMode double-mount) gets unlistened immediately instead
    // of being silently leaked.
    let dead = false;
    const unsubs: (() => void)[] = [];
    // B-11.2: reset transient UI state on mount so a remount with stale
    // local draft doesn't show up. (Skills list is global, not workspace-
    // scoped, so this is mostly about closing the inspector + clearing
    // the create form.)
    setOpenId(null);
    setCreateId("");
    setCreateName("");
    setTab("list");
    refresh();
    refreshTrace();
    onSkillsReloaded(() => {
      if (dead) return;
      refresh();
    }).then((u) => {
      if (dead) u();
      else unsubs.push(u);
    });
    onSkillsError((e) => {
      if (dead) return;
      pushToast({ kind: "error", text: `Skill error: ${e.error}` });
      refresh();
    }).then((u) => {
      if (dead) u();
      else unsubs.push(u);
    });
    return () => {
      dead = true;
      unsubs.forEach((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!openId) {
      setOpen(null);
      return;
    }
    ipc.getSkill(openId).then(setOpen).catch((e) => {
      pushToast({ kind: "error", text: String(e) });
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [openId]);

  const grouped = useMemo(() => {
    const winners = list.filter((s) => !s.shadowed_by);
    const shadowed = list.filter((s) => s.shadowed_by);
    return { winners, shadowed };
  }, [list]);

  async function toggle(s: SkillView) {
    if (pendingToggles.current.has(s.id)) return;
    const wantEnabled = !(s.enabled && !s.override_disabled);
    pendingToggles.current.add(s.id);
    try {
      await ipc.setSkillEnabled(s.id, wantEnabled);
      await refresh();
    } catch (e) {
      pushToast({ kind: "error", text: String(e) });
    } finally {
      pendingToggles.current.delete(s.id);
    }
  }

  async function reload() {
    try {
      const n = await ipc.reloadSkills();
      pushToast({ kind: "info", text: `Reloaded ${n} skills` });
      await refresh();
    } catch (e) {
      pushToast({ kind: "error", text: String(e) });
    }
  }

  async function importClaude() {
    if (importing) return;
    setImporting(true);
    try {
      const report = await ipc.importClaudeSkills();
      setLastImport(report);
      const total = report.created + report.updated + report.unchanged;
      pushToast({
        kind: "info",
        text: `Claude import: ${report.created} new, ${report.updated} updated, ${report.unchanged} unchanged${
          report.failed ? `, ${report.failed} failed` : ""
        } (${total} total)`,
      });
      await refresh();
    } catch (e) {
      pushToast({ kind: "error", text: `Import failed: ${e}` });
    } finally {
      setImporting(false);
    }
  }

  async function createStub() {
    const id = createId.trim();
    const name = createName.trim() || id;
    if (!id) return;
    try {
      const path = await ipc.createUserSkill(id, name);
      pushToast({ kind: "info", text: `Created ${path}` });
      setCreateId("");
      setCreateName("");
      await refresh();
    } catch (e) {
      pushToast({ kind: "error", text: String(e) });
    }
  }

  return (
    <div className="skills-panel">
      <header className="skills-panel-header">
        <div className="skills-tabs">
          <button
            className={`skills-tab ${tab === "list" ? "active" : ""}`}
            onClick={() => setTab("list")}
          >
            Skills
          </button>
          <button
            className={`skills-tab ${tab === "trace" ? "active" : ""}`}
            onClick={() => {
              setTab("trace");
              refreshTrace();
            }}
          >
            Last turn
          </button>
        </div>
        <button className="skills-reload" onClick={reload} title="Force rescan">
          Reload
        </button>
        <button
          className="skills-reload"
          onClick={importClaude}
          disabled={importing}
          title="Scan ~/.claude and import skills into PigIDE"
        >
          {importing ? "Importing…" : "Import Claude Skills"}
        </button>
      </header>

      {tab === "list" ? (
        <div className="skills-list-wrap">
          <div className="skills-list">
            {grouped.winners.length === 0 ? (
              <div className="skills-empty">
                No skills discovered. Drop a `.md` file with frontmatter into
                <code>~/.pigide/skills/</code> or your workspace's
                <code>.pigide/skills/</code>.
              </div>
            ) : null}
            {grouped.winners.map((s) => (
              <button
                key={`${s.source}-${s.id}`}
                className={`skills-row ${openId === s.id ? "active" : ""}`}
                onClick={() => setOpenId(s.id)}
              >
                <div className="skills-row-top">
                  <strong>{s.name}</strong>
                  <span className={`skills-tag src-${s.source}`}>{s.source}</span>
                  <span className="skills-priority">p{s.priority}</span>
                  <span
                    className={`skills-state ${
                      s.enabled && !s.override_disabled ? "on" : "off"
                    }`}
                    onClick={(e) => {
                      e.stopPropagation();
                      toggle(s);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        e.stopPropagation();
                        toggle(s);
                      }
                    }}
                    role="switch"
                    aria-checked={s.enabled && !s.override_disabled}
                    tabIndex={0}
                    title="Click to toggle"
                  >
                    {s.enabled && !s.override_disabled ? "ON" : "OFF"}
                  </span>
                </div>
                <div className="skills-row-desc">{s.description}</div>
                <div className="skills-row-meta">
                  {s.tags.map((t) => (
                    <span key={`tag-${t}`} className="skills-chip tag">
                      {t}
                    </span>
                  ))}
                  {s.triggers.slice(0, 4).map((t) => (
                    <span key={`trig-${t}`} className="skills-chip trig">
                      {t}
                    </span>
                  ))}
                </div>
              </button>
            ))}

            {grouped.shadowed.length > 0 ? (
              <details className="skills-shadowed">
                <summary>{grouped.shadowed.length} shadowed</summary>
                {grouped.shadowed.map((s) => (
                  <div key={`sh-${s.source}-${s.id}`} className="skills-row shadowed">
                    <div className="skills-row-top">
                      <strong>{s.name}</strong>
                      <span className={`skills-tag src-${s.source}`}>{s.source}</span>
                      <span className="skills-shadow-by">
                        shadowed by {s.shadowed_by}
                      </span>
                    </div>
                    <div className="skills-row-desc">{s.path}</div>
                  </div>
                ))}
              </details>
            ) : null}

            <div className="skills-create">
              <h4>Create a user skill</h4>
              <input
                placeholder="my-skill-id"
                value={createId}
                onChange={(e) => setCreateId(e.target.value)}
              />
              <input
                placeholder="Display name"
                value={createName}
                onChange={(e) => setCreateName(e.target.value)}
              />
              <button onClick={createStub}>Create stub</button>
              <small>
                Writes a starter <code>.md</code> to
                <code>~/.pigide/skills/</code>. Edit in your editor; reloads
                automatically.
              </small>
            </div>

            {lastImport ? (
              <details className="skills-import-report" open>
                <summary>
                  Last Claude import: {lastImport.created} new ·{" "}
                  {lastImport.updated} updated · {lastImport.unchanged} unchanged
                  {lastImport.failed ? ` · ${lastImport.failed} failed` : ""}
                </summary>
                <div className="skills-import-dest">
                  → <code>{lastImport.destination}</code>
                </div>
                <ul className="skills-import-roots">
                  {lastImport.roots.map((r) => (
                    <li key={r.path}>
                      <span className={`skills-tag src-${r.exists ? "user" : "off"}`}>
                        {r.label}
                      </span>{" "}
                      <code>{r.path}</code>{" "}
                      {r.exists ? `· ${r.skill_count} skills` : "· (missing)"}
                    </li>
                  ))}
                </ul>
                <ul className="skills-import-list">
                  {lastImport.imported
                    .filter((i) => i.status !== "unchanged")
                    .slice(0, 50)
                    .map((i) => (
                      <li key={`${i.id}-${i.source_path}`}>
                        <span className={`skills-import-status ${i.status}`}>
                          {i.status}
                        </span>{" "}
                        <strong>{i.id || i.name}</strong>
                        {i.warnings.length > 0 ? (
                          <span className="skills-import-warn">
                            {" "}
                            ⚠ {i.warnings.join("; ")}
                          </span>
                        ) : null}
                      </li>
                    ))}
                </ul>
              </details>
            ) : null}
          </div>

          <div className="skills-detail">
            {open ? (
              <SkillDetail full={open} />
            ) : (
              <div className="skills-detail-empty">
                Pick a skill to inspect its body.
              </div>
            )}
          </div>
        </div>
      ) : (
        <SkillsTrace trace={trace} onRefresh={refreshTrace} />
      )}
    </div>
  );
}

function SkillDetail({ full }: { full: SkillFull }) {
  return (
    <div className="skills-detail-body">
      <header>
        <h3>{full.name}</h3>
        <code>{full.id}</code>
      </header>
      <p className="skills-detail-desc">{full.description}</p>
      <dl>
        <dt>Source</dt>
        <dd>
          <code>{full.path}</code>
        </dd>
        <dt>Priority</dt>
        <dd>{full.priority}</dd>
        <dt>Tags</dt>
        <dd>{full.tags.join(", ") || "—"}</dd>
        <dt>Triggers</dt>
        <dd>{full.triggers.join(", ") || "—"}</dd>
        <dt>Digest</dt>
        <dd>
          <code>{full.digest}</code>
        </dd>
      </dl>
      <h4>Body</h4>
      <pre className="skills-body">{full.body}</pre>
    </div>
  );
}

function SkillsTrace({
  trace,
  onRefresh,
}: {
  trace: SkillsTraceRow | null;
  onRefresh: () => void;
}) {
  if (!trace) {
    return (
      <div className="skills-trace-empty">
        No turns yet. Send a message to the Architect first.
        <button onClick={onRefresh} style={{ marginLeft: 8 }}>
          Refresh
        </button>
      </div>
    );
  }
  return (
    <div className="skills-trace">
      <header>
        <span>{new Date(trace.turn_at).toLocaleString()}</span>
        <span>composed: {trace.composed_chars} chars</span>
        <span>fallback: {trace.fallback_used ? "yes" : "no"}</span>
        <button onClick={onRefresh}>Refresh</button>
      </header>
      <h4>Selected</h4>
      <ul className="skills-trace-list">
        {trace.selected.length === 0 ? <li>(none)</li> : null}
        {trace.selected.map((s) => (
          <li key={`sel-${s.id}`}>
            <strong>{s.id}</strong>
            <span className="trace-score">score {s.score.toFixed(2)}</span>
            <div className="trace-reasons">{s.reasons.join(" · ")}</div>
          </li>
        ))}
      </ul>
      <h4>Rejected</h4>
      <ul className="skills-trace-list dim">
        {trace.rejected.length === 0 ? <li>(none)</li> : null}
        {trace.rejected.map((s) => (
          <li key={`rej-${s.id}`}>
            <strong>{s.id}</strong>
            <span className="trace-score">score {s.score.toFixed(2)}</span>
            <div className="trace-reasons">{s.reasons.join(" · ") || "—"}</div>
          </li>
        ))}
      </ul>
    </div>
  );
}
