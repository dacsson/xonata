# Working on Xonata

Xonata is a Rust Kanata v4 pipeline viewer with native and browser WASM builds. Keep both supported and do not introduce Electron. Prioritize fast interaction, low memory use, and a minimal monospace interface.

## Repository map

- `crates/core`: incremental parsing, compression, paged storage, search, and worker request/event protocol.
- `crates/view`: precise viewport transforms and grid-marker geometry, independent of the renderer.
- `crates/app`: shared egui UI and native eframe transport/file opening.
- `crates/web`: browser host API, worker engine, and temporary OPFS/IndexedDB storage.
- `web`: standalone page, embedding example, and worker scheduling.
- `scripts`: WASM build and headless Chromium regression checks.
- `.github/workflows/pages.yml`: builds and verifies the static site, then deploys pushes to the default branch using GitHub Pages.

The existing viewer at `~/Tools/Konata` is an optional local format reference; builds must not depend on it. The Rust best-practices skill, when available, is at `.agents/skills/rust-best-practices/SKILL.md`.

## Implementation constraints

- Parse and search off the UI thread. Keep work batches, result pages, and decoded caches bounded; avoid retaining an entire decoded trace or fetching every search operation into the UI.
- Preserve integer cycle precision: subtract `u64` origins before converting offsets to floating-point pixels. Render only visible rows and cycle subdivisions, including for very long phases.
- Retry partially populated search-result pages as scanning progresses. Ignore stale generations; arriving result pages must not reposition an already revealed selection.
- Jump to an operation's earliest stage, falling back to its creation cycle only when stages are absent. Filter results navigate to their matched endpoint. Account for the disassembly overlay when positioning it.
- General phase filters run off the UI thread, with bounded paged indexes/results and generation-safe viewport drawing requests. Preserve signed gaps and exact decimal integers through browser messages. Keep query results independent of ordinary search and retain the drawn generation until replaced or cleared.
- Keep filter entry graphical. Offer bounded, debounced suggestions from trace data on the worker; ignore stale suggestion generations. Instruction and metadata fields use case-insensitive contains with wildcards. Interval forms default to nonnegative gaps, filtering after pairing without substituting a later target.
- Optional Exclude between reuses endpoint predicates and excludes matching Start/End boundaries strictly inside the cycle interval, either on strictly intervening rows or any row/thread. Check after pairing without choosing a later target. Build compact exclusion indexes on the worker and bound tree traversal per tick; disabled exclusion must not affect validation or results.
- Elapsed-cycle result sorting runs in bounded worker batches with disk-backed indexes, keeps original result indexes, and ignores stale revisions. Sorting changes order only; Draw includes all matches and preserves existing drawings. Form fields and result rows must fill the resizable filter window without growing on their own.
- Build the whole-trace overview in bounded worker batches and cache its compact raster. Resizing must reuse the image, and navigation must resolve unfiltered rows.
- Keep the canvas full-size beneath overlays. Disassembly and phase slabs must share row geometry under pan and independent zoom.
- Use square corners and bundled monospace fonts. Phase names should stand out from dimmer continuation numbers. Preserve font licenses in distributed builds.
- Comments are session-only per-tab text anchored to cycle/row grid points. Render bounded previews for visible anchors, keep full text editable, and block canvas shortcuts while the comment editor is open.
- Markers are session-only cycle/row grid points. Connect consecutive markers and report absolute cycle and row differences. Query-generated filter rectangles are permitted as session-only drawings; do not restore the old manual highlighting feature.
- Keep native and browser controls consistent, and update both README controls and the `?` guide when changing shortcuts. Text editing and modal dialogs must not trigger canvas zoom shortcuts.
- Avoid native-only APIs in shared WASM paths; use egui input time for UI timing. Preserve browser `WebHandle` APIs and host events when changing transport behavior.

## Verification

For Rust behavior changes, run formatting, relevant regression tests, and lints:

Generate `Cargo.lock` locally if absent (`cargo generate-lockfile`) before checks using `--locked`. Lockfiles are not committed; CI resolves dependencies and installs the matching `wasm-bindgen-cli` before building.

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy -p xonata-web --target wasm32-unknown-unknown --locked -- -D warnings
```

For shared UI or browser changes, rebuild WASM and run browser checks. Install `wasm-bindgen-cli` matching `Cargo.lock`; the smoke script requires Node.js and Chromium.

```sh
bash scripts/build-web.sh
node scripts/browser-smoke.mjs
```

For deployment changes, run `bash scripts/build-pages.sh` and verify the staged artifact under a repository subpath:

```sh
XONATA_WEB_ROOT=target/pages XONATA_BASE_PATH=/xonata/ node scripts/browser-smoke.mjs
XONATA_WEB_ROOT=target/pages XONATA_BASE_PATH=/xonata/ XONATA_ENTRY_PAGE=embed-example.html node scripts/browser-smoke.mjs
```

`XONATA_CHROMIUM` selects a Chrome/Chromium executable for local checks. Browser smoke tests are local regression checks, not prerequisites in the Pages deployment workflow. The workflow checks formatting, core/view tests, and WASM lints before building and uploading the site. Keep Pages artifacts limited to distributable browser assets, including generated WASM and the font license. Asset and worker URLs must work beneath project subpaths. Pull requests must not deploy; publishing uses the default branch and the `github-pages` environment.

If the local `drive-download-20261001T142849Z-1-001` directory is available:

```sh
node scripts/browser-smoke.mjs --real
cargo test -p xonata-core --test real_search --locked -- --ignored --nocapture
```

The real trace regression checks progressive `vfirst` results in `scr_base_lite_kanata_core0(3).log`. Browser checks also exercise multiple traces, navigation, markers, and zoom. To inspect any local trace with the shared engine:

```sh
cargo run --release -p xonata-core --example inspect -- path/to/trace.log
```

Documentation-only changes need link and command review, not a Rust rebuild. Add regression tests for behavior bugs; avoid tests that merely repeat implementation details.

## Commit hygiene

Commit source code, scripts, documentation, Cargo manifests, toolchain configuration, and bundled fonts with their license. Keep lockfiles, downloaded traces, `target/`, generated `web/pkg/`, and generated `web/font-license.txt` out of Git. Small intentional fixtures under `crates/core/tests/fixtures/` belong in Git. Treat local trace inputs as read-only; do not publish them or reference them as required build inputs.
