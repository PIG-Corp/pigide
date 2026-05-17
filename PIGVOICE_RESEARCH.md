# PigVoice — Research (Phase A)

**Goal:** instant voice-to-text inside PigIDE with sub-300ms perceived latency from speech to first visible token.

The current pipeline is **batch**: capture → stop → resample → whisper-rs `full()` → emit final transcript. There is no partial hypothesis, no streaming, and no VAD-based endpointing. Below is the landscape of options we can plug in to get true streaming.

---

## 1. On-device streaming engines

### 1.1 whisper.cpp (current backend, via `whisper-rs`)

Already a dependency. Has an `examples/stream` reference implementation that does sliding-window decoding (default `--step 500 --length 5000`, i.e. re-decode the trailing 5 s window every 500 ms). With `--step 0` it switches to sliding-window-with-VAD: only decode after silence, transcribe the trailing window. The README explicitly calls its VAD "very basic".

- **Pros:** no new native dep, GPU optional, works everywhere (CUDA / Metal / Vulkan / hipBLAS / OpenBLAS via `whisper-rs` feature flags), models we already download to `~/.cache/pigide/`.
- **Cons:** stream example is naive; quality of partials degrades because the model wants 30 s context. Re-decoding the same window every 500 ms is wasteful.
- **Trick to make it usable:** combine with **LocalAgreement-2** prefix-confirmation (see Whisper-Streaming below). Each partial only commits the prefix that two successive decodings agree on.
- **Real latency:** first **partial** ~300–500 ms after speech start (one decode pass on tiny/base on CPU). First **stable** word ~700–900 ms (two-pass agreement). Final segment behaves like batch.
- **Languages:** all Whisper-supported (99 langs), good RU/EN.
- **License:** MIT.

Source: `https://github.com/ggerganov/whisper.cpp/blob/master/examples/stream`

### 1.2 Whisper-Streaming (UFAL, ufal/whisper_streaming)

Reference implementation of **LocalAgreement-2** on top of `faster-whisper` / `whisper_timestamped`. The algorithm is the load-bearing piece, not the Python code:

> "If n consecutive updates, each with a newly available audio stream chunk, agree on a prefix transcript, it is confirmed."

Paper-reported latency: 3.3 s on **unsegmented long-form** speech (the hard case). On short dictation chunks, with `--min-chunk-size 0.3`, first stable token lands well under 1 s. Authors are migrating to **SimulStreaming** (faster + better, 2025), but the LocalAgreement-2 trick stays the same.

- **What we steal:** the algorithm. Implement in Rust on top of `whisper-rs` with a 200–300 ms hop and a 5–10 s rolling window. No new runtime dep.
- **License:** MIT.

Source: `https://github.com/ufal/whisper_streaming`

### 1.3 faster-whisper (CTranslate2)

Up to **4× faster** than openai-whisper at the same accuracy on GPU; quantized 8-bit on CPU brings further gains. Returns `segments` as a generator → can stream partials as decoding progresses. Built-in Silero VAD filter. Compatible with Distil-Whisper checkpoints.

- **Cons for us:** Python runtime. Tauri shell-out is messy on Linux/macOS/Windows. Not justified given whisper.cpp already covers our quality bar.
- **Verdict:** good fallback for users who already have a Python env; not the primary engine.
- **License:** MIT.

Source: `https://github.com/SYSTRAN/faster-whisper`

### 1.4 Distil-Whisper

Pure model swap, drop-in for whisper.cpp/faster-whisper. **6× faster** at within-1% WER on out-of-distribution audio. **English only.**
- distil-large-v3: 756M, 6.3× speedup
- distil-medium.en: 394M, 6.8× speedup (the fastest)
- distil-small.en: 166M, 5.6× speedup (memory-constrained)

- **Verdict:** ship as an **optional model** in the existing model picker for English-only users. Already partly wired (`ModelId::DistilLarge`). License: MIT.

Source: `https://github.com/huggingface/distil-whisper`

### 1.5 Moonshine (Useful Sensors) — **the latency leader**

Purpose-built for live speech. Designed around sub-200 ms response on CPU.

| Model | Params | WER (en) |
|-|-|-|
| Tiny | 26M | 12.66 |
| Tiny Streaming | 34M | 12.00 |
| Base | 58M | 10.07 |
| Small Streaming | 123M | 7.84 |
| Medium Streaming | 245M | **6.65** |

Latency for Medium-Streaming vs Whisper Large-v3:
- MacBook Pro: **107 ms** vs 11,286 ms
- Linux x86: **269 ms** vs 16,919 ms
- Raspberry Pi 5: **802 ms**

**Languages:** English, Spanish, Mandarin, Japanese, Korean, Vietnamese, Ukrainian, Arabic. Non-English variants are mostly Base-sized. **No Russian, no German, no French → not a multi-lang default for an IDE used internationally.**

**Architecture wins for streaming:** flexible input windows (no 30 s zero-padding), encoder/decoder caching for streaming. Ships as ONNX `.ort` flatbuffers (8-bit). No Rust crate yet, but ONNX Runtime via `ort` is straightforward.

- **Verdict:** **second engine, opt-in for English speakers** who want lowest possible latency. Bring up via `ort` crate; reuse the model-download flow.
- **License:** MIT.

Source: `https://github.com/usefulsensors/moonshine`

### 1.6 NVIDIA Parakeet TDT 0.6B v2

Transducer-based, **6.05% WER** average on the OpenASR leaderboard. RTFx ~3,386 (batched). Robust under noise (6.95 at SNR 10) and on telephony 8 kHz μ-law (6.32). **Trained with full attention → designed for offline batch transcription up to 24 minutes.** Streaming is only via the hosted Riva API; the published model is **offline-oriented**.

- **Languages:** English only on the v2; v3 covers 25 European languages.
- **Hardware:** Linux + NVIDIA GPU (Volta+), ~2 GB RAM minimum, NeMo runtime (PyTorch). **Non-starter for a portable Tauri desktop app.**
- **License:** CC-BY-4.0.

Source: `https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2`

### 1.7 SenseVoice (Alibaba/FunAudioLLM)

Non-autoregressive end-to-end model. Small variant processes 10 s of audio in ~70 ms, claimed **5× faster than Whisper-Small** and **15× faster than Whisper-Large**. **50+ languages**, strong on Chinese / Cantonese. ONNX + LibTorch export available.

- **Streaming?** Not natively (non-AR, capped at 30 s direct input). A community fork `streaming-sensevoice` adds chunked pseudo-streaming via truncated attention + CTC prefix beam search, **trading some accuracy** for streaming.
- **Verdict:** interesting for Chinese-heavy users; not worth a third backend in v1.

Source: `https://github.com/FunAudioLLM/SenseVoice`

---

## 2. Cloud streaming engines (opt-in, fallback)

| Provider | Median latency claim | Pricing | Languages | Notes |
|-|-|-|-|-|
| **Deepgram Nova-3** | sub-300 ms first-word, ~6.84% streaming WER | **$0.0077 / min** | EN, ES, FR, DE, HI, RU, PT, JA, IT, NL streaming + multilingual code-switching | Single best price/perf; key terms prompting (up to 100 terms) without retraining; built-in PII redaction |
| **AssemblyAI Universal-Streaming** | ~300 ms first-word | ~$0.15 / hr | EN-heavy + multi | Solid quality on EN; less aggressive on non-English |
| **Speechmatics Real-time** | 800 ms+ | $$$$ | 50 langs incl. RU | Best non-English accuracy; not the fastest |
| **Soniox** | ~300 ms first-word | $$ | 60 langs | Decent multilang; smaller ecosystem |
| **Gladia** | 300–500 ms | $$ | many | Whisper-on-cloud essentially |
| **Rev AI Realtime** | ~1 s | $$$ | EN | Old guard, slower |

**Verdict for v1:** ship **Deepgram Nova-3** as the cloud option. Best perf/price/lang mix; well-documented WebSocket streaming protocol. Keep AssemblyAI as future-extension code path. API keys go to local settings KV, never committed.

Sources: Deepgram blog `https://deepgram.com/learn/introducing-nova-3-speech-to-text-api`; AssemblyAI Universal-Streaming announcement (linked from blog).

---

## 3. VAD / endpointing

### 3.1 Silero VAD (the consensus pick)

- **Model size:** ~2 MB (JIT). ONNX export available.
- **Speed:** **<1 ms per 30 ms chunk on a single CPU thread**. Effectively free.
- **Sample rates:** 8 kHz (256-sample frames) or 16 kHz (512-sample frames).
- **Languages:** trained on 6,000+ langs; generalizes well.
- **Rust crate:** `voice_activity_detector` (Silero V5, MIT, ONNX via `ort`). Mature, supports `predict`, `PredictIterator`, `PredictStream`, `LabelStream` (auto-emits Speech/NonSpeech with padding chunks). Cross-platform.
- **License:** MIT.

Source: `https://github.com/snakers4/silero-vad`, Rust binding `https://github.com/nkeenan38/voice_activity_detector`

### 3.2 WebRTC VAD (fallback)

Tiny C library, integer-only, very fast. Three aggressiveness levels. **Quality drops noticeably in noisy environments** (cafes, fans, music). The `webrtc-vad` Rust crate exists but is unmaintained.

**Verdict:** Silero VAD is the obvious win. Use webrtc-vad only as a no-deps fallback if ORT can't load.

---

## 4. UX of partial-hypothesis rendering

Survey of the dictation tools that feel instant:

| App | Approach |
|-|-|
| **Wispr Flow** | Tokens stream into the focused field as you speak; final pass corrects on pause. Subtle italic/dim style for unconfirmed tokens, switches to normal weight on commit. |
| **SuperWhisper** | Press-to-talk only; transcribes after release (batch). Feels slower than Wispr but is more accurate. |
| **Otter.ai** | Live captions stream with a "rolling" tail — last 1–2 words wobble as the model reconsiders. Confirmed prefix never moves. |
| **Granola** | Same rolling-tail pattern as Otter for meeting notes. |
| **macOS Dictation** | Stable prefix + faded tail, commits on punctuation/silence. |
| **Google Live Caption** | Confirmed (gray) prefix + bold rolling tail (last ~1 s). |

**Distilled UX rules:**
1. Once a prefix is committed, **never edit it visually**. Re-flowing committed text is the #1 reason users say a tool feels "buggy".
2. **Render unstable suffix dimmed/italic** (50–70% opacity) so the user knows it's tentative.
3. **Commit aggressively** with LocalAgreement-2 (two-pass prefix match): the user sees stable text within 700 ms.
4. Endpointing is the trigger for the **final** pass. On a 350 ms silence with the partial in hand, run a final decode on the full segment and replace the dim tail with a clean final.
5. Insertion model: append at caret, not "type-and-correct" with backspaces. Backspace storms wreck the user's text in any focused editor.

---

## 5. On-device vs cloud trade-offs

| Axis | On-device (whisper.cpp / Moonshine) | Cloud (Deepgram) |
|-|-|-|
| First-token latency | 300–700 ms (depends on model + CPU) | 200–400 ms incl. network |
| Privacy | Audio never leaves machine | Audio streamed to provider |
| Cost | Free (compute) | $0.0077/min ≈ $0.46/hr |
| Offline | Yes | No |
| Setup | Model download (75 MB – 3.1 GB) | API key |
| Quality (EN) | Good (small) → Excellent (large) | Excellent |
| Quality (RU/multi) | Good → Excellent (Whisper) | Good (Nova-3 limited list) |
| GPU benefit | Big speedup if available | N/A |

For an IDE that opens uninvited audio into a developer's day, **privacy default = on-device**. Cloud is opt-in for users who want to trade audio for a few hundred ms.

---

## 6. Recommendation

### Primary engine: **whisper.cpp + LocalAgreement-2 + Silero VAD**

- Reuses the `whisper-rs` we already ship.
- Add `voice_activity_detector` (~Silero V5 ONNX) as a new crate.
- Implement a streaming decoder (Rust) that:
  - keeps a rolling 6 s ring buffer of 16 kHz mono `f32`,
  - runs Silero VAD at 30 ms cadence to drive `started` / `ended` segment edges,
  - hop-decodes every 250 ms (`whisper-rs::full()` on the trailing window) using a fresh `state`,
  - applies LocalAgreement-2 over the last two hypotheses to emit a **stable prefix** (`voice://partial` event) and an **unstable tail** (the dim italic span),
  - on a 400 ms VAD silence, runs a **final decode** on the full segment and emits `voice://transcript` with the clean text — same shape as today, fully back-compat.

**Default model:** `tiny` for laptops, switch to `base` if user explicitly picks "accuracy". Size on disk: 75 MB / 142 MB. CPU first-partial budget on tiny: ~150–300 ms on a modern x86; ~250–500 ms on M1; ~600 ms on RPi.

### Fallback chain

1. **On-device Whisper streaming** (default).
2. **On-device Moonshine Medium-Streaming** for English-only users who want lowest latency. Opt-in. ONNX via `ort`. Single new dep, single model file.
3. **Cloud Deepgram Nova-3** for users with an API key. WebSocket protocol; `reqwest` + `tokio-tungstenite` (already in our async stack via `axum`). 100% opt-in, key in user settings, never committed.
4. **Batch whisper.cpp** (today's path) — for legacy users or when streaming fails (e.g. no audio device).

### Settings model

- `voice.engine` ∈ `whisper-streaming` (default), `moonshine`, `deepgram`, `whisper-batch`
- `voice.partial_enabled` (default `true`)
- `voice.partial_hop_ms` (default `250`)
- `voice.endpoint_silence_ms` (default `400`)
- `voice.cloud_api_key` (Deepgram only; user settings KV; gitignored)

### What we are NOT doing in v1

- Speaker diarization (Pyannote, etc.) — out of scope.
- Streaming TTS — separate feature.
- Custom vocab / keyterm prompting — Deepgram has it natively, whisper.cpp via initial prompt; ship in v2.
- Streaming SenseVoice / Parakeet — engine swap can wait.

---

## 7. Sources

- whisper.cpp streaming README — https://github.com/ggerganov/whisper.cpp/blob/master/examples/stream/README.md
- Whisper-Streaming + LocalAgreement-2 — https://github.com/ufal/whisper_streaming
- faster-whisper — https://github.com/SYSTRAN/faster-whisper
- Distil-Whisper — https://github.com/huggingface/distil-whisper
- Moonshine — https://github.com/usefulsensors/moonshine
- NVIDIA Parakeet TDT 0.6B v2 — https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2
- SenseVoice — https://github.com/FunAudioLLM/SenseVoice
- Silero VAD — https://github.com/snakers4/silero-vad
- voice_activity_detector (Rust) — https://github.com/nkeenan38/voice_activity_detector
- Deepgram Nova-3 — https://deepgram.com/learn/introducing-nova-3-speech-to-text-api
- whisper-rs — https://github.com/tazz4843/whisper-rs
