# AGENTS.md

Vibeprint is an ICC-aware print layout engine (Rust). Two binaries: `vibeprint` (CLI) and `studio` (GUI).

## Essential Commands
- `cargo build --release` - Build both binaries (vibeprint CLI + studio GUI)
- `cargo build --release --no-default-features` - Build CLI-only (no X11/Wayland deps)
- `cargo test` - Run all tests (one test requires ghostscript: `print_pipeline_pdf_output_is_unmodified`)
- `cargo run --release --bin vibeprint -- <subcommand>` - Run CLI directly
- `cargo run --release --bin studio` - Run GUI (requires X11 for monitor ICC)

## CLI Subcommands (vibeprint)
- `meta <image>` - Print image metadata
- `printers [--name <printer>]` - List CUPS printers or get capabilities
- `process` - Process image with ICC transform:
  - Required: `--input <file> --output <file> --dpi <value>`
  - Common options:
    - `--input-icc <file>` - Input ICC profile
    - `--output-icc <file>` - Output ICC profile  
    - `--intent <relative|perceptual|saturation>` (default: relative)
    - `--engine <catmullrom|lanczos3|iterative-step|mitchell-ewa|mitchell-ewa-sharp>` (default: catmullrom)
    - `--depth <8|16>` (default: 16)
    - `--sharpen <0-20>` (default: 5)
    - `--bpc/--no-bpc` - Black point compensation

## GUI (studio)
- Builds with CLI by default; requires X11 for monitor ICC profiling
- To build without GUI/X11 deps: `cargo build --release --no-default-features`
- Monitor ICC support only works on X11 (not Wayland)

## Testing
- One test requires ghostscript: `tests/pipeline_validation.rs::print_pipeline_pdf_output_is_unmodified`
- If ghostscript missing, that test will fail; others should pass
- Generated `*_out.tif` and `*.tif` files are automatically gitignored

## Key Dependencies
- Runtime: libcups2, lcms2, libx11, libxrandr, ghostscript, libtiff-tools
- Optional X11 deps (for monitor ICC): libx11-dev, libxrandr-dev (enabled by default)
- Build with `--no-default-features` to exclude X11 requirements (headless/Wayland)

## Architecture Notes
- Library (`src/lib.rs`): processor, layout_engine, monitor_icc, printer_discovery modules
- CLI (`src/main.rs`): Command parsing delegates to processor::process()
- Image pipeline (`src/processor.rs`): ICC transform → resample → USM sharpen → TIFF
- Layout (`src/layout_engine.rs`): Page layout, borders, rotations
- GUI (`src/bin/studio/`): eframe/egui app; monitor ICC requires X11
- Printer discovery (`src/printer_discovery/`): CUPS queries via FFI

## Important Constraints
- Do not use `--no-default-features` for production builds; monitor ICC is essential for color accuracy
- The `print_pipeline_pdf_output_is_unmodified` test validates tiff2ps → gs → PDF pipeline preserves pixels
- All TIFF output paths are configurable; default behavior preserves 16-bit depth unless `--depth 8` specified