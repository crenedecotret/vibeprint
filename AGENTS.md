# Vibeprint

ICC-aware print layout engine (Rust). Two binaries: `vibeprint` (CLI) and `studio` (GUI). See `README.md` for setup and quick start; this file only covers what an agent would otherwise get wrong.

## Build

```bash
cargo build --release                       # both binaries (default features = monitor-icc)
cargo build --release --no-default-features # CLI-only, skips X11 + optional udisks2 — verification only (no X11, no D-Bus)
```

- **Always build with default features when producing artifacts for the user.** Only use `--no-default-features` to check that the CLI still compiles without X11. The `monitor-icc` feature enables `x11` + `libc` and is required for `studio`.
- **Gotcha:** `cargo build --release --no-default-features` OVERWRITES `target/release/studio` with the CLI-only (poll-path, no udisks2) build, and a later plain `cargo build --release` may NOT rewrite it (cargo's fingerprints are per-feature-set, so it can report the default build as fresh while the binary on disk is stale). After any `--no-default-features` build, always finish with a default `cargo build --release` AND verify the binary contains udisks2 (e.g. `grep -a -c "org.freedesktop.UDisks2" target/release/studio` > 0), or `cargo clean -p vibeprint` before the default rebuild.
- No rustfmt/clippy config, no Makefile, no CI. Rust defaults apply.

## Test

```bash
cargo test                              # everything
cargo test --lib                        # unit tests only (skips integration suites)
cargo test --test pipeline_validation   # integration: engine smoke, sharpen, layout, composite, PDF roundtrip
cargo test --test safe_8bit_print_path  # integration: ICC embedding toggle for 8-bit output
```

- Unit tests are inline in five files: `src/layout_engine.rs`, `src/monitor_icc.rs`, `src/printer_discovery.rs`, `src/bin/studio/app.rs`, `src/bin/studio/processing.rs`. Add new tests in the matching inline `#[cfg(test)] mod tests` block.
- `print_pipeline_pdf_output_is_unmodified` (in `pipeline_validation.rs`) shells out to **`tiff2ps`** (from `libtiff-tools`) and **`gs`** (ghostscript). It will fail if either binary is missing.
- `Cargo.toml` has an empty `[dev-dependencies]` section — tests use the crate's regular deps. Add dev-only deps there if needed.
- `tests/bug_tests/` is currently an empty directory (no harness wired up).

## CLI Gotchas

- `--dpi <N>` is **required** for `vibeprint process`; there is no default.
- `--bpc` defaults to on **only** for `--intent relative`; explicitly pass `--bpc` / `--no-bpc` if you want to override for other intents.
- Engine flag accepts: `catmullrom` (alias `mks`), `lanczos3`, `iterative-step`, `mitchell-ewa`, `mitchell-ewa-sharp` (alias `mitchell-sharp`, **default**).
- Generated outputs `*_out.tif`, `*_out.tiff`, engine-suffixed `*_<engine>.tif`, and a broad `*.tif` in the repo root are all gitignored (see `.gitignore`). Don't be surprised when processed output never shows up in `git status`.

## GUI (studio)

- Entry point: `src/bin/studio/main.rs` (cargo bin discovered via `src/bin/studio/` layout, not declared in `Cargo.toml`).
- Monitor ICC profile loading uses X11 directly — **does not work on Wayland**. Run under XWayland or a real X session when testing that path.
- Removable-device Mount via udisks2 (`Filesystem.Mount`) is polkit-gated and needs a polkit authentication agent running in the session -- built-in on GNOME/KDE, but on minimal compositors like Sway/Hyprland install and autostart one (e.g. `polkit-gnome` >= 0.105-7, `hyprpolkitagent`, `polkit-kde-agent`, `lxpolkit`, `mate-polkit`); without an agent Mount is silently denied (device stays "not mounted") while enumeration and yank still work, and this is not display-server-dependent.

## Architecture (only the non-obvious bits)

- `src/lib.rs` exposes four modules: `processor`, `layout_engine`, `monitor_icc`, `printer_discovery`. The CLI (`src/main.rs`) calls `processor::process()`; the GUI calls `processor::process_composite_page()`.
- Studio code lives under `src/bin/studio/` with UI split into `ui/{canvas,left_panel,right_panel,modals}.rs`. `app.rs` holds the `eframe::App` state and the `queued_box_px` helper that the canvas and processor depend on. Device detection in `src/bin/studio/devices.rs` uses an optional `udisks2` feature (zbus, system D-Bus) for rich enumeration + mount actions, with a zero-dependency `/proc`+`/sys` polling fallback.
- `src/printer_discovery/cups_ffi.rs` contains hand-written CUPS bindings — there is no `cups-sys` crate dependency.

## Crop / Border / Orientation Logic

**This is the single highest-risk area in the codebase.** Read `CROP_AND_BORDER.md` before touching anything that selects cell dimensions, computes crop UVs, or handles border changes. Key invariants the file documents:

- `PrintSize` is stored portrait-normalized (`w <= h`); every dimension-selection site must orient to the source aspect first.
- `force_original_orientation` (FOO) forces `will_rotate = false` and orients the cell to the source's natural orientation.
- `FOO && crop_inverted` is the trap case: cell dimensions swap from natural, but image content stays un-rotated. Every `(w, h)` vs `(h, w)` decision must check FOO and FOO+inverted **before** `will_rotate`.
- In the border-change handler in `src/bin/studio/ui/right_panel.rs`, border width must be set **before** crop UV recalculation, or aspect drifts.
- The testing checklist at the bottom of `CROP_AND_BORDER.md` enumerates every combination that must stay correct — run through it mentally for any change in this area.

## Scratch / Local-Only

- `.opencode/` is gitignored. `.opencode/plans/` holds prior agent plans and bug audits — useful prior art when investigating a regression, but not authoritative.
- `cargo_check.log` is gitignored; safe to leave or delete.
