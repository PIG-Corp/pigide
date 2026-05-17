import { useEffect, useMemo, useState } from "react";
import { useStore } from "../state/store";
import { ipc } from "../state/ipc";
import type { Task, TaskStatus } from "../state/types";
import { X } from "./icons";

const COLUMNS: { status: TaskStatus; label: string }[] = [
  { status: "todo", label: "Todo" },
  { status: "in_progress", label: "In progress" },
  { status: "in_review", label: "In review" },
  { status: "complete", label: "Complete" },
];

export function KanbanBoard() {
  const currentId = useStore((s) => s.currentId);
  const tasks = useStore((s) => s.tasks);
  const agents = useStore((s) => s.agents);
  const setTasks = useStore((s) => s.setTasks);
  const upsertTask = useStore((s) => s.upsertTask);
  const removeTask = useStore((s) => s.removeTask);
  const setShowKanban = useStore((s) => s.setShowKanban);
  const pushToast = useStore((s) => s.pushToast);

  const [creating, setCreating] = useState(false);
  const [draftTitle, setDraftTitle] = useState("");
  const [draftInstructions, setDraftInstructions] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);

  // Initial load + reload on workspace switch.
  useEffect(() => {
    if (!currentId) {
      setTasks([]);
      return;
    }
    ipc
      .listTasks({ workspace_id: currentId })
      .then(setTasks)
      .catch((err) => pushToast({ text: `list_tasks: ${err}`, kind: "error" }));
  }, [currentId, setTasks, pushToast]);

  const taskList = useMemo(
    () =>
      Object.values(tasks)
        .filter((t) => t.workspace_id === currentId)
        .sort((a, b) => a.created_at.localeCompare(b.created_at)),
    [tasks, currentId],
  );

  const tasksByStatus = useMemo(() => {
    const out: Record<TaskStatus, Task[]> = {
      todo: [],
      in_progress: [],
      in_review: [],
      complete: [],
      cancelled: [],
    };
    for (const t of taskList) out[t.status as TaskStatus]?.push(t);
    return out;
  }, [taskList]);

  const create = async () => {
    if (!currentId || !draftTitle.trim()) return;
    try {
      const t = await ipc.createTask({
        workspace_id: currentId,
        title: draftTitle.trim(),
        instructions: draftInstructions.trim(),
      });
      upsertTask(t);
      setDraftTitle("");
      setDraftInstructions("");
      setCreating(false);
    } catch (err) {
      pushToast({ text: `create_task: ${err}`, kind: "error" });
    }
  };

  const move = async (taskId: string, status: TaskStatus) => {
    const cur = tasks[taskId];
    if (!cur || cur.status === status) return;
    try {
      const t = await ipc.updateTask({ id: taskId, status });
      upsertTask(t);
    } catch (err) {
      pushToast({ text: `update_task: ${err}`, kind: "error" });
    }
  };

  const remove = async (taskId: string) => {
    if (!confirm("Удалить задачу?")) return;
    try {
      await ipc.deleteTask(taskId);
      removeTask(taskId);
    } catch (err) {
      pushToast({ text: `delete_task: ${err}`, kind: "error" });
    }
  };

  const assignToAgent = async (taskId: string, agentId: string | null) => {
    try {
      const t = await ipc.assignTask(taskId, agentId);
      upsertTask(t);
    } catch (err) {
      pushToast({ text: `assign_task: ${err}`, kind: "error" });
    }
  };

  return (
    <div className="kanban-board">
      <div className="kanban-header">
        <span>Tasks</span>
        <span className="kanban-count">{taskList.length}</span>
        <span className="spacer" />
        <button
          onClick={() => setCreating((v) => !v)}
          disabled={!currentId}
          title="New task"
        >
          + New
        </button>
        <button
          className="btn--icon"
          onClick={() => setShowKanban(false)}
          title="Hide Kanban"
        >
          <X size={12} />
        </button>
      </div>

      {creating ? (
        <div className="kanban-create">
          <input
            placeholder="Title"
            value={draftTitle}
            onChange={(e) => setDraftTitle(e.target.value)}
            autoFocus
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                create();
              }
              if (e.key === "Escape") setCreating(false);
            }}
          />
          <textarea
            placeholder="Instructions (optional)"
            rows={3}
            value={draftInstructions}
            onChange={(e) => setDraftInstructions(e.target.value)}
          />
          <div className="kanban-create-bar">
            <button onClick={() => setCreating(false)}>Cancel</button>
            <button onClick={create} disabled={!draftTitle.trim()}>
              Create
            </button>
          </div>
        </div>
      ) : null}

      <div className="kanban-cols">
        {COLUMNS.map((col) => (
          <div
            key={col.status}
            className="kanban-col"
            onDragOver={(e) => {
              if (dragId) e.preventDefault();
            }}
            onDrop={(e) => {
              e.preventDefault();
              if (dragId) {
                move(dragId, col.status);
                setDragId(null);
              }
            }}
          >
            <div className="kanban-col-header">
              <span>{col.label}</span>
              <span className="kanban-col-count">
                {tasksByStatus[col.status].length}
              </span>
            </div>
            <div className="kanban-col-body">
              {tasksByStatus[col.status].map((t) => (
                <KanbanCard
                  key={t.id}
                  task={t}
                  agents={agents}
                  expanded={editingId === t.id}
                  onClick={() => setEditingId((v) => (v === t.id ? null : t.id))}
                  onDelete={() => remove(t.id)}
                  onMove={(status) => move(t.id, status)}
                  onAssign={(aid) => assignToAgent(t.id, aid)}
                  onDragStart={() => setDragId(t.id)}
                  onDragEnd={() => setDragId(null)}
                />
              ))}
              {tasksByStatus[col.status].length === 0 ? (
                <div className="kanban-empty">—</div>
              ) : null}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function KanbanCard({
  task,
  agents,
  expanded,
  onClick,
  onDelete,
  onMove,
  onAssign,
  onDragStart,
  onDragEnd,
}: {
  task: Task;
  agents: Record<string, import("../state/types").Agent>;
  expanded: boolean;
  onClick: () => void;
  onDelete: () => void;
  onMove: (status: TaskStatus) => void;
  onAssign: (agentId: string | null) => void;
  onDragStart: () => void;
  onDragEnd: () => void;
}) {
  const agent = task.agent_id ? agents[task.agent_id] : null;
  const agentList = Object.values(agents);
  return (
    <div
      className={`kanban-card status-${task.status}`}
      draggable
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = "move";
        onDragStart();
      }}
      onDragEnd={onDragEnd}
      onClick={onClick}
    >
      <div className="kanban-card-title">{task.title}</div>
      {task.agent_id ? (
        <div className="kanban-card-agent">
          {agent ? agent.agent_type : "agent"}@{task.agent_id.slice(0, 8)}
        </div>
      ) : null}
      {expanded ? (
        <div
          className="kanban-card-expand"
          onClick={(e) => e.stopPropagation()}
        >
          {task.instructions ? (
            <div className="kanban-card-instructions">{task.instructions}</div>
          ) : (
            <div className="kanban-card-instructions empty">no instructions</div>
          )}
          <div className="kanban-card-row">
            <label>status</label>
            <select
              value={task.status}
              onChange={(e) => onMove(e.target.value as TaskStatus)}
            >
              <option value="todo">todo</option>
              <option value="in_progress">in_progress</option>
              <option value="in_review">in_review</option>
              <option value="complete">complete</option>
              <option value="cancelled">cancelled</option>
            </select>
          </div>
          <div className="kanban-card-row">
            <label>agent</label>
            <select
              value={task.agent_id ?? ""}
              onChange={(e) => onAssign(e.target.value || null)}
            >
              <option value="">— unassigned —</option>
              {agentList.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.agent_type} {a.id.slice(0, 8)}
                </option>
              ))}
            </select>
          </div>
          <div className="kanban-card-bar">
            <button onClick={onDelete}>Delete</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
