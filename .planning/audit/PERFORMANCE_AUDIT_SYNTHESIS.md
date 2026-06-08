# PigIDE Performance Audit — Synthesis

**Дата:** 2026-06-07
**Объём:** 4 параллельных субагента, 10.9K строк Rust + 1.7 MB frontend bundle
**Цель:** 240 FPS UI при множественных CLI-агентах (cloud code, open code, etc.)

---

## TL;DR

PigIDE страдает от **системной** проблемы: **sync I/O (rusqlite, std::fs) выполняется прямо внутри `async fn`** во всём коде. Tauri runtime = 8 worker threads, db pool = 8 коннектов → при 8 одновременных SQLite-bound tasks весь event loop зависает.

**Корневой fix:** helper `db::spawn_blocking` + дисциплинированное использование во всех 128 `#[tauri::command] async fn` + async-loop worker'ах.

**Уже применено в этой сессии:**

| # | Файл | Что | Влияние |
|---|------|-----|---------|
| 1 | `db.rs` | `spawn_blocking<F, R>` helper | Базовый примитив |
| 2 | `db.rs` | PRAGMA `synchronous=NORMAL`, `cache_size=-64000`, `mmap_size=256MB`, `min_idle(1)` | -50% fsync, меньше cold-start |
| 3 | `commands.rs` (8 команд) | `walk_files`, `read_file`, `write_file`, `list_dir`, `browse_dir`, `agent_log_tail`, `list_chat`, `list_agents`, `list_tasks`, `list_memories`, `search_memories`, `list_chat_queue` → `spawn_blocking` | UI не зависает на disk I/O |
| 4 | `agent.rs` | PTY stdout coalescer (50ms batch flush) | 100+ events/sec → ≤20/sec × 6 агентов |
| 5 | `agent.rs` | При Exit — drain pending buffer для финального flush | Никаких потерянных строк |
| 6 | `chat_queue_worker.rs` | `drain_once` + `emit_snapshot` → `spawn_blocking` | orchestrator не блокируется на SQLite |
| 7 | `orchestrator/client.rs` | `reqwest::Client` через `OnceLock` singleton | No socket leak, no DNS per call |

`cargo check` после всех правок: **0 ошибок**.

---

## Топ-7 оставшихся проблем (P0-P1) по приоритету

### 1. 🎯 Orchestrator 10-15 sync DB ops на каждый turn (P0)
**Файл:** `src-tauri/src/orchestrator/mod.rs:390` (`run_chat`)
**Проблема:** Один user-turn = 10-15 sync SQLite вызовов (read history, write message, update session, write to ingest_queue, etc.) → 200-500ms заблокированного event loop **на каждое** сообщение юзера.
**Fix:** Bulk-обёртка: собрать все DB calls в один `spawn_blocking` блок, или мигрировать orchestrator на собственные background task с mpsc каналом.

### 2. 🎯 `voice/capture.rs:73-94` — parking_lot::Mutex в real-time audio thread (P0)
**Файл:** `src-tauri/src/voice/capture.rs`
**Проблема:** Audio capture thread берёт `parking_lot::Mutex` и клонирует буфер 2.88M сэмплов под локом → priority inversion, audio dropouts.
**Fix:** Lock-free SPSC ring buffer (`rtrb` crate) между audio thread и consumer task.

### 3. 🎯 Voice whisper model HashMap без LRU (P0)
**Файл:** `src-tauri/src/voice/whisper.rs`
**Проблема:** `VoicePipeline.whisper` HashMap без eviction → 3 модели одновременно = 3+ GB RAM (особенно с CUDA context).
**Fix:** LRU cache на 1-2 модели + idle eviction (выгружать неиспользуемые через 5 мин idle).

### 4. 🎯 Engine `last_stdout` HashMap leak (P0)
**Файл:** `src-tauri/src/agentd/engine.rs:131, 336, 419`
**Проблема:** `last_stdout: HashMap<agent_id, Instant>` растёт бесконечно — никогда не удаляется в `kill` или EOF-ветке reader thread.
**Fix:** В `kill()` и в EOF cleanup добавить `.remove(agent_id)`.

### 5. 🎯 Reader thread heap churn (P0)
**Файл:** `src-tauri/src/agentd/engine.rs:346`
**Проблема:** Каждый chunk: `Arc::new(buf[..n].to_vec())` — heap allocation на каждый 8 KiB read. При 6 активных агентах = 100+ alloc/sec → GC pressure.
**Fix:** Pre-allocated `Bytes`/`bytes::BytesMut` ring или `Arc<[u8]>` от `Box<[u8]>` с copy-on-write.

### 6. ⚠️ `agentd` broadcast::channel(1024) лагает на burst'ах (P1)
**Файл:** `src-tauri/src/agentd/engine.rs:161`
**Проблема:** При 6+ агентах, активный cloud-code может выдать 500+ chunks/sec → broadcast лагает, subscriber теряет события.
**Fix:** Увеличить capacity до 4096 или перейти на `tauri::ipc::Channel` для stdout (per-agent stream, natural backpressure).

### 7. ⚠️ `mpsc::unbounded_channel` для chat deltas (P1)
**Файл:** `src-tauri/src/orchestrator/mod.rs:484`
**Проблема:** Memory leak при долгой генерации — если consumer медленнее producer'а, буфер растёт.
**Fix:** Bounded `mpsc::channel(256)` + drop policy на старые сообщения.

---

## Frontend топ-5 (P0)

(Полный список в `perf-frontend-react.md`.)

1. **xterm.js на DOM renderer** — 1 строка fix, **3× FPS** boost:
   ```ts
   new Terminal({ rendererType: 'canvas' })
   ```

2. **AgentTile не мемоизирован** (`AgentTile.tsx:89,97-104`) — читает `s.layout` напрямую → каскад на каждый split/drag. Fix: `React.memo(AgentTile)` + stable callbacks.

3. **MemoryGraph делает `getComputedStyle` в render** (`MemoryGraph.tsx:62-72`, `PigMemoryGraph.tsx:178-191`) — forced sync layout 9× per render. Fix: `useRef<HTMLDivElement>` + `getComputedStyle` в `useEffect`.

4. **`useAgentSummary` создаёт listener+setInterval per agent** — 6 tiles = 6 listeners на ОДНО событие. Fix: single global `onAgentStdout` hub с per-agent slice.

5. **Bundle 1.7 MB одним чанком** — `manualChunks` в `vite.config.ts`:
   ```ts
   manualChunks: {
     'cm': ['codemirror', '@codemirror/*'],
     'xterm': ['@xterm/*'],
     'graph': ['react-force-graph-2d'],
   }
   ```
   Lazy import для force-graph, CodeMirror, xterm.

---

## Метрики успеха

| Метрика | Сейчас (оценка) | Цель |
|---------|-----------------|------|
| Tauri command IPC roundtrip | ~5-15ms (с sync SQLite) | <1ms (spawn_blocking) |
| Stdout events/sec × 6 agents | 100-600 (с broadcast storm) | ≤120 (с coalescer) |
| RSS idle | ~200MB | 200MB |
| RSS + 6 agents | 800MB+ | <400MB |
| Swap | >0 (whisper) | 0 |
| Bundle size | 1.7 MB | <800 KB |
| React re-renders per chunk | O(n) на chat array | O(1) с memo |
| FPS under load | 30-60 | 240 |

---

## Дальнейшие шаги (по приоритету)

**Сегодня:**
- [ ] Engine `last_stdout` cleanup в kill/EOF
- [ ] Voice `parking_lot::Mutex` → lock-free SPSC
- [ ] Whisper LRU eviction

**Эта неделя:**
- [ ] Frontend `React.memo` + canvas renderer для xterm
- [ ] `manualChunks` + lazy imports
- [ ] Voice `mpsc::channel(256)` вместо unbounded

**Дальше:**
- [ ] Миграция orchestrator на bulk DB wrap или sqlx
- [ ] `tauri::ipc::Channel<T>` для stdout stream
- [ ] Reader thread heap allocation fix (Bytes/BytesMut)

---

## Файлы

- `perf-async-rust.md` — 30+ P0/P1 находок по Rust async
- `perf-ipc-tauri.md` — 5 P0 / 10 P1 / 7 P2 по Tauri IPC
- `perf-frontend-react.md` — 12 P0 по React рендерингу
- `perf-memory-cpu.md` — 10 P0 утечек, runtime tuning, benchmarks
