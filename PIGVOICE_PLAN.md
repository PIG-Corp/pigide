# PigVoice — Implementation Plan (Phase B)

**Goal:** sub-300 ms first-visible-token latency from speech to UI, with partial hypotheses streaming directly into the focused input.

This plan is the bridge between [PIGVOICE_RESEARCH.md](./PIGVOICE_RESEARCH.md) and the actual code. Scope is deliberately **narrow** — we are upgrading the existing voice pipeline, not replacing the whole module.

---

## 1. Chosen architecture

```
┌─────────────────┐
│ cpal input      │ 48 kHz (or device default), f32, ring-buffer
│ (callback thr.) │
└─────────┬───────┘
          │ frame chunks (~10–30 ms)
          ▼
┌─────────────────┐
│ Resample → 16k  │ linear (existing impl, simple, low-CPU)
└─────────┬───────┘
          ▼
┌─────────────────┐
│ Silero VAD      │ 512-sample frames @ 16 kHz
│ (ort, in-proc)  │ → SegmentState{Idle, Started, Ended}
└─────────┬───────┘
          │ rolling buffer + segment markers
          ▼
┌─────────────────────┐
│ Streaming decoder   │ tokio task, woken every 250 ms while
│ (whisper-rs)        │ in `Started`. Decodes the trailing
│                     │ window (max 6 s).
└──────────┬──────────┘
           │ each pass → Vec<Token>
           ▼
┌──────────────────────┐
│ LocalAgreement-2     │ stable prefix = LCP of last two passes
│ merger               │ unstable tail = remainder of latest pass
└──────────┬───────────┘
           │ on every commit:
           │   tauri::emit("voice://partial",
           │     { stable, unstable, segment_id })
           │
           │ on VAD `Ended` (≥400 ms silence) OR user stop:
           │   final decode on full segment
           │   tauri::emit("voice://transcript", { text })
           ▼
┌──────────────────────┐
│ Frontend             │ committed → focused element via
│ (App.tsx / VoicePill)│ existing draftInput + injection;
│                      │ unstable tail rendered as dim chip
│                      │ until next event.
└──────────────────────┘
```

### Decisions locked in

- **Engine v1:** whisper.cpp via `whisper-rs` (already shipped). No new heavy dep.
- **VAD:** Silero V5 via `voice_activity_detector` Rust crate (ONNX through `ort`, ~2 MB model bundled).
- **Streaming algorithm:** LocalAgreement-2 (stable prefix = longest common prefix of last 2 hypotheses).
- **Cloud fallback:** Deepgram Nova-3 over WebSocket. Code lands in v1 behind `voice.engine = deepgram`, behind a feature flag at runtime.
- **UI:** new `VoicePill` overlay with a stable+unstable visualization. No invasive change to the chat input.
- **Default model:** `tiny`. `base` available via existing model picker.
- **Default hop:** 250 ms partial cadence, 400 ms silence for endpointing.

### Why this and not the alternatives

- Moonshine is faster but English-only at the sizes we care about; we can ship later as a second engine without ripping anything out.
- faster-whisper would drag in Python; not justified.
- Parakeet is offline / NVIDIA-only; doesn't fit a cross-platform Tauri desktop.

---

## 2. Module layout (rust)

```
src-tauri/src/voice/
├── mod.rs               (existing) wires VoicePipeline. Add `streaming` mode.
├── capture.rs           (existing) extend: ring-buffer push API for streaming.
├── whisper.rs           (existing) extend: re-usable State for streaming passes.
├── streaming.rs         NEW: streaming decoder loop + LocalAgreement-2.
├── vad.rs               NEW: Silero VAD wrapper (segment edges).
├── tokens.rs            NEW: tokenizer/normalizer used by the merger; pure logic.
├── cloud.rs             NEW: Deepgram WebSocket client (opt-in, behind setting).
├── download.rs          (existing) add Silero VAD model download + Moonshine reserve.
├── dictionary.rs        (existing) unchanged.
├── history.rs           (existing) unchanged.
├── inject.rs            (existing) unchanged (still used for final-text auto-paste).
├── hotkey.rs            (existing) extend: emit a `pressed` callback so the UI can
│                                    show the live pill on hotkey-down too.
└── mode.rs              (existing) unchanged.
```

### New types

```rust
// streaming.rs
pub struct StreamingDecoder { /* whisper state, last_hypothesis, segment_id, ... */ }
pub struct StreamingPartial { pub stable: String, pub unstable: String, pub segment_id: u64 }

// vad.rs
pub enum VadEdge { SpeechStart, SpeechEnd, Continue }
pub struct VadStream { /* silero predictor, hangover counters */ }

// tokens.rs
pub fn longest_common_prefix(a: &str, b: &str) -> usize;  // by tokens, whitespace-aware
pub fn merge_partials(prev: &str, cur: &str) -> StreamingPartial;
```

### New events

- `voice://partial` `{ stable: string, unstable: string, segment_id: number }`
- `voice://final` `{ text: string, segment_id: number }` — same payload shape as
  the existing `voice://transcript`; we keep `voice://transcript` for back-compat,
  emit both for one release, then deprecate.
- `voice://engine-error` `{ engine, error }`

### New settings keys

| key | default | purpose |
|-|-|-|
| `voice.engine` | `whisper-streaming` | one of `whisper-streaming`, `whisper-batch`, `moonshine`, `deepgram` |
| `voice.partial_enabled` | `"true"` | toggle partials independent of engine |
| `voice.partial_hop_ms` | `"250"` | how often to re-decode |
| `voice.endpoint_silence_ms` | `"400"` | VAD silence required to commit final |
| `voice.cloud_api_key` | unset | Deepgram key (kept in settings KV, never written to git) |
| `voice.cloud_endpoint` | `wss://api.deepgram.com/v1/listen` | overrideable |

A new migration step (DB migration v10) seeds defaults idempotently.

---

## 3. Frontend changes

### Components

- `VoicePill.tsx` — overlay near the focused input that shows `committed_prefix` (normal weight) + `unstable_tail` (dim italic). Dismissed when state goes back to `idle` and tail is empty.
- `VoiceSettings.tsx` — extend with engine picker, partial toggle, cloud key field (password-masked).

### Store changes (`store.ts`, `types.ts`, `ipc.ts`)

```ts
interface AppStateShape {
  // existing...
  voicePartial: { stable: string; unstable: string; segmentId: number } | null;
  setVoicePartial: (p: AppStateShape["voicePartial"]) => void;
}
export type VoiceEngine = "whisper-streaming" | "whisper-batch" | "moonshine" | "deepgram";
```

`onVoicePartial(cb)` listener mirroring `onVoiceTranscript`. App-level handler:

- on `voice://partial`: update `voicePartial` and (if `inject_enabled` and target is the in-app draft) replace the unstable tail in `draftInput` with the new stable+unstable.
- on `voice://final` / `voice://transcript`: clear `voicePartial`, append final text to `draftInput`, run the existing inject path.

### Visual

```
  ┌────────────────────── chat input ─────────────────────┐
  │ How do I  ◀ stable                                    │
  │ run the migration ◀ unstable (italic, dim)            │
  └───────────────────────────────────────────────────────┘
       ●  recording   00:03   wpm 124
```

Pill anchors to the bottom-right of the focused area, transparent backdrop; matches the existing `.voice-button` palette.

---

## 4. Algorithm details

### 4.1 Capture ring buffer (cpal)

- Replace `Vec<f32>` push-and-grow with a fixed `RingBuffer<f32>` of 30 s @ 16 kHz (480k samples). Using a lock-free SPSC ring (`rtrb` or `ringbuf` crate). The cpal callback only writes; the streaming task only reads. **Goal:** zero allocations on the audio thread.
- Resample at the consumer side, in 32 ms hops, to keep the audio thread untouched.

### 4.2 Silero VAD

- Frame size: 512 samples @ 16 kHz = 32 ms.
- Threshold: 0.5 to enter `Speech`, 0.35 to exit (Schmitt trigger; avoids flicker).
- Hangover: stay in `Speech` for at least 100 ms after probability dips below threshold.
- `SpeechEnd` fires after `voice.endpoint_silence_ms` (default 400 ms) of below-exit-threshold frames.
- `voice_activity_detector` provides `LabelStream` which already does this; we wrap it with our hangover and edge accounting.

### 4.3 Streaming decoder loop

```text
loop {
    sleep partial_hop_ms (default 250 ms)
    if not in Speech segment, continue
    let window = ring.read_last(min(elapsed_in_segment, 6 s))
    pass = whisper.full(state, window, language)   // decode pass
    let (stable, unstable) = local_agreement(prev_pass, pass)
    if stable changed: emit("voice://partial", { stable, unstable })
    prev_pass = pass
}
on SpeechEnd:
    let segment = ring.read_segment(start_offset, end_offset)
    final_text = whisper.full(fresh_state, segment, language)
    emit("voice://final", { text: dictionary::apply(final_text) })
    history::insert(...)
    inject::type_text(final_text) if enabled and not focused
```

- Decoder runs in a single dedicated tokio task. Each `whisper.full` is called via `spawn_blocking` since it's CPU-bound.
- `state` is reused across hops to keep the encoder cache warm — significant speedup; see `whisper-rs` `state.full(...)` API. (For the **final** pass a fresh state on the full segment gives the cleanest result.)
- Tiny model, modern CPU: a 4–6 s window decodes in 30–80 ms on x86, ~60–120 ms on M1. With a 250 ms hop we're nowhere near saturation; partial will clearly show under 300 ms on hotword-light audio.

### 4.4 LocalAgreement-2 merger

```rust
pub fn merge_partials(prev_pass: &str, cur_pass: &str) -> StreamingPartial {
    let prev_tokens = tokenize_with_punct(prev_pass);
    let cur_tokens  = tokenize_with_punct(cur_pass);
    let lcp = prev_tokens.iter().zip(&cur_tokens)
        .take_while(|(a, b)| a == b)
        .count();
    let stable   = join_tokens(&cur_tokens[..lcp]);
    let unstable = join_tokens(&cur_tokens[lcp..]);
    StreamingPartial { stable, unstable, segment_id: ... }
}
```

- Tokenization preserves punctuation as standalone tokens so `, ` / `.` don't fight whitespace-only joining.
- The merger never shrinks `stable`. If a later pass disagrees with what was already committed, we keep the older commit and warn at debug log level.

### 4.5 Cloud (Deepgram, opt-in)

- Single WebSocket per session: `wss://api.deepgram.com/v1/listen?model=nova-3&interim_results=true&endpointing=400&language=...`
- Audio frames pushed as 16 kHz 16-bit PCM, 50 ms chunks.
- `is_final=false` → emit `voice://partial` with `{ stable: "", unstable: transcript }`.
- `is_final=true` → fold into stable; emit `voice://final` on `speech_final=true` or after the matching VAD edge locally.
- Authorization header: `Authorization: Token <key>`.
- Backpressure: if the WS lags > 1 s, dump the on-device fallback transcription as the partial; resume cloud as soon as it catches up.

---

## 5. Hotkey and modes

No change to the existing state machine in `mode.rs` (PTT vs Toggle stays). Two new behaviors:

1. **PTT press** is the "listening" trigger; partials start streaming after the first VAD `SpeechStart`. No change to release semantics.
2. **Toggle on** is treated identically; `Toggle off` flushes the current segment as a final.

The existing default hotkey is `Alt+Space` and is opt-in via `voice.hotkey_enabled`. We **keep that opt-in** to avoid the WM-conflict footgun on Linux compositors. The pill is always available via the existing on-screen mic button.

---

## 6. Permissions and platform notes

- `tauri.conf.json` — no new capabilities. We're not adding a new plugin; cpal handles audio, ort downloads its own runtime, fetches over reqwest go through existing TLS.
- macOS: microphone TCC entitlement is already granted via Tauri's default plist (cpal's input device requires it on first launch — system prompt). No change.
- Linux Wayland: input is unaffected. Global hotkey on GNOME/Wayland still requires `gsd-keybinding` portal; falls back to UI button — unchanged.
- Windows: nothing extra.

---

## 7. Model download flow

- **Whisper models** (existing): unchanged. Default still `tiny` for streaming; user can pick larger.
- **Silero VAD model** (new): bundled in repo as `src-tauri/assets/silero_vad.onnx` (~2 MB, MIT). The `voice_activity_detector` crate ships its own model; we vendor a copy for offline-first builds and to pin the version. Loaded via `ort::Session::from_memory(...)`.
- **Moonshine** (future): same flow as whisper — `ensure_model` in `download.rs`, listed in `voice_list_models`.
- **Deepgram**: only an API key, no download.

---

## 8. Testing plan

| Test | Where | Asserts |
|-|-|-|
| `tokens::longest_common_prefix` | `tokens.rs` | LCP for canonical / unicode / punctuation cases |
| `merge_partials` does not shrink stable | `streaming.rs` | regressions on out-of-order pass arrivals |
| VAD Schmitt-trigger edges | `vad.rs` | flapping audio doesn't yield extra `SpeechStart`s |
| WAV → expected transcript (integration) | `tests/integration_streaming.rs` | full pipeline on `tests/fixtures/hello_world_16k.wav` |
| Latency benchmark | `tests/bench_latency.rs` (release-only) | median first-token latency < 300 ms on a CI machine; logged + asserted at 600 ms upper bound to keep CI flake-free |

A small synthetic 16 kHz mono WAV is generated in `build.rs` (sine + recorded-phrase fallback) so the integration test doesn't need an audio device. The latency bench uses the same WAV and times from "first sample available" to "first `voice://partial` emitted".

---

## 9. Rollout and back-compat

- Behind setting `voice.engine` defaulting to `whisper-streaming`. Users on a system where ORT can't load (e.g. exotic libc) auto-fall back to `whisper-batch` (today's behavior) with a one-line toast.
- All existing IPC (`start_voice` / `stop_voice` / `voice_list_models` / etc.) keeps working with identical shapes.
- The `voice://transcript` event is emitted alongside the new `voice://final` for one release; UI listens to whichever fires first.

---

## 10. Out of scope for this PR

- Speaker diarization
- Streaming TTS / pig-out-loud
- Custom keyterm prompting (Deepgram has it; we'll wire UI in v2)
- Mid-utterance auto-correction with backspaces (we always commit-only)
- Sibling agent's BridgeSpace 3 port — explicitly hands-off
