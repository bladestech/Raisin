# Changelog

All notable changes to Raisin are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.5] - 2026-11-25

### Added

- Power modes next to polling rate and sleep timer: Power Saver (lowest polling rate, 1-minute sleep), Balanced (1000 Hz, 5 minutes) and Max (the mouse's fastest rate, 5 minutes). In the settings window, the tray menu and `raisin power`; `raisin info` shows the current mode.
- Info bubbles: hover the (i) next to a setting in the settings window to see what it does.

## [0.1.0] - 2026-11-25

First release: a lightweight Razer Synapse replacement for Windows.

### Added

- Support for 118 Razer mice, with per-model details generated from OpenRazer (`tools/openrazer_devices.py`). Tested on the Viper V3 HyperSpeed.
- Tray app with a battery gauge, logo or alternating icon, and a quick menu. Idles at about 2 MB of memory.
- Settings window with an acrylic background that only shows what the connected mouse supports. It runs as a separate process, so its memory is freed when it closes.
- DPI stages (or a single DPI control on mice without stages), including 1-byte DPI for older mice.
- Polling rate, up to 8000 Hz on HyperPolling mice.
- Battery level, charging status, and approximate cell voltage on AA mice.
- Battery type (Alkaline, NiMH, Lithium) and a Li-ion 1.5 V profile that shows the battery as High or Low, with the mouse's percentage crossed out.
- Low-battery blink threshold with a matching Windows notification.
- Scroll wheel settings for the Basilisk V3 family: free spin, acceleration, Smart Reel.
- Sleep timer.
- Mouse details: firmware version, USB ID, connection, tested status, and one-click serial number copy.
- Start with Windows.
- Command line: `info`, `dpi`, `stages`, `stage`, `poll`, `sleep`, `lowbat`, `battery`, `scroll`, `accel`, `smartreel`, `devices`, `probe`, `scan`.

[Unreleased]: https://github.com/bladestech/Raisin/compare/v0.1.5...HEAD
[0.1.5]: https://github.com/bladestech/Raisin/compare/v0.1.0...v0.1.5
[0.1.0]: https://github.com/bladestech/Raisin/releases/tag/v0.1.0
