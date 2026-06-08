# Performance, Memory, and CPU Audit — pigide (Tauri 2 + Rust)

> Дата: 2026-06-07. Аудит кода `src-tauri/src/**` (broker, orchestrator,
> voice, memory, swarm, watcher). Без правок. Все оценки — upper bound при
> типичной нагрузке 10 активных CLI-агентов на 4-ядерной / 16 ГБ машине
> под Linux cachyos 7.0.

## TL;DR (P0)

| # | Проблема | Файл:строка | Эффект | Effort |
|---|----------|-------------|--------|--------|
| 1 | `runtime.last_stdout` HashMap растёт вечно при churn агентов | `agentd/engine.rs:131`, `agentd/engine.rs:336` | +200 B/агент/сек, утечка в broker | XS |
| 2 | `broadcast::channel(1024)` для stdout — обрезается, клиент вынужден ребилдить scrollback | `agentd/engine.rs:161`, `server.rs:266` | лаг UI + лишний дисковый read | S |
| 3 | `Reader thread` аллоцирует новый `Vec<u8>` per chunk (8 KiB каждый) | `agentd/engine.rs:346` | +1–5 MB/s heap churn на 10 агентов | XS |
| 4 | `Capture.samples: Vec<f32>` без upper-bound в I16/U16 путях (только F32 имеет 60s cap) | `voice/capture.rs:98-145` | +1 MB/мин если забыли drain | XS |
| 5 | `agent_mgr.last_stdout` HashMap в PigIDE не очищается при Exit | `agent.rs:162`, `agent.rs:642` | медленная утечка в UI процессе | XS |
| 6 | `phantom_log.jsonl` append-only без rotation | `orchestrator/phantom.rs:136` | неограниченный рост при шторме | S |
| 7 | `mpsc::unbounded_channel` для chat deltas | `orchestrator/mod.rs:484` | OOM при зависшем provider | S |
| 8 | `Writer task` в broker сериализует **все** frames под одним mutex'ом, а rx=256 — back-pressure дропает events | `agentd/server.rs:59` | потеря stdout-чанков при burst'е | S |

## TL;DR (P1)

| # | Проблема | Файл:строка | Эффект |
|---|----------|-------------|--------|
| 9 | `r2d2_sqlite::Pool::max_size(8)` без `min_idle` — cold starts | `db.rs:39` | +50–200 ms latency на холодных запросах |
| 10 | `OmniClient` создаёт `reqwest::Client` per instance, не thread-pooled singleton | `orchestrator/client.rs:36-46` | socket leak если client дробится |
| 11 | `whisper` транскрипция держит модель в RAM между вызовами — singleton в `voice/mod.rs:26` уже, но HashMap-keyed без LRU | `voice/mod.rs:121-140` | +150 MB-1.5 GB если загрузить 3 модели подряд |
| 12 | SQLite `busy_timeout=5000` без `synchronous=NORMAL` PRAGMA на pool | `db.rs:33-38` | лишний fsync каждый commit |
| 13 | Watcher `inner: HashMap<agent_id, …>` не prune'ит | `watcher/supervisor.rs:60-65` | утечка в 10s агентов → 100s |
| 14 | `Engine::runtimes` HashMap тоже — `kill` чистит, EOF чистит, но orphaned при panic в reader | `agentd/engine.rs:130`, `agentd/engine.rs:371` | до ~1 KB/орфан |
| 15 | `Mailbox::list_thread` без индекса `(thread_id, read_at)` | `db.rs:275-277` (idx_mbox_thread без read_at) | full scan на большом mailbox'е |
| 16 | `VoicePipeline.whisper: HashMap<String, Arc<Whisper>>` без TTL/LRU | `voice/mod.rs:26` | OOM если пользователь свичнул 5 моделей |

## TL;DR (P2)

| # | Проблема |
|---|----------|
| 17 | `tracing_subscriber::fmt()` без writer rotation — лог в stderr → journald / journal; cap отсутствует |
| 18 | `cpal::Stream` нигде не `Drop` явно при cancel — зависит от того, кто владеет stream |
| 19 | Tauri `webview.eval()` не используется (вместо этого events) — OK |
| 20 | `metrics` crate отсутствует — нет ни единого gauge/histogram для HUD |

---

## P0: Memory leaks / unbounded growth

### L1. `Engine::last_stdout` — orphan entries
**File:** `src-tauri/src/agentd/engine.rs:131`, `336`, `419`

```rust
last_stdout: Arc<Mutex<HashMap<String, Instant>>>,
...
last_stdout.lock().insert(agent_id_for_reader.clone(), Instant::now());
```

- **Проблема:** HashMap заполняется на каждом stdout-chunk. При Exit
  агент удаляется из `runtimes` (line 363), но соответствующая запись в
  `last_stdout` — **никогда**. `Engine::kill` тоже не чистит.
- **Оценка:**
  - 32 B (String heap + inline + Instant) на агента.
  - При 100 spawned/killed за сессию — 3.2 KB. Не катастрофа, но при
    долгой сессии (PigIDE переживает broker — `kill_all` это no-op)
    растёт неделями.
  - +200 B/sec на 10 chunk/sec/agent = 2 KB/s/agent = 17 MB/час на 10
    активных агентов.
- **Fix:** в `kill` и в EOF-ветке reader thread:
  ```rust
  self.last_stdout.lock().remove(&agent_id);
  ```
  После `runtimes.lock().remove(...)` на engine.rs:363.

### L2. `AgentManager.last_stdout` в PigIDE-процессе (UI side)
**File:** `src-tauri/src/agent.rs:162`, `642`

```rust
last_stdout: Arc<Mutex<HashMap<String, Instant>>>,
```

Точно такая же проблема, как L1, но в PigIDE: pump-task на каждом
`Event::Stdout` делает `insert` (line 642), а `Event::Exit` лишь эмитит
`EV_AGENT_EXIT` — **не удаляет** запись.

- **Оценка:** аналогично L1, ~10–20 KB за день при постоянном churn.
- **Fix:** в матче `Event::Exit` добавить `last_stdout.lock().remove(&agent_id)`.

### L3. `Engine::runtimes` — orphaned entries при panic в reader
**File:** `src-tauri/src/agentd/engine.rs:130`, `320-369`

Reader thread:
```rust
let mut log_file = std::fs::OpenOptions::new()...open(&log_path).ok();
let mut buf = vec![0u8; READ_BUF_SIZE];
loop {
    match reader.read(&mut buf) { ... }
}
// EOF cleanup
runtimes.lock().remove(&agent_id_for_reader);
```

- **Проблема:** если reader thread **паникует** (например, при OOM в
  `Arc::new(buf[..n].to_vec())` line 346), `remove` не вызывается,
  `last_stdout` не очищается, событие `Exit` не отправляется. Runtime
  остаётся в HashMap **навсегда**.
- **Оценка:** редкая (panic), но и бессрочная утечка: 1 Runtime ≈
  ~1–5 KB metadata + открытый master fd + writer Arc.
- **Fix:** обернуть loop в `std::panic::catch_unwind` или хотя бы
  использовать `scopeguard`/`DropGuard` с cleanup-логикой.

### L4. `broadcast::Sender<EngineEvent>` capacity 1024 + max chunks
**File:** `src-tauri/src/agentd/engine.rs:161`

```rust
let (events, _) = broadcast::channel(1024);
```

`Reader thread` шлёт `EngineEvent::Stdout` (до 8 KiB base64) на каждый
chunk. При burst'е (claude streaming разговор, 50 chunks/sec) — за 20
секунд 1024 frames уходят в recycle.

- **Поведение:** `Err(Lagged(n))` на slow receiver, `Lagged` логируется,
  но клиент-потребитель теряет события и **вынужден ребилдить scrollback**
  через `log_tail` RPC.
- **Оценка:** на 10 активных агентах под 50 chunks/sec = 500 events/sec,
  overflow за ~2 сек. Восстановление = `tail(64 KiB)` + emit (медленно).
- **Fix:**
  - Увеличить до 4096 (8 MiB лимит на стороне broker — OK).
  - **Или** per-agent `broadcast::channel(N)` keyed by `agent_id` —
    events изолированы.
  - В текущей архитектуре компромисс: держать 1024, но при `Lagged`
    немедленно слать UI `agent://rebuild` event (single message, заменяет
    scrollback).

### L5. `VoicePipeline.whisper` HashMap без LRU
**File:** `src-tauri/src/voice/mod.rs:26`, `121-140`

```rust
whisper: Mutex<HashMap<String, Arc<whisper::Whisper>>>,
```

Каждая уникальная `model_id` (small, medium, large-v3) загружает
контекст в RAM:
- `small` ~ 480 MiB (CPU GGML, без GPU)
- `medium` ~ 1.5 GiB
- `large-v3` ~ 3.1 GiB

Текущая логика: `evict_model()` есть, но **не вызывается автоматически**
при смене модели. Пользователь с `small → medium → small` оставляет в
RAM 480 + 1500 = ~2 GB.

- **Оценка:** до **3 GB лишней RAM** на 3 модели, которые в сумме
  невозможно держать в типичной системе.
- **Fix:** перейти на `lru::LruCache<String, Arc<Whisper>>` с capacity=1
  (или 2 для GPU). При вставке evict + явный drop. На `evict` ставить
  `Arc::strong_count() == 1` check + задержку drop.

### L6. `cpal::Capture` I16/U16 paths без upper-bound
**File:** `src-tauri/src/voice/capture.rs:88-145`

F32 path имеет 60s cap (line 90):
```rust
if s.samples.len() > 60 * 48_000 { ... }
```

**I16 и U16 пути** (lines 98-145) **не имеют cap** — только `clear()`
в `start()`.

- **Оценка:** при 48 kHz stereo f32 = 384 KB/sec, забытый stop
  (Ctrl+C, panic в whisper init) → 22 MB/мин. Не критично (capture
  отдельный объект), но при `start() → whisper.err → retry` без stop
  копится.
- **Fix:** один helper `append_samples(&mut Vec<f32>, &[T])` + cap check
  в одном месте.

### L7. `mpsc::unbounded_channel` для chat deltas
**File:** `src-tauri/src/orchestrator/mod.rs:484`

```rust
let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
```

Если `forwarder` task отвалится (UI window destroyed, app handle dropped),
producer (`provider.chat_stream`) продолжит слать — **unbounded**, пока
не съест RAM.

- **Оценка:** при chunk-rate 50/sec, текст ~ 100 B = 5 KB/sec. Час
  работы = 18 MB. День = 432 MB. Не unbounded на практике (provider
  имеет timeout), но unbounded **по контракту** — потенциальный риск.
- **Fix:** `mpsc::channel::<String>(512)` + на стороне provider
  `try_send` и при `Full` дропать delta (emit'ить только последний).

### L8. `phantom_log.jsonl` append-only
**File:** `src-tauri/src/orchestrator/phantom.rs:136-152`

```rust
if let Ok(mut f) = std::fs::OpenOptions::new()
    .create(true).append(true).open(&path)
{
    let _ = writeln!(f, "{}", line);
}
```

Каждое phantom-срабатывание = +1 строка (~280 B). При шторме (плохой
промпт / модель «галлюцинирует tool calls» 100 раз в минуту) = 28 KB/min,
1.6 MB/час.

- **Оценка:** в normal use безобидно (~10 KB/день). В runaway prompt
  loop — 100+ MB/час, swap-thrash на 4 GB RAM.
- **Fix:**
  - LRU rotation: при `len > 10 MB` rename `phantom_log.jsonl` →
    `phantom_log.1.jsonl` (max 3 rotation, удалять старые).
  - Или `tracing` target `orchestrator::phantom` — drop to `tracing-appender`
    с rotation 10 MiB × 3.

### L9. Agent per-task log on disk
**File:** `src-tauri/src/agentd/engine.rs:320-336`

```rust
let mut log_file = std::fs::OpenOptions::new()
    .create(true).append(true).open(&log_path).ok();
...
if let Some(f) = log_file.as_mut() {
    let _ = f.write_all(&buf[..n]);
}
```

- **Проблема:** лог файл `<data_local>/pigide/agents/<id>.log` растёт
  неограниченно. `claude` с большим response может дать 5–10 MB на
  один turn; long-lived claude tile с 50 turn'ами = 250–500 MB.
- **Оценка:** 500 MB × 10 активных агентов = 5 GB диска, инодов,
  file descriptor'ов (хоть они закрываются при exit).
- **Fix:**
  - В `kill()` и EOF добавить `keep_last_bytes: usize` truncate.
  - Или внешний logrotate — но Tauri app не в `/etc/logrotate.d`.
  - Проще всего: `f.set_len(0)` при достижении `> 50 MB`, keep last
    1 MB via `f.seek(SeekFrom::End(-1MB))` → `f.set_len(...)`.

### L10. `MemoryService` rebuild_links — full-workspace scan per write
**File:** `src-tauri/src/memory/service.rs:281-323`

```rust
let mut stmt = conn.prepare(
    "SELECT id, slug, title, aliases_json FROM memory_notes WHERE workspace_root=?1",
)?;
let candidates: Vec<links::Candidate> = stmt
    .query_map(...)
    .collect::<rusqlite::Result<Vec<_>>>()?;
```

- **Проблема:** при каждом `update()` / `create()` / `append_section()` —
  SELECT всех notes в workspace. На 1000-note workspace = 1000-row
  scan + deserialize JSON × 2.
- **Оценка:** +10–50 ms per write, +200 KB-2 MB JSON parse per
  per-1000-notes workspace.
- **Fix:** индексировать candidates в `Arc<RwLock<HashMap<slug,
  Vec<note>>>>` поверх SQLite, invalidate on watcher event.

---

## P0: CPU hotspots

### H1. Whisper inference: per-call state allocation
**File:** `src-tauri/src/voice/whisper.rs:88-129`

```rust
let mut state = self.ctx.create_state()?;
```

- **Проблема:** `create_state` на каждый `transcribe()` call — аллокация
  внутренних буферов (mel spectrogram, KV cache, decode state). При
  hotkey-style повторных activations (10 транскрипций/мин) — лишний CPU.
- **Оценка:** +30–80 ms overhead per call. На `large-v3` это незаметно
  (full pass = 2–5 s), но на `small` (200–400 ms) — 25% overhead.
- **Fix:** thread-local `tls_thread_local!` + `RefCell<Option<WhisperState>>`
  per thread. Whisper state можно reuse'ить через `state.full()` (это
  reentrant в whisper.cpp).
- **Threads:** `n_threads = num_cpus/2` (line 99) — для CPU 4 cores = 2
  threads. На 10-core машине = 5, что ОК; но при **нескольких**
  одновременных `transcribe` (что невозможно сегодня — single
  VoicePipeline) это уже конкуренция.

### H2. Reader thread: per-chunk `Arc::new(Vec<u8>)` allocation
**File:** `src-tauri/src/agentd/engine.rs:346`

```rust
let chunk = Arc::new(buf[..n].to_vec());
let _ = events.send(EngineEvent::Stdout { ... });
```

- **Проблема:** `buf[..n].to_vec()` копирует до 8 KB на каждый
  chunk; `Arc::new(...)` оборачивает. Потом клиент делает
  `data_b64.encode(&*data)` — ещё одно копирование.
- **Оценка:** при 50 chunks/sec × 4 KB avg = 200 KB/s allocations
  на одного агента. 10 агентов = 2 MB/s. malloc/free pressure → CPU.
- **Fix:** `bytes::Bytes` (cheap clone, refcounted) или arena-allocator
  per-agent. Можно короче: `events.send(EngineEvent::Stdout { data: Bytes::from(buf[..n].to_vec()) })`.

### H3. Orchestrator: `build_system_prompt` format-heavy allocations
**File:** `src-tauri/src/orchestrator/mod.rs:161-232`

```rust
for w in &workspaces {
    s.push_str(&format!(
        "  {} id={} name={:?} agents={}\n",
        marker, w.id, fence::neutralize(&w.name), w.agent_count
    ));
}
```

- **Проблема:** на каждый tool-loop iteration (max 6 per turn) —
  пересобирается весь system prompt: list всех workspaces, list всех
  agents (sync RPC через broker → `block_on_safely`), list tasks.
- **Оценка:**
  - `block_on_safely` per `self.agent_mgr.list(&w.id)` = 2–10 ms × N
    workspaces.
  - `format!` per agent: ~100 ns × 10 agents × 6 iterations = 6 µs —
    незначительно.
  - Главное: **синхронный RPC в async loop** — пока broker отвечает,
    orchestrator task висит.
- **Fix:** кэшировать `last_agents_list: Arc<RwLock<Vec<Agent>>>` с TTL
  1s; обновлять из event pump.

### H4. `sanitize_fts_query` per search
**File:** `src-tauri/src/memory/service.rs:700-757`

```rust
let cleaned: String = q.chars().map(|c| ...).collect();
for raw_tok in cleaned.split_whitespace() { ... }
```

- **Проблема:** O(n) over query + Vec allocations. На 2 KiB query =
  ~10 µs. Низкий приоритет, но на каждом keystroke в searchbox
  (debounced) — лишний CPU.
- **Fix:** кэш последних (q, cleaned) пар, LRU 16.

### H5. `Memory::graph` full table scan + per-edge serialize
**File:** `src-tauri/src/memory/service.rs:584-622`

```rust
let nodes: Vec<GraphNode> = stmt_n.query_map(...)?.collect()...?;
let links: Vec<GraphEdge> = stmt_e.query_map(...)?.collect()...?;
```

- **Проблема:** читает ВСЕ nodes + ВСЕ links в workspace, без пагинации.
  На 5000-note workspace = 5000 nodes + 20000 edges → 2–5 MB JSON
  payload, +100 ms Tauri→webview передача + react-flow рендер
  5000 нод.
- **Оценка:** при каждом открытии графа (любой navigate) — 100–500 ms
  CPU spike.
- **Fix:**
  - Cap nodes/edges в API (e.g. top 1000 nodes, фильтр по kind).
  - Передавать `binary` (postcard / bincode) вместо JSON.
  - Backend pagination — `graph_window(root, from, to)`.

### H6. Phantom detection: lowercase entire content per check
**File:** `src-tauri/src/orchestrator/phantom.rs:97-103`

```rust
pub fn is_phantom(content: &str, has_tool_calls: bool) -> bool {
    if has_tool_calls { return false; }
    let lower = content.to_lowercase();
    TRIGGER_PHRASES.iter().any(|p| lower.contains(p))
}
```

- **Проблема:** `to_lowercase()` аллоцирует новый `String` на каждый
  assistant turn (max 6 per user turn). На 10 KB response = 10 KB alloc
  + 30 phrases × O(n) = 300 KB scanned.
- **Оценка:** ~50 µs per call. На 100 turns/day = 5 ms. Не горячо.
- **Fix:** `unicase` crate для case-insensitive `contains` без аллокации,
  или `aho-corasick` для multi-pattern match за O(n+m).

### H7. `commands.rs` JSON serialization on big payload
**File:** `src-tauri/src/commands.rs:246` (agent_log_tail) and many more

Каждый Tauri command сериализует ответ через `serde_json::to_string` →
Tauri → webview postMessage. На `list_memories` returning 500 notes:
500 × ~500 B = 250 KB JSON + base64 in scripts = 500 KB IPC per call.

- **Fix:** добавить `summary_only` mode для больших list endpoints.

### H8. `Fence::neutralize` per-message on tool results
**File:** `src-tauri/src/orchestrator/mod.rs:315`, `fence.rs`

```rust
let body = truncate_for_model(&fence::neutralize(&m.content), 4000);
```

- **Проблема:** `fence::neutralize` regex scan per character/tool-result.
  4000-char tool result = 4000 regex matches. На 6 iterations × 3 tool
  calls = 18 × 4000 = 72000 regex ops per turn.
- **Оценка:** ~500 µs per turn. Не горячо в абсолюте, но это в
  hot path.
- **Fix:** early-return если content length < 100 chars (heuristic: short
  tool results are unlikely to contain injection markers).

---

## P1: Connection pool / runtime tuning

### P1-1. r2d2_sqlite pool
**File:** `src-tauri/src/db.rs:33-40`

```rust
let manager = SqliteConnectionManager::file(&path).with_init(|c| {
    c.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")
});
let pool = Pool::builder().max_size(8).build(manager)?;
```

**Проблемы:**
- `min_idle=0` (default) — на каждом холодном пути первый запрос
  открывает connection (~10–50 ms WAL+attach).
- `busy_timeout=5000` ок, но `synchronous=NORMAL` не выставлен
  (default=FULL → fsync every commit).
- `journal_mode=WAL` уже в migrate_one (line 26), но `with_init`
  пере-выставляет только `foreign_keys` + `busy_timeout` — ОК.
- `cache_size` (default 2 MiB) не выставлен — на больших запросах
  disk I/O.

**Рекомендация:**
```rust
let pool = Pool::builder()
    .max_size(8)
    .min_idle(2)                    // keep 2 warm
    .connection_timeout(Duration::from_secs(3))
    .build(manager)?;
```

В `with_init` добавить:
```sql
PRAGMA synchronous = NORMAL;       -- WAL = safe with NORMAL
PRAGMA cache_size = -32000;        -- 32 MiB
PRAGMA temp_store = MEMORY;        -- temp tables in RAM
PRAGMA mmap_size = 134217728;      -- 128 MiB mmap (fast read on indexed)
```

`PRAGMA mmap_size` даст -50% read latency на big SELECTs (graph,
list_memories).

### P1-2. reqwest Client singleton
**File:** `src-tauri/src/orchestrator/client.rs:36-46`

```rust
pub fn new(base_url: String, model: String, api_key: Option<String>) -> Self {
    Self {
        ...,
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("reqwest client"),
    }
}
```

`OmniClient::new()` вызывается из `Orchestrator::build_provider()`
каждый tool-loop iteration (line 115) → новый reqwest::Client →
новый connection pool → **socket leak** под нагрузкой.

**Проблема:** `reqwest::Client` держит internal connection pool +
idle connections + TLS sessions. Создание per-call = drop the pool
после every request, kill idle keep-alives, force fresh TCP/TLS
handshake.

**Оценка:** на 10 active agents × 6 iterations × 1 req = 60 new clients
per user turn. Каждое ~10–50 ms TLS handshake на OpenAI/Anthropic.

**Fix:** в `AppState` добавить `http_client: OnceCell<reqwest::Client>`;
или кэш `Arc<DashMap<(base_url), Arc<reqwest::Client>>>`. Если
разные `base_url` для разных провайдеров — кэш keyed by base_url.

```rust
struct OmniClient {
    http: Arc<reqwest::Client>,  // shared
    ...
}
```

### P1-3. Tokio worker_threads
**File:** нет явной конфигурации

`tauri::async_runtime` использует default tokio runtime, который сам
берёт `num_cpus`. ОК. Но:

- `spawn_blocking` (для whisper, file I/O) использует default
  blocking pool = **512 threads max**, но они создаются по требованию
  (16 KiB stack each = 8 MiB максимум).
- Voice pipeline `transcribe` → `spawn_blocking` (voice/mod.rs:144).
  На одновременных transcriptions — конкуренция за blocking pool.
  Сегодня single-user = OK.

**Рекомендация:** в `lib.rs:run()` после `tauri::Builder` явно
инициализировать custom runtime:
```rust
#[cfg(not(mobile))]
let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(num_cpus::get())
    .max_blocking_threads(64)         // whisper + file I/O
    .thread_name("pigide-tokio")
    .enable_all()
    .build()?;
let _guard = rt.enter();  // or use tauri::async_runtime::set(rt.handle().clone())
```

### P1-4. blocking on tokio worker via `block_in_place`
**File:** `src-tauri/src/agent.rs:67-72`

```rust
fn block_on_safely<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(fut)),
        Err(_) => tauri::async_runtime::block_on(fut),
    }
}
```

- **Проблема:** `block_in_place` берёт worker thread out of pool, ждёт
  future. На 10 одновременных Tauri commands = 10 workers заблокированы.
  Если `num_cpus=4` — UI зависает.
- **Оценка:** каждый Tauri command вызывает AgentManager API →
  `block_on_safely(client.list())` = ~5–20 ms. Не страшно разово,
  но при burst'е (open workspace = 50 commands) = 500 ms-1s.
- **Fix:** Tauri commands должны быть `async fn`, использовать
  `tauri::AppHandle` + `.await` напрямую вместо `block_on_safely`.

### P1-5. Tauri webview windows
**File:** `src-tauri/src/lib.rs:223-518`

Tauri default = 1 webview. Workspace TilingArea = DOM tiling, не
multi-webview. **OK**, не leak.

### P1-6. axum (MCP) runtime
**File:** `src-tauri/src/mcp/server.rs`

`MCP autostart` запускает axum-сервер в `tauri::async_runtime::spawn`
на `127.0.0.1:20129` (default). Single instance, max connections =
default (unlimited). На 100 подключений = 100 tasks. ОК для MCP.

---

## P1: Swap thrash sources

### S1. Whisper model loading strategy
**File:** `src-tauri/src/voice/whisper.rs:46-85`, `voice/mod.rs:121-140`

Текущая стратегия: `Mutex<HashMap<String, Arc<Whisper>>>` — каждая
модель загружается один раз, держится до explicit `evict_model()`.

**Проблема:**
- `WhisperContext` для CPU GGML `large-v3` ≈ 3.1 GiB.
- 2 модели loaded = 6 GiB RSS. Типичный ноутбук — 16 GiB. + OS + PigIDE
  + webkit2gtk = 8 GiB. Остаётся 8 GiB на page cache, что OK.
- 3 модели = OOM territory.
- GPU backend: `large-v3` CUDA = 3.1 GiB VRAM. Загрузка падает если
  VRAM < 3.1 GiB, что не отлавливается gracefully (только лог).

**Оценка swap risk:** на 4 GB RAM машинах, если `large-v3` загружен и
foreground process пытается malloc — kernel начинает swap out inactive
pages (включая hot `WhisperContext`), что даёт 5–20 sec pauses.

**Fix:**
- Default model = `small` (~480 MiB) — уже.
- `evict_model` сделать автоматическим при `last_used > 5 min`.
- Добавить `idle_eviction: bool` setting — при idle (5 min после
  последнего transcription) → `Arc::try_unwrap()` → drop context.
- На UI: показывать `voice.model_loaded: bool` индикатор.

### S2. SQLite WAL pruning
**File:** `src-tauri/src/db.rs` (нет WAL checkpoint в коде)

`journal_mode=WAL` (line 26) — ОК. Но:
- `wal_autocheckpoint` default = 1000 pages. На 4 KiB page = 4 MiB.
- При active PigIDE (orchestrator_chat 100 rows/min) WAL растёт до
  4 MiB, потом checkpoint. Fine.
- **Но:** если PigIDE падает без `PRAGMA wal_checkpoint(TRUNCATE)` —
  next boot увидит stale WAL, который sqlite почистит автоматически
  но возьмёт 100–500 ms на boot.
- **И:** `chat_queue` rolling DELETE (mark_done / mark_failed) —
  фрагментация + bloat в `orchestrator_chat` (нет DELETE, history
  forever).

**Fix:**
- На startup: `PRAGMA wal_checkpoint(TRUNCATE);` — clamp WAL to 0.
- В `chat_sessions::delete`: cascade уже чистит orchestrator_chat (FK
  CASCADE), но старые сессии никогда не удаляются автоматически.
- Background job: раз в час `DELETE FROM orchestrator_chat WHERE created_at
  < date('now', '-30 days')` — configurable retention.

### S3. `.pigmemory/` рост
**File:** `src-tauri/src/orchestrator/phantom.rs:155-160`, `memory/ingest/`

`.pigmemory/phantom_log.jsonl` — без cap.
`.pigmemory/hot.md` — write-through smart-lane.
`.pigmemory/notes/` — user notes, write-once.

**Fix:** для phantom_log — rotation. Для hot.md — bounded at 64 KB,
ring buffer в DB вместо file.

### S4. CodeMirror document sizes
**File:** frontend (не входит в этот аудит) — `frontend/src/components/pigmemory/`
использует editor. Без lazy-loaded / virtualized rendering 50 KiB
markdown = 200 ms render. Рекомендация: virtualize (CodeMirror-virtualization
или textarea + line numbers для small notes).

### S5. PTY log files
**File:** `src-tauri/src/agentd/engine.rs:312-336`

Per-agent log `<data_local>/pigide/agents/<id>.log` — без rotation.
Описано в L9.

### S6. tracing log to stderr
**File:** `src-tauri/src/lib.rs:116-121`

```rust
let _ = tracing_subscriber::fmt()
    .with_env_filter(...)
    .try_init();
```

stderr → systemd journal на cachyos. Без explicit rotation, но systemd
rotates journal сам (default 4 files × 100 MiB). OK.

**Но:** `pigide-agentd` отдельный `tracing_subscriber::fmt().init()` —
если crash, последние логи в stderr journalctl (но systemd unit
должен быть настроен). Verify `journalctl -u pigide-agentd`.

---

## P2: Process supervision improvements

### PS1. PTY cleanup: master fd + slave leak on panic
**File:** `src-tauri/src/agentd/engine.rs:280-369`

```rust
let child = pair.slave.spawn_command(cmd).map_err(...)?;
drop(pair.slave);
let writer_raw = pair.master.take_writer().map_err(...)?;
let writer = Arc::new(Mutex::new(writer_raw));
let mut reader = pair.master.try_clone_reader().map_err(...)?;
```

- **Проблема:** `pair.master` держится в `Runtime.master`. На `kill()`
  line 458-459:
  ```rust
  drop(rt.writer);
  drop(rt.master);
  ```
  OK. Но при **panic в reader thread** между `let mut log_file = ...` и
  `runtimes.lock().remove(...)` — `rt.master` не дропнут, fd leak.
  На 100 panics = 100 leaked PTY master fds (kernel limit 1024 per proc).

**Fix:** `Runtime` владеет всеми ресурсами через `Drop`:
```rust
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.child.lock().kill();
        let _ = self.child.lock().wait();
    }
}
```

Но `Runtime` не дропается при panic, потому что HashMap entry не
удалён. См. L3.

### PS2. Zombie reaping
**File:** `src-tauri/src/agentd/engine.rs:447-451`

```rust
{
    let mut child = rt.child.lock();
    let _ = child.kill();
    let _ = child.wait();
}
```

`child.wait()` reap'ит — OK. `SIGKILL` → child dies → wait() returns
exited status → no zombie. **Хорошо.**

Но: **on EOF** (lines 357-368) — child умер сам, но `try_wait` не
вызывается явно. Reader thread не делает `child.wait()` в EOF path
только удаляет из HashMap. Child reaps автоматически при `drop(child)`
(line 365 → 363) — поскольку `Arc<Mutex<Box<dyn Child>>>` дроппится
при удалении Runtime из HashMap.

**Проверить:** при manual `kill()` (line 449) `child.lock().wait()`
вызывается, OK. При natural EOF (line 363) `Arc::drop` на child →
`Child::drop` → SIGCHLD handled by reaper в portable_pty. **OK**.

### PS3. detach cleanup
**File:** `src-tauri/src/agentd/server.rs:305-316`

`Op::Detach` aborts subscribe task + drops tx + returns. Connection
task ends. Но child processes не дропнуты — broker держит runtimes.
**OK by design.**

### PS4. agentd broker exit path
**File:** `src-tauri/src/bin/pigide-agentd.rs:99-109`

```rust
if std::env::var("PIGIDE_AGENTD_SHUTDOWN_ON_SIGNAL")... == "1" {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = std::fs::remove_file(&socket_clone);
        std::process::exit(0);
    });
}
```

- **Проблема:** `std::process::exit(0)` **не запускает Drop**, не
  закрывает socket gracefully, не ждёт active connections. Acceptable
  since SIGINT = "die now", но in-flight RPCs get connection reset.
- **Fix:** explicit `Drop for PidGuard` уже есть, sock file deleted by
  `std::process::exit`. OK.

### PS5. nohup/setsid
**File:** `src-tauri/src/agentd/supervisor.rs:138-171`

`spawn_detached` использует `setsid() + chdir("/") + dup2(/dev/null)`.
**Хорошо.** Но: вторая fork (отсутствует, по комменту "не нужно") — если
PigIDE SIGKILL'нут, broker может re-acquire controlling tty. Минорный
риск, никого не волнует.

### PS6. Reaped PID bookkeeping
**File:** нет — broker не трекает PIDs (через `child.id()` available в
`portable_pty` v0.8+, но не используется). Для debug — `Op::ListAll`
возвращает только `id`, `workspace_id`, etc. — не `pid`.

**Fix (low pri):** add `pid: u32` to `AgentInfo` + surface in
`list_agents` Tauri command for `ps` correlation.

---

## P2: Disk I/O patterns

### D1. `synchronous=NORMAL` уже через default?
**File:** `src-tauri/src/db.rs:33-40`

`with_init` НЕ выставляет `synchronous`. Default = FULL (один fsync per
commit, медленно). С WAL = NORMAL достаточно (no data loss on single
process crash, only on power loss).

**Fix:** добавить `PRAGMA synchronous = NORMAL;` в `with_init`.

### D2. WAL mode OK
`PRAGMA journal_mode = WAL;` в migrate_one (line 26) + with_init
(хотя redundant). OK.

### D3. Temp file cleanup
SQLite temp tables: `PRAGMA temp_store = MEMORY` рекомендуется (см. P1-1).

В коде PigIDE нет explicit `std::env::temp_dir()` использования кроме
Tauri internals. OK.

### D4. Log file rotation
Tracing: см. S6.
Per-agent PTY log: см. L9.
phantom_log: см. L8.

### D5. `connection_event_pump` infinite loop
**File:** `src-tauri/src/agent.rs:638-715`

```rust
tauri::async_runtime::spawn(async move {
    loop {
        match events.recv().await {
            ...
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(...);
                continue;
            }
            Err(_) => break,
        }
    }
});
```

- **Проблема:** `Err(_)` break — task ends. При broadcast sender drop
  (broker down) task exits silently. **Нет** `shutdown_tx` механизма
  → при `kill_all` (no-op) или broker restart pump не поднимается
  заново.
- **Fix:** pump должен подключаться повторно при broker reconnect.
  Сегодня нет reconnect logic в `AgentManager` — однократное
  подключение.

---

## P2: Network / IPC

### N1. WebSocket frame size
MCP server (axum-based, см. `mcp/server.rs`) — likely JSON-RPC over
HTTP, не WebSocket. Default axum WebSocket frame = unlimited,
configurable per-message. OK.

### N2. SSE chunk size
OmniRouter `chat_completions_stream` (orchestrator/client.rs:112-260) —
accumulate в `String buf` (line 167) до `\n\n`. На 100 KB SSE event
с `data: ...` одним блоком — `buf` растёт до 100 KB. На длинных
completion может накопиться > 1 MB до `[DONE]`.

**Fix:** process SSE events as they come, не буферизировать chunk-
от-chunk. Уже сделано: `while let Some(end) = buf.find("\n\n")`. OK.

### N3. Reconnect storms
При broker crash — `AgentClient` read_loop exits, broadcast sender
dropped, pump task ends. Frontend видит `agent.spawned` events freeze,
но **никакого reconnect не происходит**. UI должен детектировать
disconnect и переинициализировать.

**Fix:** в `AgentManager` добавить background reconnect task:
- Каждые 5s: `try_connect`. При success → `restore_session()`.
- Expose `AgentManager::broker_connected() -> bool` для UI.

---

## P2: Tauri-specific

### T1. `webview.eval()` — не используется, OK
Все коммуникации через Tauri events (`emit`). Нет synchronous JS exec.

### T2. Asset loading — bundle size
**File:** `frontend/dist/...` (после build)

Tauri default bundle: WASM? Не используется. JS bundle = Vite output.
Frontend deps: React + TipTap/CodeMirror + xterm.js + react-flow
= ожидаемо 5–10 MB JS minified. Webview cold start = 300–800 ms на
chromium webkit. **OK** для desktop.

### T3. Tray icon
Нет tray icon в lib.rs setup. Не leak.

### T4. System tray/menu memory
**File:** `src-tauri/src/commands.rs` + `state.rs`

`AppState` содержит 14 Arc-полей. Каждое Arc = 8 B, итого 112 B на
один AppState. OK.

Но `mcp: Arc<McpServerHandle>` (line 215) — handle держит
`running: AtomicBool`, `port: AtomicU16`, `bind_addr: RwLock<Option<...>>`.
`bind_addr: Option<SocketAddr>` = 16 B. OK.

### T5. Webview count
1 webview (main window). TilingArea — DOM tiles, не webview. OK.

---

## P2: Lock contention

### LC1. `Engine::runtimes` mutex
**File:** `src-tauri/src/agentd/engine.rs:130, 248, 363, 379, 444`

`parking_lot::Mutex<HashMap<String, Runtime>>` — держится в
`spawn`, `write`, `resize`, `kill`, `list_*`. `write` берёт lock +
clone Arcs + отпускает (line 379-389), поэтому крит секция короткая.
**OK.**

### LC2. `Engine::last_stdout` mutex
Каждый chunk = `lock().insert(...)`. 50 inserts/sec/agent = 500
locks/sec. parking_lot::Mutex на uncontended path = 10 ns. 5 µs/s
overhead. **OK.**

### LC3. `Engine::events` (broadcast) — internal lock
`tokio::sync::broadcast::Sender::send` — internal Mutex. Slow
consumer (PigIDE на UI thread через 2 RPC hops) → `Lagged` events.

### LC4. `Orchestrator.abort_triggers`
**File:** `src-tauri/src/orchestrator/mod.rs:51`

`Mutex<BTreeMap<u64, oneshot::Sender<()>>>` — register on turn start,
deregister on turn end. Lock contention = 1 lock per turn. OK.

### LC5. `AgentManager.connected: Mutex<Option<ConnectedState>>`
**File:** `src-tauri/src/agent.rs:167`

Каждый method (`client_or_err`, `write`, `kill`, `list`) берёт этот
mutex чтобы достать clone client. Mutex held < 1 µs (clone is cheap).
**OK.**

### LC6. `WatchdogWatcher.inner: RwLock<WatcherInner>`
**File:** `src-tauri/src/watcher/supervisor.rs:73, 109-119`

`bucket()` (line 109) — read lock, check hashmap, on miss write lock
+ insert. **OK** для 10s of agents. **P0 для 100s+** — upgrade to
`DashMap<String, Arc<TokenBucket>>`.

### LC7. `MemoryService.rebuild_links` per write
Уже отмечено L10.

---

## Бенчмарк-цели

| Метрика | Target | Измеряется |
|---------|--------|------------|
| RSS (PigIDE, idle, 0 agents) | ≤ 200 MB | `ps -o rss= -p $(pgrep -f pigide)` |
| RSS (PigIDE, 10 active agents) | ≤ 400 MB (200 baseline + 20/agent) | same + agent count |
| RSS (pigide-agentd, 10 agents) | ≤ 250 MB (engine + 10 PTYs) | `pgrep -f pigide-agentd` |
| RSS (whisper ctx, large-v3) | ≤ 3.1 GiB | `pgrep -f pigide` after transcribe |
| CPU (idle, 0 agents) | ≤ 2 % (1 core) | `top -b -n 1` |
| CPU (10 active agents, idle) | ≤ 5 % (1 core avg) | `top -b -n 1` |
| CPU (10 agents, busy) | ≤ 50 % (1 core avg, 200 % all cores) | stress test |
| CPU (whisper transcribe, large-v3) | < 2× realtime (1 min audio < 2 min CPU) | custom |
| Swap usage | 0 KB (no swap pressure) | `free -m` |
| PTY read latency (PigIDE → broker) | < 50 ms p99 | custom metric |
| Whisper transcribe (1 min audio, large-v3 CPU) | < 2 min | `time` |
| DB query latency (PigIDE, hot path) | < 5 ms p99 | tracing |
| LLM request (OmniRouter) | < 200 ms TTFB p50 | HTTP trace |
| Startup (PigIDE → UI ready) | < 2 s cold | `time pigide` |
| Broker spawn | < 500 ms | socket connect timer |
| log file size per agent | < 50 MB (rotate/truncate) | `du` |

---

## Метрики для production

### HUD (visible в Tauri status bar)
- `RSS_MB` — gauge, color: green < 400, yellow < 800, red ≥ 800
- `CPU%` — sparkline 60 s
- `agents_running` / `agents_max` (предел из настроек)
- `whisper_loaded` — model id или "none"
- `broker_connected` — boolean + uptime
- `db_lag_ms` — последний `journal_mode=wal` checkpoint age
- `llm_spend_today_usd` — из `meter::Meter`

### Логировать (info level)
- `agent.spawned { id, type, workspace_id }` — counter increment
- `agent.killed { id, uptime_secs }`
- `whisper.load { model, size_mb, took_ms }`
- `whisper.transcribe { model, audio_ms, took_ms, rtf }`  ← RTF = realtime factor
- `broker.connect { spawned: bool, took_ms }`
- `broker.disconnect { reason }`
- `db.query { sql_hash, took_ms, rows }` — sample 1% only

### Логировать (warn level)
- `db.slow { took_ms, sql }` — > 50 ms
- `whisper.fallback { from: gpu, to: cpu, reason }`
- `agent.lag { agent_id, expected_first_stdout_ms, actual_ms }` — readiness timeout
- `broadcast.lagged { n }` — уже есть
- `phantom.detected { model, snippet_preview }` — уже есть

### Алертить (error → Sentry/Datadog/etc, если подключён)
- `whisper.init_failed`
- `broker.connect_failed_after_spawn`
- `db.corruption` (PRAGMA integrity_check failed)
- `oom.approaching` (RSS > 90% of available RAM)
- `swap.thrash` (swapin/s > 100/s for 10s)
- `phantom.exhausted` (PHANTOM_MAX_RETRIES hit)
- `token_budget.exceeded` (compact fired + still over)

### Реализация
Без внешних dep — `tracing` (already in) для логов, custom
`AtomicU64`/`AtomicI64` gauges in `AppState` для HUD. Перед тем как
тянуть `metrics` / `prometheus`, профилировать с `perf` (см. ниже).

### Профилирование
```bash
# CPU hotspots (sampled, 99 Hz):
perf record -F 99 -p $(pgrep -f pigide) -g -- sleep 60
perf script | head -100

# Heap allocations:
heaptrack $(which pigide)

# Syscall trace:
strace -c -p $(pgrep -f pigide)

# Lock contention (with debug builds):
#  Add `tracing` spans around mutex acquisitions
#  RUST_LOG=parking_lot=trace
```

---

## Дополнительные рекомендации

### Инструментирование — добавить минимальный `metrics` слой
```rust
// crates/metrics.rs (new)
pub struct Metrics {
    pub rss_bytes: AtomicI64,
    pub cpu_pct: AtomicU32,        // basis points (10000 = 100%)
    pub agents_running: AtomicU32,
    pub whisper_loaded: AtomicU32, // 0 = none, 1 = small, 2 = medium, 3 = large
    pub db_lag_ms: AtomicI64,
    pub llm_spend_cents: AtomicI64,
    pub broadcast_lag_total: AtomicU64,
}
```
Snapshot read by Tauri command `get_perf_stats`, отрендерить в HUD.

### Сборка — release profile
Cargo.toml `[profile.release]` уже должен иметь:
```toml
lto = "thin"
codegen-units = 1
strip = "symbols"
panic = "abort"
opt-level = 3
```
Подтвердить: `cargo build --release --message-format=json | jq`.

### Runtime monitoring
- На Linux cachyos 7.0: `systemd-cgtop` для cgroup-level RSS.
- `cgroup v2` обязательно — `systemd-run --user --scope` для broker.
- `prlimit -p $$ --as=...` если hard limit нужен (но pigide — UI app).

### IPC budget
- Tauri events: payload max ~ 64 MB (postMessage limit), но
  per-event < 1 MB рекомендуется.
- В коде: `EV_AGENT_STDOUT` payload = `{agent_id, data_b64}` — до
  ~16 KB на chunk (8 KB raw + 33% b64 + JSON envelope). OK.
- `EV_CHAT_CHUNK` = `{id, delta}` — delta cap 4 KB OK.

### Frontend suggestions (не в этом аудите, но влияют)
- `frontend/src/components/TilingArea.tsx` — `react-window` /
  virtualized для tiles, иначе 50+ tiles = jank.
- `pigmemory` graph — `react-flow` с min 200 nodes = slow;
  use `WebGL` renderer (`sigma.js` / `cytoscape`) для > 100 nodes.

---

## Контрольные точки (acceptance)

После применения фиксов из P0:

1. `cargo build --release` ок.
2. `cargo test` — все green (whisper mocks, agentd, orchestrator).
3. `cargo clippy --all-targets` — zero new warnings.
4. Manual smoke: `pigide` стартует за < 2 s, 10 agents idle = < 50 MB RSS/agent.
5. Stress: 100 spawns over 5 min → RSS delta < 50 MB (proof no leak).
6. `journalctl -u pigide-agentd` — no `Lagged` warnings at 10 chunks/sec.
7. Whisper: transcribe 30s audio with `large-v3` < 60s.
8. `PRAGMA wal_checkpoint(TRUNCATE)` on idle broker → WAL = 0 bytes.

## Резюме по файлам с действиями

| Файл | Что поменять | Severity |
|------|--------------|----------|
| `agentd/engine.rs` | L1 (cleanup last_stdout), L3 (panic-safe), L4 (per-agent broadcast), L9 (log rotation) | P0 |
| `agent.rs` | L2 (cleanup last_stdout on Exit), N3 (reconnect) | P0/P2 |
| `voice/mod.rs` | L5 (LRU whisper), D5 (idle eviction) | P0/P1 |
| `voice/capture.rs` | L6 (cap I16/U16 paths) | P0 |
| `orchestrator/phantom.rs` | L8 (rotate log) | P0 |
| `orchestrator/mod.rs` | L7 (bounded mpsc), H3 (cache agent list) | P0/P1 |
| `orchestrator/client.rs` | P1-2 (singleton reqwest) | P1 |
| `db.rs` | P1-1 (min_idle, PRAGMAs), S2 (WAL checkpoint), D1 (sync NORMAL) | P1 |
| `commands.rs` | T4 (perf stats command) | P2 |
| `lib.rs` | P1-3 (custom tokio runtime) | P1 |
| `watcher/supervisor.rs` | LC6 (DashMap), L13 (prune dead agents) | P2 |
| `swarm/mailbox.rs` | P1-15 (add read_at index) | P2 |
| `memory/service.rs` | L10 (cache candidates), H5 (paginate graph) | P1 |
| `memory/watcher.rs` | D5 (rebuild on workspace switch) | P2 |
| `bin/pigide-agentd.rs` | PS4 (graceful shutdown вместо exit(0)) | P2 |
