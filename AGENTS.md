# Vibeprint

ICC-aware print layout engine (Rust). Two binaries: `vibeprint` (CLI) and `studio` (GUI).

## Build

```bash
cargo build --release --no-default-features  # CLI-only, no X11 deps (test/verify only)
cargo build --release                       # Build both binaries (default features = monitor-icc)
```

- `monitor-icc` feature (default) pulls in `x11` and `libc` crates.
- No rustfmt/clippy/Makefile config — uses Rust defaults.
- **When building for the user** (not for compiler verification), always build with `cargo build --release` (default features). Never use `--no-default-features` for user-facing builds.

## Run CLI

```bash
cargo run --release --bin vibeprint -- process --input in.tif --output out.tif --dpi 720
cargo run --release --bin vibeprint -- printers   # List CUPS printers
cargo run --release --bin vibeprint -- meta image.tif  # Image metadata
```

### Process Options

- `--intent <relative|perceptual|saturation>` (default: relative)
- `--engine <catmullrom|lanczos3|iterative-step|mitchell-ewa|mitchell-ewa-sharp>` (default: catmullrom)
- `--dpi <N>` — **required**, controls output resolution
- `--depth <8|16>` (default: 16)
- `--sharpen <0-20>` (default: 5)
- `--input-icc <path>` — input ICC profile (default: use embedded or sRGB)
- `--output-icc <path>` — output ICC profile (default: sRGB passthrough)
- `--bpc/--no-bpc` — black point compensation (on by default only for Relative intent)

## GUI (studio)

```bash
cargo run --release --bin studio
```

Monitor ICC profile loading requires X11 — does **not** work on Wayland.

## Test

```bash
cargo test                                    # everything
cargo test --test pipeline_validation         # focused integration test
cargo test --test safe_8bit_print_path        # focused integration test
cargo test --lib                              # unit tests only
```

- Tests are in three inline modules (`src/layout_engine.rs`, `src/monitor_icc.rs`, `src/printer_discovery.rs`) plus integration tests in `tests/`.
- Integration tests: `tests/pipeline_validation.rs` (engine smoke, sharpen, page layout, composite, PDF roundtrip) and `tests/safe_8bit_print_path.rs` (ICC embedding toggle).
- Test `print_pipeline_pdf_output_is_unmodified` requires `ghostscript` installed.
- No `[dev-dependencies]` in Cargo.toml — tests use the same dependency set as the crate.

## System Dependencies

Ubuntu: `libcups2 cups-client libcups2-dev liblcms2-2 liblcms2-dev libx11-6 libx11-dev ghostscript libtiff-tools`

Fedora: `cups-libs cups-client lcms2 lcms2-devel libX11 libX11-devel ghostscript libtiff-tools`

## Architecture

```
src/
  lib.rs              — re-exports: processor, layout_engine, monitor_icc, printer_discovery
  main.rs             — CLI entry, delegates to processor::process()
  processor.rs        — ICC transform → resample → USM sharpen → TIFF output
                        Also exposes process_composite_page() for the GUI path
  layout_engine.rs    — page layout logic
  monitor_icc.rs      — X11 monitor ICC profile extraction
  printer_discovery.rs        — CUPS printer discovery
  printer_discovery/cups_ffi.rs
  bin/studio/         — eframe/egui GUI
    main.rs, mod.rs, app.rs, types.rs, icc.rs, processing.rs, utils.rs
    ui/               — canvas.rs, left_panel.rs, mod.rs, right_panel.rs, modals.rs
```

## Key Gotchas

- **Generated TIFFs are gitignored**: `*_out.tif`, `*_out.tiff`, and broad `*.tif` pattern in repo root. Do not commit processed output.
- **Crop/border/inversion logic** is complex and documented in [`CROP_AND_BORDER.md`](./CROP_AND_BORDER.md). Key invariants:
  - `force_original_orientation` orients the print size to match the **source image's natural orientation** (not the raw portrait-normalized print size).
  - When `force_original_orientation && crop_inverted` are both true, the **cell dimensions must be swapped** from that natural orientation to match the inverted crop aspect, while the image content stays un-rotated.
  - Every code path that selects between `(w, h)` and `(h, w)` must handle FOO and FOO+inverted before checking `will_rotate` — see the file's testing checklist.
- **Order of operations matters** in border change handler (`right_panel.rs`): border width must be set *before* crop UV recalculation.
- **Working-tree noise**: `.bak` files and `cargo_check.log` are present in the working tree but are not tracked by git. Do not commit them.