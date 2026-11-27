# Changelog

All notable changes to Raisin are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.5] - 2026-11-27

### Added

- Battery history: the tray records a reading every 5 minutes to `%APPDATA%\Raisin\battery.csv` (kept for 60 days), and the new Battery tab graphs the last 7 days. With the Li-ion profile the graph shows voltage instead of the percentage.
- Battery estimate: once there are at least 12 hours and 3% of drain since the last battery change or charge, the Battery tab, the tray tooltip and `raisin info` show roughly how long the battery should last ("about 3 weeks left"). Not shown for the Li-ion profile, whose percentage means nothing.
- `raisin history`: where the history file is, how many readings it holds, and the estimate.

### Changed

- The settings window has a Battery tab for the graph, battery type and warning level. The Mouse tab keeps DPI and the mouse details, and the window is shorter.

## [0.2.0] - 2026-11-26

### Added

- Automatic switching: per-app rules that change the power mode and/or DPI stage while an app is in front, and an option for Power Saver while the PC is on battery or energy saver. Your settings come back when you switch away or quit Raisin. The tray tooltip shows when a rule is active.
- DPI popup: pressing the DPI button shows the new DPI and stage near the bottom of the screen. The mouse reports the press itself, so Raisin doesn't poll for it. On by default; turn it off in Settings > App.
- Keyboard shortcuts (off by default): Ctrl+Alt+= and Ctrl+Alt+- step through DPI stages, Ctrl+Alt+P cycles power modes, each with a popup.
- About section in the settings window: version, links to GitHub, releases and the issue tracker, and credits.

### Changed

- The settings window is split into tabs (Mouse, Performance, Automatic, App), so it stays compact, and it scrolls when a screen is too small to fit it.

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

[Unreleased]: https://github.com/bladestech/Raisin/compare/v0.2.5...HEAD
[0.2.5]: https://github.com/bladestech/Raisin/compare/v0.2.0...v0.2.5
[0.2.0]: https://github.com/bladestech/Raisin/compare/v0.1.5...v0.2.0
[0.1.5]: https://github.com/bladestech/Raisin/compare/v0.1.0...v0.1.5
[0.1.0]: https://github.com/bladestech/Raisin/releases/tag/v0.1.0
