# Kanata trace

Source: https://github.com/shioyadan/Konata/blob/master/docs/kanata-log-format.md

Kanata describes processor pipeline activity without requiring a particular ISA
or microarchitecture. It uses tab-delimited text records. Version 4 begins with
`Kanata\t0004`, where `\t` represents a tab.

- `C=` sets absolute simulation time; `C` advances it. Following events share
  that time until another cycle command appears. Time progresses monotonically.
- `I` introduces an instruction with a file-local ID, simulator ID, and thread ID.
  These identifiers serve different purposes.
- `L` adds annotations: type 0 labels the instruction, type 1 supplies instruction
  details, and type 2 annotates its most recently started stage.
- `S` starts a named stage in a lane; `E` ends it and may be omitted.
  Stage names are arbitrary. Lanes represent overlapping states; conventionally,
  lane 0 contains ordinary stages and lane 1 contains stalls.
- `R` finishes an instruction: type 0 means retirement; type 1 means flushing.
  Retirement IDs can overlap after speculation.
- `W` records a dependency from a consumer instruction to a producer instruction.

This describes the format, not a particular processor. Use trace tools to establish
actual stage meanings, operation timings, and evidence for performance hypotheses.
