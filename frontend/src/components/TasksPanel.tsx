import { useState } from "react";

export interface TaskRow {
  id: string;
  title: string;
  status: "todo" | "in_progress" | "done";
}

interface Props {
  tasks?: TaskRow[];
}

export function TasksPanel({ tasks = [] }: Props) {
  const [collapsed, setCollapsed] = useState(false);

  return (
    <div className="tasks-panel">
      <button
        className="tasks-panel__header"
        onClick={() => setCollapsed(!collapsed)}
      >
        <span className="tasks-panel__title">Tasks</span>
        <span className="tasks-panel__chevron">{collapsed ? "▸" : "▾"}</span>
      </button>
      {!collapsed && (
        <div className="tasks-panel__body">
          {tasks.length === 0 ? (
            <div className="tasks-panel__empty">
              <span className="tasks-panel__empty-bold">No tasks yet</span>
              <span className="tasks-panel__empty-hint">
                Ask Bridge to plan something — &lsquo;help me wire up the auth
                refactor&rsquo;, &lsquo;make a list of the bugs in the swarm
                runner&rsquo;...
              </span>
            </div>
          ) : (
            <div className="tasks-panel__list">
              {tasks.map((t) => (
                <div key={t.id} className="tasks-panel__row">
                  <span
                    className={`tasks-panel__dot tasks-panel__dot--${t.status}`}
                  />
                  <span className="tasks-panel__row-title">{t.title}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
