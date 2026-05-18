# Vibeprint

ICC-aware print layout engine (Rust). Two binaries: `vibeprint` (CLI) and `studio` (GUI).

## Build

```bash
cargo build --release                    # Build both binaries
cargo build --release --no-default-features  # CLI-only (no X11 deps)
```

## Run CLI

```bash
cargo run --release --bin vibeprint -- process --input in.tif --output out.tif --dpi 720
cargo run --release --bin vibeprint -- printers                      # List CUPS printers
cargo run --release --bin vibeprint -- meta image.tif              # Image metadata
```

## Process Options

- `--intent <relative|perceptual|saturation>` (default: relative)
- `--engine <catmullrom|lanczos3|iterative-step|mitchell-ewa|mitchell-ewa-sharp>` (default: catmullrom)
- `--depth <8|16>` (default: 16)
- `--sharpen <0-20>` (default: 5)
- `--bpc/--no-bpc` - Black point compensation

## GUI (studio)

```bash
cargo run --release --bin studio  # Requires X11 for monitor ICC profiling
```

Monitor ICC only works on X11, not Wayland.

## Test

```bash
cargo test  # One test requires ghostscript: print_pipeline_pdf_output_is_unmodified
```

Generated `*_out.tif` and `*.tif` files are gitignored.

## Dependencies

Runtime: libcups2, lcms2, libx11, libxrandr, ghostscript, libtiff-tools

## Architecture

- `src/lib.rs`: processor, layout_engine, monitor_icc, printer_discovery modules
- `src/main.rs`: CLI delegates to processor::process()
- `src/processor.rs`: ICC transform → resample → USM sharpen → TIFF
- `src/bin/studio/`: eframe/egui GUI