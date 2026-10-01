# Xonata

A Rust viewer for Kanata v4 pipeline traces. Runs natively or in a browser through WebAssembly, without Electron. Browser traces stay local.

- **Explore several traces:** tabs and split view, independent cycle/row zoom, numbered cells for long phases, and a clickable whole-trace minimap.
- **Find every match:** literal or regex search across disassembly and metadata, matching snippets, filters, and navigation to the first phase.
- **Measure the pipeline:** draggable grid markers connected by cycle and pipeline-row distances.
- **Keep the trace central:** resizable disassembly and inspector overlays, aligned rows, metadata tooltips, and a minimal monospace interface.
- **Handle large logs:** worker-based parsing, paged temporary storage, and a bounded decoded-page cache.

## Run

Install Rust (the repository toolchain includes the WASM target).

```sh
cargo run -p xonata-app
```

Use **Open** or drag in `.log`, `.gz`, or `.zst` traces.

For the browser build, generate a local lockfile and install its matching `wasm-bindgen-cli`:

```sh
cargo generate-lockfile
xonata_bindgen_version="$(python3 -c 'import pathlib, tomllib; print(next(p["version"] for p in tomllib.loads(pathlib.Path("Cargo.lock").read_text())["package"] if p["name"] == "wasm-bindgen"))')"
cargo install wasm-bindgen-cli --version "$xonata_bindgen_version" --locked
bash scripts/build-web.sh
python3 -m http.server 8000 --directory web
```

Open <http://localhost:8000/>. For integration, see [the embedding example](web/embed-example.html): `WebHandle` exposes `start(canvas)`, `open_file(file)`, `navigate(traceId, decimalOpId)`, and `destroy()`.

## GitHub Pages

In your GitHub repository, choose **Settings → Pages → Build and deployment → Source: GitHub Actions**. Commit this project, including [.github/workflows/pages.yml](.github/workflows/pages.yml), and push to the repository’s default branch. The workflow builds and checks the viewer, then publishes it; the deployment URL appears in the workflow run. It also supports manual runs from **Actions → GitHub Pages → Run workflow**. Pull requests build and test without publishing. See [GitHub’s Pages workflow guide](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages).

For a local preview of the deployment artifact:

```sh
bash scripts/build-pages.sh
python3 -m http.server 8000 --directory target/pages
```

The artifact contains only browser assets and the font license. Generated files and local traces stay out of Git.

## Controls

| Action | Control |
| --- | --- |
| Search / close popup | **F** or **Ctrl+F** / **Esc** |
| Next / previous match | **n / p**; offers a popup before wrapping |
| Horizontal zoom in / out | **Ctrl+Right / Ctrl+Left** |
| Vertical zoom in / out | **Ctrl+Up / Ctrl+Down** |
| Zoom both axes / pan / scroll | **Ctrl+wheel** or pinch / drag / wheel |
| Place / move / remove marker | **Shift+click** / drag its dot / hover its dot and press **Delete** |
| Toggle whole-trace overview | **M** or **Overview** in the header |
| Hotkeys guide | **?** |

The overview fits all pipeline rows and cycles into a full-height strip. Click or drag to jump to a row’s first phase; the white indicator tracks the current viewport. Drag its left edge to resize, use **> / <** to expand or compact it, or **×** to hide it.

Shortcuts for letters and zoom are inactive while typing. **X/Y** percentages reset each zoom axis. Click a phase or disassembly row to inspect metadata. Drag the disassembly edge to resize it; **> / <** expands or compacts it. **Inspector → Markers** provides editing, **×** removal, and **Clear markers**. Markers last for the session. **View** contains split view, the flushed-operation filter, and jumps by operation or retired ID.

Development and verification instructions are in [AGENTS.md](AGENTS.md). Bundled Liberation Mono fonts include their [license](crates/app/assets/fonts/LICENSE).
