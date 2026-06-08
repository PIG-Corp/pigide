# Async Performance Audit — pigide (Tauri 2 + Rust + Tokio)

**Аудит:** Rust async runtime / blocking I/O
**Объём:** ~46 000 строк Rust, 128 `#[tauri::command] async fn`, 2 использования `spawn_blocking` (sic!)
**Дата:** 2026-06-07
**Среда:** Tauri 2 (multi-thread tokio), tokio 1.x, r2d2_sqlite (max_size=8), `parking_lot` повсюду
**Runtime:** Tauri default = multi-threaded tokio; `pigide-agentd` = `#[tokio::main(flavor = "multi_thread")]`

---

## TL;DR — Корневая причина проблемы

PigIDE заявляет "async-first" архитектуру (128 Tauri-команд `async fn`, `tokio::select!` для cancellation, broadcast channels), **но 99% этих `async fn` содержат блокирующие sync-вызовы без `spawn_blocking`**:

1. **Все `pub fn` в `db.rs`, `chat.rs`, `chat_sessions.rs`, `workspace.rs`, `tasks.rs`, `memory/*`, `swarm/*`, `ssh.rs` — синхронные rusqlite + `std::fs::*`.** Они вызываются напрямую из `async fn` команд. Каждый `conn.prepare` / `query_row` / `execute` блокирует event loop worker thread.

2. **Pool всего 8 соединений** (db.rs:39). При множественных одновременных Tauri-командах (chat send, list workspaces, list tasks, etc.) воркеры выстраиваются в очередь к pool → cascading latency.

3. **`spawn_blocking` использован только 2 раза за весь проект** (voice/download.rs:221 для SHA256, voice/mod.rs:144 для whisper). Whisper inference и SHA256 — единственные вещи, которые архитекторы посчитали "достаточно долгими". При этом `verify_sha256` загружает 2.5 GB Whisper Large — **медленнее большинства остальных операций**, и оно изолировано; а `chat_queue::claim_next` + SQLite transaction на **каждом** `drain_once()` итерации — нет.

4. **Tauri runtime = single-process multi-thread.** Дефолтные worker threads = кол-во CPU. При 16 CPU = 16 workers. Если 16 пользователей одновременно шлют chat / читают workspaces, **все 16 workers блокируются на rusqlite** — UI event loop зависает полностью.

5. **`parking_lot::Mutex` используется в async-friendly контекстах**, но НИКОГДА не удерживается через `.await` (проверено). Однако `parking_lot::RwLock` в `chat_queue_worker.rs:36`, `state.rs:31` хранит `AppHandle` — короткие секции, deadlock-риска нет, но **priority inversion возможен**, если writer ждёт audio thread в `voice/capture.rs:77-94` (closure в real-time audio thread берёт тот же `parking_lot::Mutex`).

6. **cpal audio thread** делает `state.lock()` (parking_lot) на `samples: Vec<f32>` — запись в реальном времени. Если main thread держит этот же мьютекс дольше ~10ms (например, `stop_and_transcribe` клонирует `s.samples.clone()` пока lock держится), audio буфер переполнится. См. `voice/capture.rs:170-172`.

---

## Топ-10 блокирующих мест (P0)

### P0-1. `chat_queue_worker.rs:drain_once` — каждая итерация блокирует event loop
**Файл:** `/home/camer/pigide/src-tauri/src/chat_queue_worker.rs:104-130`
**Что делает:** Single-consumer worker, дренирует очередь user messages. **Каждый** claim/done/failed = синхронный SQLite.
**Блокирует:** Worker `tauri::async_runtime::spawn` task; пока он крутится в `drain_once`, ничего другого не работает в текущем worker thread. Хуже: `chat::list` (history 60 messages) внутри `orchestrator::tool_loop` → ~5-10 SELECT подряд.
**Fix:**
```rust
// chat_queue_worker.rs:104
async fn drain_once(&self) -> Result<()> {
    loop {
        let session_id = chat_sessions::ensure_current(&self.db)?;
        let item = match chat_queue::claim_next(&self.db, &session_id)? {  // BLOCKING
            Some(it) => it,
            None => return Ok(()),
        };
        ...
        let res = self.orch.clone().run_chat(item.text.clone()).await;  // OK, async
        match res {
            Ok(()) => { let _ = chat_queue::mark_done(&self.db, &item.id); }  // BLOCKING
            ...
        }
    }
}
```
**Патч:**
```rust
use tokio::task::spawn_blocking;

async fn drain_once(&self) -> Result<()> {
    loop {
        let session_id = chat_sessions::ensure_current(&self.db)?;  // OK if ensure_current < 1ms
        let db = self.db.clone();
        let item = spawn_blocking(move || chat_queue::claim_next(&db, &session_id))
            .await
            .map_err(|e| crate::error::Error::Other(format!("join: {e}")))??;
        let Some(item) = item else { return Ok(()) };
        self.emit_snapshot();

        let res = self.orch.clone().run_chat(item.text.clone()).await;
        let item_id = item.id.clone();
        let db = self.db.clone();
        let _ = spawn_blocking(move || {
            if res.is_ok() { chat_queue::mark_done(&db, &item_id) }
            else { chat_queue::mark_failed(&db, &item_id) }
        }).await;
        self.emit_snapshot();
    }
}
```

---

### P0-2. `commands.rs` — все 128 Tauri-команд вызывают sync DB без spawn_blocking
**Файл:** `/home/camer/pigide/src-tauri/src/commands.rs` (2241 строк, 128 `async fn`)
**Что делает:** Каждый Tauri command (list_workspaces, list_tasks, list_agents, send_chat, list_chat, create_memory, и т.д.) вызывает sync pub fn из `workspace.rs`, `tasks.rs`, `chat.rs`, `chat_sessions.rs`, `memory/service.rs`, `db.rs` — все rusqlite.
**Блокирует:** Tauri event loop. При 16 worker threads и 8 pool connections: при burst из 20 UI-events → 8 попадают в pool, 4 ждут, 8 идут на `try_wait` (rusqlite ждёт `busy_timeout=5000` = 5 секунд!). UI жёстко фризится.
**Примеры горячих команд (вызываются часто):**
- `list_workspaces` (commands.rs:23) → `WorkspaceManager::list` (workspace.rs:107) → `conn.prepare` + `query_map` + Vec alloc. ~2-10ms под нагрузкой.
- `list_tasks` (commands.rs:1238) → `TaskManager::list` (tasks.rs:155) → multi-criteria query, ~5-15ms.
- `list_chat` (commands.rs:270) → `chat::list` (chat.rs:91) — история чата, ~5-20ms.
- `send_chat` (commands.rs:287) → `chat_sessions::ensure_current` (chat_sessions.rs:308) + `enqueue_with_attachments` (chat_queue.rs:130) — 4 SQL statements подряд, ~10-30ms.
- `create_memory`, `update_memory`, `delete_memory` (commands.rs:1321-1393) — `memory/service.rs` + DB + gray_matter parse.
- `read_file` (commands.rs:1173) → `files::read` (files.rs:110) — `std::fs::read` блокирующий.
- `walk_files` (commands.rs:1204) → recursive `std::fs::read_dir` — для больших проектов секунды.

**Fix (стратегия):** Обернуть **каждый** Tauri command в `spawn_blocking`:
```rust
#[tauri::command]
pub async fn list_workspaces(state: State<'_, AppState>) -> Result<Vec<Workspace>, String> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || {
        crate::workspace::WorkspaceManager::new(db).list()
    })
    .await
    .map_err(|e| format!("join: {e}"))?
    .map_err(Into::into)
}
```
Альтернатива: переписать все DB-функции на `sqlx` или `tokio::task::spawn_blocking`-friendly обёртки. **Глобальное** изменение — высокое усилие, но без него проект не масштабируется.

---

### P0-3. `orchestrator::run_chat` — главный hot path делает 6+ sync DB операций
**Файл:** `/home/camer/pigide/src-tauri/src/orchestrator/mod.rs:390-419`
**Что делает:** На каждый user turn (самый частый user-action):
1. `chat_sessions::ensure_current` (chat_sessions.rs:308) — **2 SELECT** (chat_sessions + workspaces)
2. `chat::insert` (chat.rs:120) — 1 INSERT
3. `chat_sessions::touch` (chat_sessions.rs:210) — 1 UPDATE
4. `build_provider` (orchestrator/mod.rs:115) → `providers::build_provider` — чтение settings (~3 SELECT)
5. `tool_loop` (orchestrator/mod.rs:427) → `build_messages` → `chat::list` (chat.rs:91) — `chat_messages` full scan, лимит 60 → 1 SELECT
6. `build_memory_preamble` → `db::get_setting` + `memory.search` (FTS5 query) → 1-2 SELECT
7. На каждой iteration `MAX_ITERATIONS=6` → повтор `build_messages` (60-row scan каждый раз)
8. `chat::insert` placeholder (chat.rs:120)
9. `chat::delete_after` (chat.rs:145) при ошибке
10. `tools::dispatch` для каждого tool_call → ещё ~3-5 SELECT в среднем

**Блокирует:** В среднем 10-15 sync DB операций на 1 user turn. На multi-agent workspace с памятью: 30-50 SQL подряд. Каждая блокировка ~2-10ms. **Суммарно 200-500ms event loop заблокировано на один turn.**

**Fix:**
```rust
pub async fn run_chat(self: Arc<Self>, text: String) -> Result<()> {
    let session_id = chat_sessions::ensure_current(&self.db).await?;  // OK (small)
    // ... pattern: каждый sync pub fn → spawn_blocking обёртка
    let user_msg = ChatMessage::user(session_id.clone(), text);
    let db = self.db.clone();
    let m = user_msg.clone();
    tokio::task::spawn_blocking(move || chat::insert(&db, &m)).await??;
    // и т.д.
}
```
Альтернатива: `db.execute(...)` напрямую через `tokio::task::block_in_place` + dedicated `tokio::task::spawn_blocking` pool.

---

### P0-4. `orchestrator::tools::dispatch` — `std::fs::read` в async fn
**Файл:** `/home/camer/pigide/src-tauri/src/orchestrator/tools.rs:526`
```rust
// tools.rs:526 (контекст: read_file tool call)
let bytes = std::fs::read(&log)?;  // БЛОКИРУЮЩИЙ в async fn
```
**Что делает:** Tool call от orchestrator (read agent log) делает `std::fs::read`. Если файл большой или на медленном диске — секунды.
**Блокирует:** Orchestrator tool_loop на текущей итерации → следующие iterations и UI event loop ждут.
**Fix:** Заменить на `tokio::fs::read`:
```rust
let bytes = tokio::fs::read(&log).await?;
```

---

### P0-5. `mcp::server::audit` — SQLite INSERT в async fn
**Файл:** `/home/camer/pigide/src-tauri/src/mcp/server.rs:387-406`
```rust
fn audit(db: &DbPool, key: Option<&KeyInfo>, tool: &str, args: &Value, status: &str) {
    if let Err(e) = (|| -> Result<()> {
        let conn = db.get()?;  // BLOCKING
        conn.execute("INSERT INTO mcp_audit ...", ...)?;
        Ok(())
    })() { ... }
}
```
**Что делает:** Пишет audit log в SQLite на КАЖДЫЙ MCP tool call. Вызывается из `async fn handle_rpc` (axum handler, mcp/server.rs:178) → вызывает `dispatch_tool` → вызывает `audit` синхронно.
**Блокирует:** Axum worker thread. HTTP-сервер на 127.0.0.1 для MCP — под нагрузкой (10+ tool calls/sec) забьёт пул.
**Fix:**
```rust
async fn audit(db: &DbPool, key: Option<&KeyInfo>, tool: &str, args: &Value, status: &str) {
    let db = db.clone();
    let key_id = key.map(|k| k.id.clone());
    let args_str = serde_json::to_string(args).ok();
    let status = status.to_string();
    let tool = tool.to_string();
    if let Err(e) = tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = db.get()?;
        conn.execute("INSERT INTO mcp_audit ...", ...)?;
        Ok(())
    }).await {
        tracing::warn!("mcp audit insert failed: {e}");
    }
}
// В dispatch_tool: audit(&state.db, ...).await;
```

---

### P0-6. `agent.rs::read_log_tail` — `std::fs::File::open` + `read_to_end` синхронно
**Файл:** `/home/camer/pigide/src-tauri/src/agent.rs:251-268`
```rust
pub fn read_log_tail(&self, agent_id: &str, max_bytes: usize) -> Result<Vec<u8>> {
    // ... валидация ...
    let meta = std::fs::metadata(&path)?;  // BLOCKING
    let mut f = std::fs::File::open(&path)?;  // BLOCKING
    f.seek(SeekFrom::Start(start as u64))?;
    let mut buf = Vec::with_capacity(size - start);
    f.read_to_end(&mut buf)?;  // BLOCKING, может быть 64 KiB
    Ok(buf)
}
```
**Вызывается из:** `#[tauri::command] agent_log_tail` (commands.rs:246) — на каждом restore / mount tile. С 10 агентами и 64 KiB scrollback = 640 KiB I/O синхронно в event loop.
**Блокирует:** UI при каждом restore_session / workspace switch.
**Fix:**
```rust
pub async fn read_log_tail(&self, agent_id: &str, max_bytes: usize) -> Result<Vec<u8>> {
    if !is_safe_agent_id(agent_id) { return Err(...); }
    let path = log_dir().join(format!("{}.log", agent_id));
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        if !path.exists() { return Ok(Vec::new()); }
        let meta = std::fs::metadata(&path)?;
        let size = meta.len() as usize;
        let start = size.saturating_sub(max_bytes);
        let mut f = std::fs::File::open(&path)?;
        use std::io::{Read, Seek, SeekFrom};
        f.seek(SeekFrom::Start(start as u64))?;
        let mut buf = Vec::with_capacity(size - start);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    }).await?
}
```
Аналогичная проблема в `agentd/engine.rs:216-228 log_tail` — sync, но `Engine` не async (читается из broker бинаря). Тоже нужен spawn_blocking если broker process когда-нибудь станет async.

---

### P0-7. `voice/capture.rs` — audio thread → parking_lot Mutex contention
**Файл:** `/home/camer/pigide/src-tauri/src/voice/capture.rs:73-94, 98-118, 122-141`
**Что делает:** Real-time audio thread (cpal) вызывает closure на каждом audio buffer (~10ms @ 48kHz). Closure берёт `parking_lot::Mutex` на `state.samples: Vec<f32>`. Main thread (UI) вызывает `stop()` (voice/capture.rs:163-172) → клонирует `s.samples.clone()` под тем же мьютексом. Для 60s записи = 2.88M samples → `.clone()` = десятки мегабайт копирования **под мьютексом** в audio thread.
**Блокирует:** Audio dropouts (XRUN) при больших записях. Также `VoicePipeline::stop_and_transcribe` (voice/mod.rs:85) ждёт audio thread → event loop задержка.
**Fix:** Двойная буферизация / `Arc<Mutex<Vec<f32>>>` + atomic swap, или вообще убрать мьютекс через `crossbeam::queue` / lock-free SPSC:
```rust
// В Capture::start: использовать ring buffer
let (producer, consumer) = rtrb::RingBuffer::<f32>::new(2_880_000);
// audio thread пишет в producer (lock-free, no contention)
// stop() забирает consumer (lock-free) и сразу drop producer
```

---

### P0-8. `memory/service.rs` — ВСЕ pub fn синхронные, дёргаются из `async fn` Tauri commands
**Файл:** `/home/camer/pigide/src-tauri/src/memory/service.rs` (928 строк, все `pub fn` — sync)
**Что делает:** `list`, `search`, `upsert_by_slug`, `delete`, `reindex_from_disk` — все rusqlite + `std::fs::metadata` + `std::fs::read_to_string` + `gray_matter` парсинг.
**Блокирует:** Всё, что вызывает memory через Tauri commands: `list_memories` (commands.rs:1395), `search_memories` (commands.rs:1418), `read_memory` (commands.rs:1346), `update_memory` (commands.rs:1367), `delete_memory` (commands.rs:1378), `memory_graph` (commands.rs:1449), `find_backlinks` (commands.rs:1429), `suggest_connections` (commands.rs:1437). Под нагрузкой все 16 worker threads забиты rusqlite.
**Fix:** Глобальный `spawn_blocking` wrapper в `memory::tools::dispatch` (memory/tools.rs:120) — каждое обращение через Tauri → spawn_blocking.

---

### P0-9. `chat_sessions::ensure_current` вызывается на КАЖДОМ Tauri command
**Файл:** `/home/camer/pigide/src-tauri/src/chat_sessions.rs:308-328`
**Что делает:**
```rust
pub fn ensure_current(db: &DbPool) -> Result<String> {
    let cur = current(db)?;  // 1 SELECT chat_sessions + 1 SELECT settings
    if let Some(id) = cur.id { return Ok(id); }
    // else: создать дефолт
    let conn = db.get()?;
    let tx = conn.transaction()?;  // BLOCKING
    // ... INSERT + UPDATE
    Ok(...)
}
```
**Блокирует:** На КАЖДОМ `list_chat_queue`, `send_chat`, `list_chat`, `cancel_chat_queue_item`, `voice_history_list`, и т.д. — то есть **на каждом частом user-action**. Транзакция при cold-start записи.
**Fix:** Кэш в `RwLock<Option<String>>` обнуляется только при `set_current`:
```rust
// In AppState: current_session: parking_lot::RwLock<Option<String>>
// ensure_current → сначала cache hit, потом DB
// set_current → обновляет cache + DB
```

---

### P0-10. `orchestrator/mod.rs:484` — `tokio::sync::mpsc::unbounded_channel` для chat deltas
**Файл:** `/home/camer/pigide/src-tauri/src/orchestrator/mod.rs:484-491`
```rust
let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
let forwarder = tokio::spawn(async move {
    while let Some(delta) = delta_rx.recv().await {
        if let Some(app) = &app {
            let _ = app.emit(EV_CHAT_CHUNK, json!({ "id": id_for_cb, "delta": delta }));
        }
    }
});
```
**Что делает:** Provider streaming → delta_tx → forwarder → Tauri event emit.
**Проблема:** `unbounded_channel` может расти бесконечно. Если Tauri event emit медленнее, чем provider streaming — **unbounded memory growth**. На длинной streaming session (10+ минут генерации) — сотни MB.
**Блокирует:** Memory leak → OOM при долгих генерациях. Под нагрузкой (multi-agent) — несколько каналов одновременно растут.
**Fix:**
```rust
// bounded с backpressure:
let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(256);
// provider chat_stream должен handle .await на .send() (если забит — provider ждёт)
```

---

## Топ-10 lock contention (P1)

### P1-1. `chat_queue_worker.rs:36` — `parking_lot::RwLock<Option<AppHandle>>`
**Файл:** `/home/camer/pigide/src-tauri/src/chat_queue_worker.rs:27, 36-42`
**Contention:** Низкий — короткие секции. НО: `emit_snapshot` берёт `read()`, вызывает `chat_queue::list` → rusqlite **под read lock**. Если UI рендерит snapshot + пользователь шлёт chat → оба берут read lock, snapshot read может длиться десятки ms.
**Fix:** `read()` держать минимально, `app.emit` — после `drop(lock)`.

### P1-2. `state.rs:31` — `parking_lot::RwLock<Option<Arc<Watcher>>>`
**Файл:** `/home/camer/pigide/src-tauri/src/state.rs:31`
**Contention:** Низкий (set один раз), но `app.emit` во всех watcher callback'ах берёт этот `RwLock`. Если несколько `EV_AGENT_STDOUT` событий — все ждут.

### P1-3. `voice/mod.rs:15, 26-29` — Mutex на `whisper: HashMap` + `last_emit`
**Файл:** `/home/camer/pigide/src-tauri/src/voice/mod.rs:26-29, 121-140`
**Contention:** Voice pipeline stop_and_transcribe → кэш Whisper моделей. Mutex удерживается во время `Whisper::open` (loading model, секунды!). Если два одновременных `stop_and_transcribe` — второй ждёт загрузки модели. Lock удерживается через `db.get_setting("whisper.model_id")` (sync DB) внутри `current_model()`.
**Fix:** `RwLock<HashMap<…, Arc<Whisper>>>` — `read()` для существующих, `write()` только для insert. Загрузку делать вне write lock.

### P1-4. `voice/capture.rs:23-24` — `parking_lot::Mutex<CaptureState>` (см. P0-7)
**Contention:** Real-time audio thread vs main thread. Самый опасный случай.

### P1-5. `mcp/server.rs:27, 95` — `parking_lot::Mutex<Option<RunningHandle>>`
**Файл:** `/home/camer/pigide/src-tauri/src/mcp/server.rs:95-117`
**Contention:** Минимальный (start/stop). OK.

### P1-6. `architect/supervisor.rs:16` — `parking_lot::RwLock<Option<AppHandle>>`
**Contention:** Минимальный. OK.

### P1-7. `watcher/supervisor.rs:22` — `parking_lot::RwLock` на `Watcher::Inner`
**Файл:** `/home/camer/pigide/src-tauri/src/watcher/supervisor.rs`
**Contention:** `run_reply_pump` (tick 250ms) + `handle_stdout_event` (per stdout chunk) — оба читают inner. Низкий. OK.

### P1-8. `agentd/engine.rs:28-29` — `parking_lot::{Condvar, Mutex}` для readiness
**Файл:** `/home/camer/pigide/src-tauri/src/agentd/engine.rs:301, 339-345`
**Что делает:** `Condvar` для синхронизации "agent ready for write". Writer ждёт, reader сигналит. **Это НЕ tokio::sync::Notify, а std.** OK для broker (отдельный процесс), не блокирует UI.
**Примечание:** broker-процесс `pigide-agentd` однопоточный broker → agent RT, OK.

### P1-9. `agent.rs:51, 167-185` — AgentManager: 6 `parking_lot::Mutex<...>`
**Файл:** `/home/camer/pigide/src-tauri/src/agent.rs:167-185`
**Contention:** `connected: Mutex<Option<ConnectedState>>` — сериализует доступ к broker. `connect_lock: AsyncMutex<()>` — но всё равно под parking_lot снаружи. **Гонка:** если два Tauri-команды одновременно хотят broker — обе берут `connected.lock()`, одна видит `Some`, другая ждёт.
**Проблема:** `connected.lock()` держится через `block_on_safely` → `block_in_place` → async agent client. Это `parking_lot::Mutex` **НЕ через await** (используется `block_in_place`), OK, НО — `block_in_place` + parking_lot Mutex = другие tokio workers продолжают работать, deadlock-риска нет, но **starvation** если один worker держит долго.

### P1-10. `orchestrator/mod.rs:51, 77, 89-100` — `parking_lot::Mutex<BTreeMap<u64, oneshot::Sender>>` для abort
**Файл:** `/home/camer/pigide/src-tauri/src/orchestrator/mod.rs:51, 77, 89-100`
**Contention:** Низкий (только register/clear/cancel_all). OK.

---

## Долгие sync операции без spawn_blocking (P1)

Все они прямо или косвенно блокируют event loop. **P1 = нужно исправить, но не блокирует UI на старте:**

| Файл:строка | Функция | Что делает | Приоритет |
|-------------|---------|------------|-----------|
| `db.rs:24,30` | `Connection::open` + `migrate_one` | Открытие + миграции SQLite | P1 (один раз при boot) |
| `db.rs:658, 669, 704, 718, 732, 744, 759, 768` | `get_setting`, `set_setting`, `list_custom_providers`, ... | Каждая rusqlite операция | P0 |
| `chat_queue.rs:81,108,123,130,215,261,272,282,289,321` | `ensure_table`, `recover_inflight`, `enqueue`, `claim_next`, `cancel`, `mark_done`, `mark_failed`, `list`, `pending_count` | Все sync rusqlite | P0 |
| `chat.rs:91,120,145` | `list`, `insert`, `delete_after` | sync rusqlite | P0 |
| `chat_sessions.rs:102,143,154,191,204,210,230,237,241,308,330,342` | `list`, `get`, `create`, `rename`, `delete`, `touch`, `get_scope`, `set_scope`, `current_workspace_id`, `ensure_current`, `current`, `set_current` | sync rusqlite | P0 |
| `workspace.rs:107,142,174,234,240` | `list`, `get`, `create`, `delete`, `update_layout` | sync rusqlite + JSON | P0 |
| `tasks.rs:90,142,155,202,282` | `create`, `get`, `list`, `update`, `delete` | sync rusqlite | P0 |
| `agent.rs:241,272,300, ...` (sync methods) | `reset_statuses`, `list_persisted_running`, `respawn_persisted`, `read_log_tail` | sync rusqlite + `std::fs` | P0 |
| `mcp/server.rs:387` | `audit` | sync rusqlite INSERT | P0-5 |
| `memory/service.rs:120,393,416,459,637,669` | `upsert_by_slug`, `delete`, `list`, `search`, `reindex_from_disk`, `delete_by_path` | sync rusqlite + `std::fs::metadata` + `gray_matter` | P0-8 |
| `memory/note.rs:336,341,343` | `read`, write note | `std::fs::read_to_string` + `std::fs::write` | P0 |
| `memory/storage.rs:28,48,98,108,121,136,146` | `create_dir_all`, `remove_dir_all` | sync `std::fs` | P1 |
| `memory/migration.rs:83,109,150,185` | scan + read + write | sync `std::fs` | P1 |
| `memory/ingest/chat_chunk.rs:11` | `parking_lot::Mutex<Buffer>` | parking_lot в async ingest pipeline | P1 |
| `memory/ingest/smart.rs:83,337,361,373,382,405,436,461,501` | `run_pass_for_workspace` + tests | async + sync `std::fs` | P1 |
| `skills/registry.rs:105,179,312,324` | `read_to_string`, `read_dir`, `canonicalize` | sync `std::fs` | P1 |
| `skills/claude_import.rs:83,119,204,216,223,242,284,288,292,325,745` | Claude import | sync `std::fs` + `gray_matter` парсинг | P1 |
| `skills/composer.rs:265` | compose | sync | P1 |
| `skills/trace.rs:140` | record | sync rusqlite | P1 |
| `project_resolver/indexer.rs:127,162,174,275,281,294` | file walk | sync `std::fs::metadata`/`read_dir`/`read_to_string` | P1 (heavy на большом проекте) |
| `project_resolver/service.rs:12` | RwLock on ResolverService | parking_lot в async context | P1 |
| `files.rs:27,66,110,120,122,284` | `browse_dir`, `read`, `write` | sync `std::fs` | P0 (Tauri commands) |
| `ssh.rs:427` | ssh presets | sync rusqlite | P1 |
| `secrets.rs:33,72,93,110` | read/write encrypted secrets | sync `std::fs` | P0 (Tauri commands) |
| `orchestrator/phantom.rs:139,145,244,248` | phantom log append | sync `std::fs` | P1 |
| `voice/download.rs:100,120,148,157,217` | `cache_dir`, `model_exists`, `ensure_model`, `download_model`, `verify_sha256` | async + `tokio::fs` (OK) + `spawn_blocking` (OK в `verify_sha256`) | OK (правильно) |
| `voice/history.rs:193` | WAV write | sync `std::fs` | P1 (не async) |
| `voice/dictionary.rs:175` | dictionary | sync rusqlite | P1 |
| `voice/inject.rs:105` | `std::thread::sleep` в async | БЛОКИРУЕТ на 200ms | P0 |
| `swarm/mailbox.rs`, `swarm/ownership.rs`, `swarm/review.rs`, `swarm/rollcall.rs` | все sync rusqlite + std::fs | P1 |
| `watcher/supervisor.rs:198,221,254,265` | `handle_stdout_event`, `process_chunk`, `run_reply_pump`, `drain_replies` | async (OK), но `mailbox::send_system` + `mailbox::list_thread` sync | P0-5 |
| `watcher/rate_limiter.rs:7,10,105` | `parking_lot::Mutex` + `std::thread::sleep` (test) | P1 (тест) |
| `agentd/engine.rs:161,216,320,377,402,409` | `broadcast::channel(1024)`, `log_tail`, reader thread, `write` | sync (broker OK) | OK (отдельный процесс) |
| `agentd/client.rs:104,105,530,531` | `mpsc::channel(256)`, `mpsc::channel(8)`, `broadcast::channel(1024)`, `broadcast::channel(8)` | OK | OK |
| `agentd/supervisor.rs:52,87,67` | `connect_or_spawn`, `try_connect`, `remove_file` | async + sync `std::fs::remove_file` | P1 |
| `bin/pigide-agentd.rs:34,47,49,69,106,150,176` | `tokio::main`, `create_dir_all`, `remove_file`, `set_permissions`, `OpenOptions` | OK (broker, sync daemon) | OK |
| `orchestrator/providers/omni.rs:159`, `orchestrator/providers/anthropic.rs:332` | `tokio::sync::mpsc::unbounded_channel` для стрима | unbounded → memory leak | P1 |
| `commands.rs:1173,1188,1204` | `read_file`, `write_file`, `walk_files` | sync `std::fs` (см. P0-2) | P0 |
| `commands.rs:828` | `voice_list_models` → `voice::download::list_models` → `model_exists` → `std::fs::metadata` | sync | P0 |

---

## Рекомендации по архитектуре async runtime

### 1. Worker thread pool

**Tauri runtime = multi-thread tokio = N workers = N_cpu.** По умолчанию Tokio подбирает = `num_cpus`. На 8-CPU машине = 8 воркеров. Если rusqlite пул = 8 соединений → 1:1 → при 8 одновременных SQLite-bound tasks UI замирает.

**Рекомендации:**
- **Увеличить rusqlite pool** с `max_size=8` (db.rs:39) до `max_size=32-64`. WAL mode позволяет много reader'ов.
- **spawn_blocking pool size** = Tokio default = 512. ОК.
- **Критические "горячие" workers** (chat_queue_worker, voice pipeline) запускать в **отдельных runtime thread** с `tokio::runtime::Handle::current().spawn_blocking(...)` либо `block_in_place`.

### 2. Каналы (bounded vs unbounded, capacity)

| Место | Текущее | Проблема | Рекомендация |
|-------|---------|----------|--------------|
| `orchestrator/mod.rs:484` | `mpsc::unbounded_channel::<String>()` для deltas | Memory leak при долгой генерации | `mpsc::channel(256)` — bounded с backpressure на provider |
| `orchestrator/providers/omni.rs:159` | `mpsc::unbounded_channel` | memory leak | `mpsc::channel(128)` |
| `orchestrator/providers/anthropic.rs:332` | `mpsc::unbounded_channel` | memory leak | `mpsc::channel(128)` |
| `agentd/client.rs:104,530` | `mpsc::channel(256)`, `mpsc::channel(8)` | OK | OK |
| `agentd/client.rs:105,531` | `broadcast::channel(1024)`, `broadcast::channel(8)` | OK | OK |
| `agentd/engine.rs:161` | `broadcast::channel(1024)` для всех agents | Backpressure: при 16+ agents × 8 KiB chunks → 128 KB lag drop. OK для scrollback. | OK (design choice) |
| `memory/watcher.rs:37` | `std::sync::mpsc::channel()` (sync, OK) | OK | OK |
| `skills/watcher.rs:43` | `std::sync::mpsc::channel()` (sync, OK) | OK | OK |

### 3. Где применить `spawn_blocking`

**Глобальное правило:** **ЛЮБОЙ** `std::fs::*`, `rusqlite::*`, `std::process::Command`, `cpal::*`, `whisper_rs::*`, `gray_matter` парсинг, `std::thread::sleep` в `async fn` → `spawn_blocking` или `tokio::fs::*`.

**Конкретные места (топ приоритет):**
- `db.rs` — обернуть `init_pool` целиком (один раз при boot, OK).
- `db::get_setting` / `set_setting` — самые частые sync вызовы из commands.rs.
- `chat_queue::*` — все 11 функций, вызываются из async event loop.
- `memory/service.rs` — все pub fn, вызываются из memory::tools::dispatch (async).
- `commands.rs` — глобально обернуть каждый Tauri command.
- `mcp/server.rs::audit` — на каждом MCP call.
- `agent.rs::read_log_tail` — на каждом restore.
- `orchestrator/phantom.rs::append_event` — на каждом phantom detection.
- `voice/capture.rs::start` — cpal инициализация (долгая).

### 4. Где использовать `tokio::task::JoinSet` для fan-out

**`orchestrator::tool_loop` (orchestrator/mod.rs:603-631):** Когда модель эмитит несколько `tool_calls` подряд, они выполняются последовательно:
```rust
for call in assembled.tool_calls.unwrap_or_default() {
    let result = tools::dispatch(...).await;
    chat::insert(...);
}
```
**Fix:** Fan-out через `JoinSet`:
```rust
use tokio::task::JoinSet;
let mut set = JoinSet::new();
for call in assembled.tool_calls.unwrap_or_default() {
    let args: Value = ...;
    let name = call.function.name.clone();
    let id = call.id.clone();
    let db = self.db.clone();
    let ws_mgr = self.ws_mgr.clone();
    let agent_mgr = self.agent_mgr.clone();
    // ... clone все Arc handles
    set.spawn(async move {
        let result = tools::dispatch(&db, ..., &name, &args).await
            .unwrap_or_else(|e| json!({"error": e.to_string()}));
        (id, result)
    });
}
let mut tool_msgs = Vec::new();
while let Some(res) = set.join_next().await {
    if let Ok((id, result)) = res {
        tool_msgs.push(ChatMessage::tool(session_id, &id, result.to_string()));
    }
}
// затем batch INSERT (внутри одного transaction)
```

**`watcher::supervisor::run_reply_pump` (watcher/supervisor.rs:254-261):** Опрашивает все агенты последовательно. JoinSet по агентам.

### 5. Где использовать `tokio_stream` для backpressure

**`chat_queue_worker.rs::drain_once` → orchestrator run_chat:** Сейчас worker спит на `Notify`. Альтернатива: stream of QueueItem через `tokio_stream::wrappers::ReceiverStream`:
```rust
let (item_tx, mut item_rx) = mpsc::channel::<QueueItem>(8);
let worker = tokio::spawn(async move {
    let mut stream = ReceiverStream::new(item_rx);
    while let Some(item) = stream.next().await {
        // process
    }
});
```

**`watcher::supervisor::process_chunk` → Gemini API:** Поток stdout chunks от N агентов → rate limiter → classifier. Использовать `Stream::buffer_unordered(N)`:
```rust
use tokio_stream::StreamExt;
let chunks = futures::stream::iter(agents.iter().flat_map(|a| a.pending_chunks()));
chunks.for_each_concurrent(4, |chunk| async move {
    classify_chunk(&client, &chunk).await
}).await;
```

### 6. Где пересмотреть архитектуру целиком

**`Tauri command pattern: async fn → sync DB`** — это anti-pattern, который нужно либо:
- (a) Глобальный `#[tauri::command] async fn` → первый вызов `tokio::task::spawn_blocking` (минимальное изменение, **рекомендую**).
- (b) Миграция на `sqlx` (async rusqlite замена) — большое усилие, но правильное долгосрочное решение.
- (c) **DB connection per Tokio worker** вместо r2d2 pool — не рекомендую (SQLite + WAL плохо с сотнями коннектов).

**`Audio capture → main thread clone`** — нужен lock-free SPSC ring buffer (`rtrb` crate).

**`MCP server audit` (P0-5)** — fire-and-forget через `tokio::spawn` + mpsc channel в writer task.

**`watcher::supervisor::drain_replies`** — должен быть полностью async, не дёргать rusqlite напрямую из async fn.

### 7. Скорейшие выигрыши (top 5 P0)

Если бы нужно было исправить **5 мест** для максимального эффекта:

1. **Все 128 Tauri commands в `commands.rs`** → `tokio::task::spawn_blocking` wrapper. Один helper macro:
   ```rust
   macro_rules! blocking_cmd {
       ($fn:ident($($arg:ident: $t:ty),*) -> $ret:ty $body:block) => {
           #[tauri::command]
           pub async fn $fn($($arg: $t),*) -> Result<$ret, String> {
               tokio::task::spawn_blocking(move || $body)
                   .await.map_err(|e| format!("join: {e}"))?
           }
       }
   }
   ```

2. **`chat_queue_worker.rs::drain_once`** — spawn_blocking на каждой DB-операции.

3. **`orchestrator::run_chat` + `tool_loop`** — все `chat::*`, `db::get_setting` → spawn_blocking.

4. **`memory::tools::dispatch` + `memory::service::*`** — async-friendly wrapper.

5. **`voice/capture.rs`** — lock-free SPSC ring buffer вместо `parking_lot::Mutex<Vec<f32>>`.

### 8. Метрики для отслеживания

- **Tokio Console** (`RUSTFLAGS="--cfg tokio_unstable"`, feature `tracing`) — live view на blocked tasks.
- **`prometheus` exporter** для `db.pool.connections_in_use`, `db.pool.connections_idle`, `chat_queue.pending`, `voice.recording.duration_ms`.
- **`strace -p <pid> -e futex,ioctl,read,write`** — увидеть реальные syscall'ы во время "UI зависа".

---

## Приложение: каналы между std и tokio (anti-patterns)

**Найдено:**
- `memory/watcher.rs:37` — `std::sync::mpsc::channel()` — это ОК, т.к. watcher thread сам std (НЕ tokio), notify-debouncer требует std.
- `skills/watcher.rs:43` — то же.
- `agentd/engine.rs` — sync runtime, не ток. ОК.

**Не найдено** (хорошо):
- Никто не пытается клонировать `tokio::sync::mpsc::Sender` в `std::thread` (что было бы UB — tokio Sender не Send в non-tokio contexts в старых версиях, а в новых — Send, но `.send()` требует .await).

**Однако:** `orchestrator/mod.rs:51` — `parking_lot::Mutex<BTreeMap<u64, tokio::sync::oneshot::Sender<()>>>`. Mutex-контейнер с tokio-sender'ами — `register_abort` (orchestrator/mod.rs:89) берёт `self.abort_triggers.lock()` → `insert(tx)`. Это sync mutex с tokio-sender'ом внутри — ОК для короткой секции. Sender'ы отправляются через `tx.send(())` из `cancel_all` (orchestrator/mod.rs:104-113) — `send` для oneshot не async, ОК. **Но** `cancel_all` вызывается из `stop_chat` (commands.rs:392) — sync из async fn, OK.

---

## Итоговая сводка приоритетов

| # | Категория | Файл | Приоритет | Effort |
|---|-----------|------|-----------|--------|
| 1 | 128 Tauri commands без spawn_blocking | `commands.rs` | **P0** | Medium (macro) |
| 2 | chat_queue_worker drain_once | `chat_queue_worker.rs:104` | **P0** | Low |
| 3 | orchestrator run_chat / tool_loop | `orchestrator/mod.rs` | **P0** | High |
| 4 | mcp::server::audit | `mcp/server.rs:387` | **P0** | Low |
| 5 | agent::read_log_tail | `agent.rs:251` | **P0** | Low |
| 6 | memory::service + tools::dispatch | `memory/service.rs`, `memory/tools.rs` | **P0** | High |
| 7 | voice::capture lock-free | `voice/capture.rs` | **P0** | Medium |
| 8 | orchestrator mod unbounded mpsc | `orchestrator/mod.rs:484` | **P0** | Low |
| 9 | voice::inject std::thread::sleep | `voice/inject.rs:105` | **P0** | Low |
| 10 | projects/files walk_files | `files.rs:284`, `commands.rs:1204` | **P0** | Medium |

**Главный вывод:** PigIDE — **это по сути sync код, обёрнутый в async fn signature.** Tokio runtime даёт ложное ощущение async, но реально весь I/O блокирует worker threads. Для production-grade multi-agent IDE нужно либо мигрировать DB слой на `sqlx` / `deadpool-sqlite` (async), либо дисциплинированно обернуть ВСЕ sync-вызовы в `spawn_blocking`. Без этого проект не выдержит >5 одновременных CLI-агентов.
