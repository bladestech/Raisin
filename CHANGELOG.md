# Changelog

All notable changes to Raisin are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Support for 118 Razer mice, with per-model details generated from OpenRazer (`tools/openrazer_devices.py`). The settings window, tray menu and command line only show what the connected mouse supports. Tested on the Viper V3 HyperSpeed.
- HyperPolling (up to 8000 Hz), 1-byte DPI for older mice, and a single DPI control for mice without stages.
- Scroll wheel settings for the Basilisk V3 family: free spin, acceleration, Smart Reel.
- Mouse details: firmware version, USB ID, connection, tested status, and one-click serial number copy.
- Charging status for rechargeable mice.
- Li-ion profile shows the battery as High or Low, with the mouse's percentage crossed out.
- `raisin devices`, `raisin scroll`, `raisin accel` and `raisin smartreel` commands.
- First version, for the Razer Viper V3 HyperSpeed.
- Tray app with a battery gauge, logo or alternating icon, and a quick menu.
- Settings window with an acrylic background, run as a separate process so the tray stays around 2 MB.
- DPI and DPI stages, polling rate, sleep timer.
- Battery level, approximate cell voltage, battery type (Alkaline, NiMH, Lithium) and a Li-ion 1.5 V profile.
- Low-battery blink threshold with a matching Windows notification.
- Start with Windows.
- Command line: `info`, `dpi`, `stages`, `stage`, `poll`, `sleep`, `lowbat`, `battery`, `probe`, `scan`.
