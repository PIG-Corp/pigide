# Tooling Audit Report

Context: repository `pigide`, branch `reset/main-20260517-181552`, commit `cd711e9`.

Scope: read-only audit of build system, config, dependencies, lint/format/test tooling, CI/CD, release flow, and developer experience. Application backend and UI implementation were intentionally ignored.

Commands used for evidence: static file reads; `pnpm list --depth 0 --json`; `pnpm outdated --format json`; `pnpm audit --json`; `cargo metadata --locked --format-version 1 --no-deps`; `cargo tree --locked`; `cargo tree -d --locked`; `du -sh` for local artifact sizes. Heavy build/test commands were not run.

## 1. Tooling Inventory

| Tool | Version | Config file | Purpose |
|---|---:|---|---|
| pnpm | lockfile v9.0; local 10.33.4; CI configured v10 | `frontend/pnpm-lock.yaml`, `.github/workflows/ci.yml`, `.github/workflows/release.yml` | Frontend package manager |
| Node.js | CI 20; local v26.1.0 | `.github/workflows/ci.yml`, `.github/workflows/release.yml` | Runtime for frontend scripts and tooling |
| Vite | 8.0.13 installed; latest 8.0.14 | `frontend/vite.config.ts` | Frontend dev server and bundle |
| TypeScript | 6.0.3 installed | `frontend/tsconfig.json`, `frontend/tsconfig.app.json`, `frontend/tsconfig.node.json` | Typecheck via `tsc -b` before Vite build |
| React | 19.2.6 | `frontend/package.json` | Frontend UI runtime |
| @vitejs/plugin-react | 6.0.1 installed; latest 6.0.2 | `frontend/vite.config.ts` | React transform and HMR |
| ESLint flat config | 10.3.0 installed; latest 10.4.0 | `frontend/eslint.config.js` | TS/React hooks/react-refresh lint |
| typescript-eslint | 8.59.3 installed; latest 8.59.4 | `frontend/eslint.config.js` | TypeScript ESLint preset |
| Cargo/Rust | local cargo 1.95.0; CI stable | `Cargo.toml`, `src-tauri/Cargo.toml` | Rust workspace build, checks, tests |
| Tauri | Rust crate 2.11.1; JS CLI 2.11.1, latest 2.11.2 | `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` | Desktop runtime, dev orchestration, packaging |
| rustfmt | CI stable component | `.github/workflows/ci.yml` | Rust format check |
| clippy | CI stable component | `.github/workflows/ci.yml` | Rust static checks with `-D warnings` |
| cargo-audit | installed during CI run | `.github/workflows/ci.yml` | Rust advisory scan, currently non-blocking |
| Swatinem/rust-cache | v2 | `.github/workflows/ci.yml`, `.github/workflows/release.yml` | Rust dependency/build cache |
| GitHub Actions CI | n/a | `.github/workflows/ci.yml` | PR/main checks |
| GitHub Actions Release | n/a | `.github/workflows/release.yml` | Tag/manual Tauri release matrix |
| Custom release script | bash | `scripts/build.sh` | Local one-shot release build and artifact collection |
| Cargo env config | n/a | `.cargo/config.toml` | Global Cargo/Tauri build environment, `NO_STRIP=1` |
| Custom helper test runner | Node built-in `node --test` + `tsc` | `frontend/scripts/test-helpers.mjs` | Isolated helper tests without adding a JS test framework |
| Ad-hoc Playwright script | no package/config found | `redesign.spec.js` | Screenshot capture, not wired into package tooling |

Not found: Prettier, Biome, Jest, Vitest, Playwright config, Husky, Lefthook, lint-staged, `.nvmrc`, `.node-version`, `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, release/changelog config. `CHANGELOG.md` is referenced by release workflow but not present.

## 2. Dependency Map

### Status Summary

- `pnpm audit --json`: 0 info, 0 low, 0 moderate, 0 high, 0 critical advisories; total npm dependencies reported: 278.
- `pnpm outdated --format json`: only minor/patch drift in dev dependencies.
- Rust advisory status: not verified locally because `cargo-audit` is not installed; CI installs it per run and marks the step non-blocking.
- `Cargo.lock`: 714 crate entries.
- `cargo tree -d --locked`: notable duplicate major/version families include `reqwest` 0.12/0.13, `dirs` 5/6, `thiserror` 1/2, `rand` 0.8/0.9/0.10, `bitflags` 1/2, `toml` 0.8/0.9/1.1, `toml_edit` 0.19/0.20/0.25, `getrandom` 0.2/0.3/0.4, `syn` 1/2.

### Top Prod Dependencies

| Package | Spec | Current | Status | Local size |
|---|---:|---:|---|---:|
| lucide-react | ^1.16.0 | 1.16.0 | ok | 34.3M |
| react-dom | ^19.2.6 | 19.2.6 | ok | 7.1M |
| @xterm/xterm | ^6.0.0 | 6.0.0 | ok | 6.1M |
| react-force-graph-2d | ^1.29.1 | 1.29.1 | ok | 1.7M |
| @codemirror/view | ^6.43.0 | 6.43.0 | ok | 1.1M |
| @xterm/addon-search | ^0.16.0 | 0.16.0 | ok | 0.8M |
| @xterm/addon-image | ^0.9.0 | 0.9.0 | ok | 0.7M |
| @codemirror/state | ^6.6.0 | 6.6.0 | ok | 0.4M |
| allotment | ^1.20.5 | 1.20.5 | ok | 0.3M |
| @codemirror/language | ^6.12.3 | 6.12.3 | ok | 0.3M |
| react | ^19.2.6 | 19.2.6 | ok | 0.3M |
| @codemirror/autocomplete | ^6.20.2 | 6.20.2 | ok | 0.2M |
| @codemirror/commands | ^6.10.3 | 6.10.3 | ok | 0.2M |
| zustand | ^5.0.13 | 5.0.13 | ok | 0.2M |
| @codemirror/search | ^6.7.0 | 6.7.0 | ok | 0.1M |
| @codemirror/lang-html | ^6.4.11 | 6.4.11 | ok | 0.1M |
| @codemirror/lang-markdown | ^6.5.0 | 6.5.0 | ok | 0.1M |
| @codemirror/lang-javascript | ^6.2.5 | 6.2.5 | ok | 0.1M |
| @xterm/addon-web-links | ^0.12.0 | 0.12.0 | ok | 0.1M |
| @codemirror/lang-css | ^6.3.1 | 6.3.1 | ok | 0.1M |
| @codemirror/lang-python | ^6.2.1 | 6.2.1 | ok | 0.1M |
| @xterm/addon-fit | ^0.11.0 | 0.11.0 | ok | 0.1M |
| codemirror | ^6.0.2 | 6.0.2 | ok | 0.1M |
| @codemirror/lang-json | ^6.0.2 | 6.0.2 | ok | <0.1M |
| @codemirror/lang-rust | ^6.0.2 | 6.0.2 | ok | <0.1M |

### Top Dev Dependencies

| Package | Spec | Current | Status | Local size |
|---|---:|---:|---|---:|
| typescript | ~6.0.2 | 6.0.3 | ok | 23.6M |
| eslint-plugin-react-hooks | ^7.1.1 | 7.1.1 | ok | 3.9M |
| eslint | ^10.3.0 | 10.3.0 | outdated -> 10.4.0 | 3.8M |
| @types/node | ^24.12.3 | 24.12.4 | latest major 25.9.1 | 2.6M |
| vite | ^8.0.12 | 8.0.13 | outdated -> 8.0.14 | 2.2M |
| @tauri-apps/api | ^2.11.0 | 2.11.0 | ok | 0.8M |
| @types/react | ^19.2.14 | 19.2.14 | outdated -> 19.2.15 | 0.4M |
| @tauri-apps/cli | ^2.11.1 | 2.11.1 | outdated -> 2.11.2 | 0.4M |
| globals | ^17.6.0 | 17.6.0 | ok | 0.3M |
| @types/react-dom | ^19.2.3 | 19.2.3 | ok | 0.1M |
| @vitejs/plugin-react | ^6.0.1 | 6.0.1 | outdated -> 6.0.2 | 0.1M |
| typescript-eslint | ^8.59.2 | 8.59.3 | outdated -> 8.59.4 | 0.1M |
| @eslint/js | ^10.0.1 | 10.0.1 | ok | 0.1M |
| eslint-plugin-react-refresh | ^0.5.2 | 0.5.2 | ok | 0.1M |

### Rust Direct Dependencies

Important direct prod deps locked by `cargo tree --locked -e normal --depth 1`: `tauri` 2.11.1, `tauri-plugin-updater` 2.10.1, `reqwest` 0.12.28, `tokio` 1.52.3 with `full`, `rusqlite` 0.31.0 with `bundled`, `r2d2_sqlite` 0.24.0, `axum` 0.7.9, `tower-http` 0.6.10, `portable-pty` 0.8.1, `whisper-rs` 0.16.0, `cpal` 0.15.3, `notify` 6.1.1, `notify-debouncer-full` 0.3.2, `enigo` 0.3.0, `arboard` 3.6.1, `serde` 1.0.228, `serde_json` 1.0.149, `uuid` 1.23.1, `chrono` 0.4.44, `regex` 1.12.3, `rand` 0.8.6, `dirs` 5.0.1, `thiserror` 1.0.69, `anyhow` 1.0.102, `tracing-subscriber` 0.3.23, `tower` 0.5.3.

Direct Rust dev/build deps: `tauri-build` 2.6.1, `wiremock` 0.6.5, `tempfile` 3.27.0, `tokio-test` 0.4.5.

## 3. TOP-15 Issues

| Severity | File | Category | Issue | Fix |
|---|---|---|---|---|
| P1 | `.github/workflows/ci.yml` | CI quality gate | Frontend lint is non-blocking via `continue-on-error: true`, so PR/main can pass with lint regressions. | Make lint blocking; if there is existing noise, create a baseline or temporarily narrow the checked scope. |
| P1 | `.github/workflows/ci.yml` | Security gate | `cargo audit` is non-blocking and installed every run, so advisory findings do not protect merges. | Pin/cache `cargo-audit` or use a maintained action; remove `continue-on-error` after initial advisory triage. |
| P1 | `.github/workflows/ci.yml`, `.github/workflows/release.yml` | CI cache | pnpm store is not cached; every CI/release run performs a cold frontend install. | Add `actions/setup-node` pnpm cache or explicit `actions/cache` using `pnpm store path`. |
| P1 | `frontend/package.json` | Toolchain pinning | No `packageManager` and no `engines`; CI uses Node 20, README allows Node 20+/pnpm 9+, local audit used Node 26/pnpm 10.33.4. | Add `packageManager: pnpm@10.x`, `engines`, and optionally `.node-version` or a Corepack policy. |
| P1 | `src-tauri/tauri.conf.json`, `scripts/build.sh` | Build path risk | Tauri config uses `pnpm --dir frontend ...`; local build script runs Tauri after `cd src-tauri`. If Tauri executes commands relative to project dir, `frontend` is the wrong path. | Verify Tauri command cwd; use `pnpm --dir ../frontend ...` or invoke Tauri consistently from repo root. |
| P1 | `.github/workflows/release.yml` | Release flow | Release body says `See CHANGELOG.md for details.`, but no `CHANGELOG*` file was found. | Add changelog generation/file or change `releaseBody`. |
| P2 | `frontend/tsconfig.app.json`, `frontend/tsconfig.node.json` | TypeScript strictness | `strict` is absent, so strict mode is off despite other lint-like compiler flags. | Enable `strict` incrementally; consider later `noUncheckedIndexedAccess` and `exactOptionalPropertyTypes`. |
| P2 | `frontend/tsconfig.app.json`, `frontend/tsconfig.node.json` | TypeScript safety | `skipLibCheck: true` hides dependency type breakage, especially risky on bleeding-edge TS/React. | Add a periodic CI job without `skipLibCheck` or remove it after dependency stabilization. |
| P2 | `frontend/eslint.config.js` | Lint depth | ESLint uses `tseslint.configs.recommended`, not type-aware rules; the template README recommends type-checked presets for production apps. | Add `projectService`/project config and `recommendedTypeChecked` or `strictTypeChecked` with a baseline. |
| P2 | repo root | Format/pre-commit | No JS formatter config and no pre-commit hook tooling; Rust formatting only runs in CI. | Choose one formatter path and add `rustfmt` plus frontend format/lint to Lefthook or Husky. |
| P2 | `frontend/package.json`, `.github/workflows/ci.yml` | Tests | `test:helpers` exists but is not run in CI; no Vitest/Jest/Playwright config or coverage tooling. | Add `pnpm run test:helpers` to frontend CI; define a frontend test strategy before adding more frameworks. |
| P2 | `redesign.spec.js` | Test/tooling drift | Root Playwright script requires `@playwright/test`, which is not declared; it uses CJS style in an ESM frontend repo and absolute screenshot paths. | Remove it or formalize it with a devDependency, `playwright.config.*`, and relative artifacts. |
| P2 | `scripts/build.sh` | Cross-platform | The release helper is bash-only and depends on Unix tools and ANSI output, so it is not Windows-friendly. | Document it as Linux/macOS only or add a Node/Rust cross-platform wrapper. |
| P2 | `src-tauri/Cargo.toml` | Rust dependency bloat | Direct deps use broad or old choices: `tokio` full, `dirs` 5 while Tauri pulls 6, `thiserror` 1 while ecosystem pulls 2, `rand` 0.8 while transitives pull 0.9/0.10, `reqwest` 0.12 plus updater pulls 0.13. | Narrow `tokio` features; update direct major versions where low risk; review `reqwest`/TLS alignment. |
| P2 | `Cargo.toml`, `.cargo/config.toml`, `scripts/build.sh` | Config drift | Comments conflict around `strip`/`NO_STRIP`: root Cargo says `strip=false`, `.cargo/config.toml` says binaries are already trimmed, build script comment mentions `strip=true`. | Align comments and choose one documented source of truth; do not change behavior without release verification. |

## 4. Consolidation Recommendations

- Keep a single JS lint/format stack. The lowest-churn option is ESLint for lint plus Prettier for formatting; if Biome is preferred, replace rather than run competing formatters.
- Formalize frontend testing. Either keep the current zero-dependency `node --test` approach and wire it into CI, or adopt Vitest deliberately. Avoid ad-hoc Playwright scripts outside package config.
- Consolidate Rust dependency majors where direct deps allow it: `dirs` 5 to 6, `thiserror` 1 to 2, `rand` 0.8 to the current compatible major, and review the `reqwest` 0.12/0.13 split introduced by updater dependencies.
- Pin toolchains for reproducible local and CI behavior: `packageManager`, Node version, and `rust-toolchain.toml`.
- Narrow heavy features after usage review: `tokio = { features = ["full"] }`, reqwest default TLS, Tauri/updater TLS stacks, and bundled SQLite.
- Keep `cargo audit` as a real security gate rather than a best-effort informational step once current advisories are triaged.

## 5. CI Bottlenecks

- No exact step durations are available in YAML, but static CI structure shows likely bottlenecks.
- Linux jobs install WebKit/GTK/system packages with apt on every run and no apt cache.
- `cargo install cargo-audit --locked` runs every CI execution.
- Frontend `pnpm install --frozen-lockfile` runs without pnpm store cache in CI and release workflows.
- Release builds a Tauri matrix for Linux, macOS, and Windows; macOS universal target is likely the slowest compile/package leg.
- Local artifact sizes indicate cache pressure: `target/` is about 34G, `frontend/node_modules` about 219M, `frontend/node_modules/.pnpm` about 207M, `frontend/dist` about 1.8M.
- Rust cache is configured with `Swatinem/rust-cache@v2`; frontend cache is not.

## 6. Quick Wins

1. Remove `continue-on-error` from frontend lint or add a required blocking lint job.
2. Add pnpm cache to CI and release workflows.
3. Add `packageManager`, `engines`, and a Node version file or Corepack policy.
4. Add `pnpm run test:helpers` to frontend CI.
5. Add `CHANGELOG.md` or change the release body.
6. Cache/pin `cargo-audit` and make the audit gate blocking after triage.
7. Add `rust-toolchain.toml`.
8. Verify and, if needed, fix Tauri `beforeBuildCommand`/`beforeDevCommand` cwd assumptions.
9. Add one formatter config and a small pre-commit hook.
10. Remove or formalize `redesign.spec.js`.
11. Enable TypeScript `strict` incrementally.
12. Add type-aware ESLint gradually with a baseline.
13. Review Rust direct dependency majors that cause duplicate graphs.
14. Narrow `tokio` features from `full` if all subfeatures are not required.
15. Align `strip`/`NO_STRIP` comments with the behavior that was last release-verified.

## 7. Open Questions

- Which Node, pnpm, and Rust versions are officially supported for contributors and CI?
- Should the release workflow rerun all quality gates, or is it allowed to package already-validated tags only?
- Is Windows support required for the local release command, or is `scripts/build.sh` officially Linux/macOS only?
- Can `cargo audit` become blocking immediately, or are there known advisories in `Cargo.lock` that need triage first?
- Should TypeScript `strict` and type-aware ESLint become required gates now, or start as warnings/baseline?
- Which `strip`/`NO_STRIP` strategy was last verified for AppImage packaging?
- Is the ad-hoc Playwright screenshot workflow still needed, or should it be removed from the repository?
- Should frontend tests stay dependency-free with `node --test`, or should the project standardize on Vitest?
