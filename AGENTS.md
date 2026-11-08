# AGENTS.md

Handoff notes for coding agents and fresh sessions. Keep this current after meaningful work.

## What it is

Rig Hub: one native Rust/egui app for a mouse (via ratbagd), NZXT Kraken cooler (temps + LCD), headset (via HeadsetControl) and Blue Yeti mic (via ALSA `amixer`). Tray icon, KDE Plasma 6 widget, six themes. Linux first; Windows/macOS only get the cooler LCD and headset (see the platform table in `README.md`). Alpha. License GPL-3.0-or-later (Kraken LCD code is ported from liquidctl).

Scope: keyboards and RGB are out (OpenRGB does that).

## Layout

| Path | What |
|---|---|
| `src/main.rs` | UI, tray, single-instance socket, update-version parsing, KDE widget install, KWin window raise, status file for the widget |
| `src/workers.rs` | Shared `State`, `Settings` (load/save), one background thread per device (cooler, mouse, headset, mic) plus a one-shot update check. The UI only reads `State` and sends commands |
| `src/mouse.rs` | ratbagd over D-Bus (zbus) + live DPI stage from Logitech HID++ (hidapi). Linux only |
| `src/kraken.rs` | Kraken status (hwmon, Linux) and LCD protocol over USB (nusb/hidapi), ported from liquidctl `kraken3` |
| `src/audio.rs` | `headsetcontrol`, `amixer`, `upower --dump` parsing |
| `src/diagram.rs` | Clickable mouse picture, using Piper's artwork from the user's own install |
| `src/keys.rs` | evdev key codes ↔ names ↔ egui keys |
| `src/theme.rs` | Color themes |
| `plasmoid/` | KDE widget (`metadata.json`, `contents/ui/main.qml`). Embedded in the binary with `include_str!` |
| `assets/` | `rig-hub.svg` (app icon, embedded and shipped in release tarballs), `screenshots/` for the README |
| `install.sh` | Linux/macOS installer: downloads from `releases/latest`, puts the binary in `~/.local/bin`; on Linux also the icon, a `.desktop` file, and (asks first, with sudo) a udev rule for NZXT USB access |
| `install.ps1` | Windows installer, same idea (`releases/latest`, x86_64 zip) |
| `.coderabbit.yaml` | Turns off CodeRabbit's clippy (its sandbox has no libudev, so hidapi won't build) |

## Build, test, lint

```sh
# Debian/Ubuntu deps: sudo apt install libudev-dev pkg-config
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests are small unit tests (parsers, version compare, single-instance lock, key table, Kraken bucket math). Run all three before a PR. CI fails on any clippy warning.

## CI and releases

- `.github/workflows/ci.yml`: on push to `main` and every PR. Build + test on `ubuntu-22.04`, `windows-latest`, `macos-15`; clippy on Linux only. (`macos-14` is rejected by GitHub now.)
- `.github/workflows/release.yml`: on a `v*` tag. Builds 5 archives: `rig-hub-{x86_64,aarch64}-linux.tar.gz`, `rig-hub-{aarch64,x86_64}-macos.tar.gz` (Intel is cross-built on Apple Silicon), `rig-hub-x86_64-windows.zip`.
- **Version guard:** the first release step fails unless the tag minus `v` equals `version` in `Cargo.toml`. So: bump `Cargo.toml` (and `Cargo.lock`) in a PR, merge, then tag, e.g. `v0.1.0-alpha.5`.
- Releases are published with `make_latest: true` and **not** marked pre-release, because the installers and the update check use `releases/latest`, which skips pre-releases.

## Update notice

At startup a worker thread runs `curl -sSI --max-time 10 <repo>/releases/latest` and reads the tag from the `location:` redirect header (no GitHub API: it allows only 60 requests/hour). If that tag is newer than `CARGO_PKG_VERSION`, a "⬆ Rig Hub X is out" button shows and opens the release page. Nothing is downloaded. Silent on any failure. Version compare understands `X.Y.Z` plus `-alpha.N` / `-beta.N` / `-rc.N`, anything else counts as "no update". That's why the tag must match `Cargo.toml`: a stale version makes the app say its own release "is out".

## Gotchas (learned the hard way)

- **headsetcontrol exits 1** with an empty device list when no headset is reachable (e.g. dongle in, headset off). That's normal, not an error: `audio::headset()` skips the exit-status check and parses stdout. Setting commands (`-s`, `-i`) do check it.
- **Headset settings revert on failure:** the slider saves at once (headsetcontrol takes ~170 ms), and `set_or_revert` puts the old value back if the headset rejects it.
- **Mouse retry:** if `ratbagd` can't load the mouse (not running, unplugged, plugged in later), the worker tries again every 5 s. With a mouse but no HID++ it rechecks every 30 s, since HID++ read failures are otherwise the only unplug signal. No system D-Bus: retry every 10 s.
- **Settings** live in `<config dir>/rig-hub/settings.json` and are saved atomically (`write_atomic`: write `.tmp`, fsync, rename). The path is canonicalized first so a symlinked `settings.json` (dotfiles) keeps its target instead of being replaced. Save after releasing the `State` lock.
- **Kraken sensors:** the nzxt-kraken3 driver names hwmon differently per model. `kraken::status()` tries name prefixes `kraken` (2023/2024 models, e.g. `kraken2023`, `kraken2024elite`), then `z53` (Kraken Z), then `x53` (X series).
- **Kraken LCD buckets:** reuse a slot only when the upload fits; a bad overlap check once wiped other stored screens. GIFs are decoded one frame at a time.
- **Private files** (widget status file `rig-hub-status.json`, KWin raise script) go only in `$XDG_RUNTIME_DIR` or an owner-only `~/.cache/rig-hub`, never shared `/tmp`. The widget reads the same path.
- **Wayland close-to-tray:** winit can't hide a window, so on Linux closing destroys the window while `App`, workers and tray live on; `main()` reruns `eframe::run_native` to reopen. Raising the window goes through a KWin script over D-Bus.
- **Single instance:** a Unix socket `rig-hub.sock` in the runtime dir; a second launch pokes the first to show itself and exits.
- **Windows:** the app has no console, so every child process (`headsetcontrol`, `curl`, …) needs `CREATE_NO_WINDOW` or a black window flashes.
- **KDE widget** is reinstalled into `~/.local/share/plasma/plasmoids/com.susiee.righub` whenever its embedded files change (KDE sessions only). Plasma picks it up after re-adding the widget or restarting Plasma.

## Only verifiable on real hardware

Unit tests and CI can't cover these; test by hand and say so in the PR:

- Mouse remapping, DPI stages, profiles, DPI pop-up, unplug/replug recovery (dev rig: Logitech G502 HERO).
- Kraken temps/RPM and LCD upload, rotation, brightness (dev rig: Kraken Elite 2024, hwmon `kraken2024elite`). Kraken Z and Windows/macOS LCD are untested.
- Headset battery, sidetone, auto-off, low-battery warning (dev rig: Corsair HS80 MAX).
- Mic mute sync, gain, monitoring (Blue Yeti).
- Tray, KDE widget, and window raise on KDE Plasma 6 Wayland.
