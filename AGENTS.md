# AGENTS.md

Vibeprint is an ICC-aware print layout engine (Rust). Two binaries: `vibeprint` (CLI) and `studio` (GUI).

## Build Commands
- `cargo build --release` - Build both binaries
- `cargo build --release --no-default-features` - Build without X11 deps (headless/Wayland)
- `cargo test` - Run tests (note: `print_pipeline_pdf_output_is_unmodified` requires ghostscript)

## System Dependencies
`libcups2`, `lcms2`, `libx11`, `libxrandr`, `ghostscript`, `libtiff-tools`

## Architecture
- `src/lib.rs` - Library root; `processor` and `layout_engine` modules
- `src/processor.rs` - Image pipeline: ICC transform, resample (Mks/Lanczos3/Mitchell-EWA), USM sharpen, TIFF output
- `src/layout_engine.rs` - Page layout, borders, rotations
- `src/bin/studio/` - GUI app (eframe/egui); requires X11 for monitor ICC
- `src/monitor_icc.rs` - X11-only; skipped on `--no-default-features`
- `src/printer_discovery/` - CUPS printer queries via FFI

## Notes
- Tests in `tests/pipeline_validation.rs` - one requires ghostscript
- Generated `*_out.tif`, `*.tif` files are gitignored
- No lint/typecheck/format commands (standard cargo only)
- Do not build with `--no-default-features` for production use; monitor ICC support is essential