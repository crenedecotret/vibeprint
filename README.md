# Vibeprint

ICC-aware print layout engine built in Rust. Two binaries: `vibeprint` (CLI) and `studio` (GUI).

> This was entirely vibe coded. I haven't written a line of code in 30 years.

**Note:** Monitor ICC profile loading requires X11 — does **not** work on Wayland (for now).

## System Dependencies

### Rust toolchain
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### Ubuntu
```bash
sudo apt install \
    libcups2 cups-client libcups2-dev \
    liblcms2-2 liblcms2-dev \
    libx11-6 libx11-dev \
    libxrandr2 libxrandr-dev \
    ghostscript \
    libtiff-tools
```

### Fedora
```bash
sudo dnf install \
    cups-libs cups-client \
    lcms2 lcms2-devel \
    libX11 libX11-devel \
    libXrandr libXrandr-devel \
    ghostscript \
    libtiff-tools
```

## Build

```bash
# Clone
git clone https://github.com/crenedecotret/vibeprint.git
cd vibeprint

# CLI-only build (no X11 deps needed)
cargo build --release --no-default-features

# Full build with Studio GUI (requires all system deps above)
cargo build --release
```

Binaries are placed in `target/release/`:
- `vibeprint` — command-line image processor
- `studio` — GUI application

## Quick Start (CLI)

```bash
cargo run --release --bin vibeprint -- process --input in.tif --output out.tif --dpi 720
cargo run --release --bin vibeprint -- printers          # List CUPS printers
cargo run --release --bin vibeprint -- meta image.tif     # Image metadata
```

## Quick Start (GUI)

```bash
cargo run --release --bin studio
```

Removable-device Mount via udisks2 requires a polkit authentication agent running in the session -- built-in on GNOME/KDE, but on minimal compositors like Sway/Hyprland install and autostart one (e.g. `polkit-gnome` >= 0.105-7, `hyprpolkitagent`); without an agent Mount is silently denied (device stays "not mounted") while enumeration still works.

## Test

```bash
cargo test
```

## More Info

- CLI options: see `cargo run --bin vibeprint -- --help`
- Crop/border/inversion internals: `CROP_AND_BORDER.md`
- Agent instructions: `AGENTS.md`
