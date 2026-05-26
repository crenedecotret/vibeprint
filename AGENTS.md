# Vibeprint

ICC-aware print layout engine (Rust). Two binaries: `vibeprint` (CLI) and `studio` (GUI).

## Build

```bash
cargo build --release --no-default-features  # CLI-only, no X11 deps
cargo build --release                       # Build both binaries (default features = monitor-icc) ALWAYS BUILD THIS LAST
```

- `monitor-icc` feature (default) pulls in `x11` and `libc` crates.
- No rustfmt/clippy/Makefile config — uses Rust defaults.

## Run CLI

```bash
cargo run --release --bin vibeprint -- process --input in.tif --output out.tif --dpi 720
cargo run --release --bin vibeprint -- printers   # List CUPS printers
cargo run --release --bin vibeprint -- meta image.tif  # Image metadata
```

### Process Options

- `--intent <relative|perceptual|saturation>` (default: relative)
- `--engine <catmullrom|lanczos3|iterative-step|mitchell-ewa|mitchell-ewa-sharp>` (default: catmullrom)
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
cargo test
```

- Tests are in three inline modules (`src/layout_engine.rs`, `src/monitor_icc.rs`, `src/printer_discovery.rs`) plus integration tests in `tests/`.
- Integration tests: `tests/pipeline_validation.rs` (engine smoke, sharpen, page layout, composite, PDF roundtrip) and `tests/safe_8bit_print_path.rs` (ICC embedding toggle).
- Test `print_pipeline_pdf_output_is_unmodified` requires `ghostscript` installed.
- No `[dev-dependencies]` in Cargo.toml.

## System Dependencies

Ubuntu: `libcups2 cups-client libcups2-dev liblcms2-2 liblcms2-dev libx11-6 libx11-dev libxrandr2 libxrandr-dev ghostscript libtiff-tools`

Fedora: `cups-libs cups-client lcms2 lcms2-devel libX11 libX11-devel libXrandr libXrandr-devel ghostscript libtiff-tools`

## Architecture

```
src/
  lib.rs              — re-exports: processor, layout_engine, monitor_icc, printer_discovery
  main.rs             — CLI entry, delegates to processor::process()
  processor.rs        — ICC transform → resample → USM sharpen → TIFF output
  layout_engine.rs    — page layout logic
  monitor_icc.rs      — X11 monitor ICC profile extraction
  printer_discovery/  — CUPS printer discovery (cups_ffi.rs)
  printer_discovery.rs
  bin/studio/         — eframe/egui GUI
    main.rs, mod.rs, app.rs, types.rs, icc.rs, processing.rs, utils.rs
    ui/               — canvas.rs, left_panel.rs, right_panel.rs, modals.rs
```

## Key Gotchas

- **Generated TIFFs are gitignored**: `*_out.tif`, `*_out.tiff`, and broad `*.tif` pattern in repo root. Do not commit processed output.
- **Crop/border/inversion logic** is complex and documented in `CROP_AND_BORDER.md`. Key invariant: always use `effective_will_rotate` (accounts for inversion) when computing visible area dimensions. `force_original_orientation` further overrides the rotation decision — see the file's testing checklist.
- **Order of operations matters** in border change handler (`right_panel.rs`): border width must be set *before* crop UV recalculation.