# SnapToggle

A tiny native Windows tray app (Rust, ~100 KB) that toggles
**Settings → System → Multitasking → Snap windows** with one click.

- **Left-click** tray icon → Snap windows ON/OFF (colour icon = on, grey icon with red slash = off)
- **Keybind / Global Hotkey** → Toggle anywhere with a customizable combination (e.g. `Win + Alt + S`, `Ctrl + Shift + S`)
- **Right-click** → menu: toggle, configure keybind, open Multitasking settings, *Start with Windows*, Exit
- Auto-starts at login (HKCU\...\Run, no admin needed); opt out from the menu
- Single instance, survives Explorer restarts, syncs if you change it in Settings

## Build (on Windows)
```
rustup target add x86_64-pc-windows-msvc   # usually already the default
cargo build --release
```
Output: `target\release\snaptoggle.exe`. Run it once; it registers itself for auto-start.
(Requires the MSVC build tools, which `rustup` installs prompt for.)

## Layout
- `src/main.rs`  – app
- `build.rs`     – embeds icons (ID 1 = on / exe icon, ID 2 = off)
- `assets/`      – `icon.ico`, `icon_off.ico`, `logo.png`, `logo.svg`
- `make_icons.py` – regenerates the icons/logo (needs Pillow)

## How it works
Reads/writes `SPI_GETWINARRANGING` / `SPI_SETWINARRANGING` (applies instantly) and mirrors
`HKCU\Control Panel\Desktop\WindowArrangementActive`, which is what the Settings toggle uses.
