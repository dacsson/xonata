# TODO

## Viewer quality of life

- [x] Shift+arrows: pan by 192 pixels per step with held-key repeat; preserve editing and dialog shortcuts.
- [ ] Recent files beside Open: bounded list, deduplicate, handle missing files. Decide native path persistence and browser permission/reopen behavior before implementation; browser file names alone cannot reopen local files.
- [ ] Classic theme: inspect the local upstream Konata palette and add a small shared theme selector while retaining the current theme.
- [x] Increase vertical zoom: raise the row-height cap from 160 to 1024 pixels while preserving the zoom anchor.
- [x] Extend horizontal zoom out: lower the minimum cycle width from 0.02 to 0.000002 pixels, allowing 10,000 times more compression.
- [ ] Thin white stage borders: draw inside sufficiently large slabs, omit when too small; keep rendering bounded and check at fractional zoom.
- [ ] Alternating row backgrounds: black/dark gray bands aligned with pipeline rows, consistent under pan/zoom and flushed-row filtering. Coordinate with Classic theme.

Suggested order: keyboard pan and vertical zoom; Classic theme with borders and row backgrounds; recent files (platform-specific reopening).

- [ ] Evaluate adapting the approach in [Anomaly Detection Using Program Control Flow Graph Mining from Execution Logs](https://doi.org/10.1145/2939672.2939712) (Nandi et al., KDD 2016) to a Xonata analyzer. Explore learning normal pipeline-stage transitions, expected successor timing, and transition distributions from reference traces, then locating deviations. Use Kanata's structured operation IDs, stages, lanes, cycles, and recorded dependencies instead of rediscovering them from unstructured logs. Consider accelerator-specific duration, gap, and concurrency analysis; distinguish pipeline behavior graphs from a program's basic-block control flow graph. Assess usefulness on known regressions and normal workload variations, including false positives, incomplete captures, explainable findings, and bounded worker processing for native and browser builds.
