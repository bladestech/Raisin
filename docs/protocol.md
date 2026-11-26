# Razer HID protocol notes

How Raisin talks to Razer mice, and what each command does. Most of this follows the [OpenRazer](https://github.com/openrazer/openrazer) driver; the commands marked **Raisin finding** were discovered by probing a Viper V3 HyperSpeed and are not in OpenRazer as of this writing.

## Transport

The mouse (or its wireless receiver) is a USB HID device with vendor ID `0x1532`. Configuration uses **HID feature reports** on the interface whose feature report length is 91 bytes (report ID `0x00` plus a 90-byte payload). On the Viper V3 HyperSpeed receiver that is interface `MI_00`.

On Windows:

1. Enumerate HID interfaces with SetupAPI and find one with `FeatureReportByteLength == 91`.
2. Open it with `CreateFileW` and **zero access rights** (shared read/write). Windows holds mouse collections exclusively, but feature reports still work on a handle opened this way.
3. Send a request with `HidD_SetFeature`, wait, then read the response with `HidD_GetFeature`.

Wireless receivers need a delay between the two calls. Raisin uses OpenRazer's per-model values: 0.6 ms for most wired mice, 31 ms for most receivers, 59.9 ms for the Viper V3 HyperSpeed and similar, and 400 ms for the Atheris receiver.

### Per-model details

Mice differ in their transaction ID (`0x1F`, `0x3F` or `0xFF`; a few models use different IDs for different features), receiver delay, DPI and polling commands, limits, and which features exist. Raisin keeps these in [`src/devices_table.rs`](../src/devices_table.rs), generated from OpenRazer's driver and daemon by [`tools/openrazer_devices.py`](../tools/openrazer_devices.py).

## Packet layout (90 bytes)

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | Status: `0x00` in requests; see below in responses |
| 1 | 1 | Transaction ID (`0x1F` for the Viper V3 HyperSpeed; other models use `0xFF` or `0x3F`) |
| 2 | 2 | Remaining packets (big-endian), `0` |
| 4 | 1 | Protocol type, `0` |
| 5 | 1 | Data size (number of argument bytes used) |
| 6 | 1 | Command class |
| 7 | 1 | Command ID. The high bit set means "get" |
| 8 | 80 | Arguments |
| 88 | 1 | CRC: XOR of bytes 2 through 87 |
| 89 | 1 | Reserved, `0` |

Response status codes:

| Status | Meaning |
|---|---|
| `0x01` | Busy: retry |
| `0x02` | Success |
| `0x03` | Failure (also returned for out-of-range values) |
| `0x04` | Timeout: usually the mouse is asleep or switched off |
| `0x05` | Not supported |

A valid response echoes the request's class, command ID and remaining-packets fields. Raisin retries up to 5 times, 10 ms apart, on busy, mismatched or failed transfers.

## Commands

`VARSTORE` (`0x01`) writes to the mouse's onboard memory; `NOSTORE` (`0x00`) applies without saving. Multi-byte values are big-endian.

### Device

| Command | Class/ID | Size | Arguments |
|---|---|---|---|
| Get firmware version | `00/81` | 2 | response `arg[0]` = major, `arg[1]` = minor |
| Get serial number | `00/82` | 22 | response `arg[0..22]` = ASCII, zero-padded |
| Get polling rate | `00/85` | 1 | response `arg[0]`: `0x01` = 1000 Hz, `0x02` = 500 Hz, `0x08` = 125 Hz |
| Set polling rate | `00/05` | 1 | `arg[0]` as above |
| Get HyperPolling rate | `00/C0` | 1 | response `arg[1]`: `0x01` = 8000, `0x02` = 4000, `0x04` = 2000, `0x08` = 1000, `0x10` = 500, `0x20` = 250, `0x40` = 125 Hz |
| Set HyperPolling rate | `00/40` | 2 | `arg[0]` = `0x00`, then sent again with `0x01`; `arg[1]` as above |

HyperPolling mice (up to 8000 Hz) use the `00/C0` and `00/40` pair. Razer's software sends each set request twice, first with `arg[0] = 0x00` and then with `0x01`; on three models (Viper 8KHz, the HyperPolling dongle and a DeathAdder V3 Pro variant) the second request uses transaction ID `0xFF`.

### DPI

| Command | Class/ID | Size | Arguments |
|---|---|---|---|
| Get DPI | `04/85` | 7 | request `arg[0]` = store; response `arg[1..3]` = X, `arg[3..5]` = Y |
| Set DPI | `04/05` | 7 | `arg[0]` = store, `arg[1..3]` = X, `arg[3..5]` = Y |
| Get DPI stages | `04/86` | 38 | request `arg[0]` = `VARSTORE`; response as below |
| Set DPI stages | `04/06` | 38 | `arg[0]` = store, `arg[1]` = active stage (1-based), `arg[2]` = count (1 to 5), then 7 bytes per stage from `arg[3]`: index, X (2), Y (2), `0`, `0` |

The Viper V3 HyperSpeed accepts 100 to 30000 DPI; other models' limits are in the device table.

Some older mice (Naga 2012, DeathAdder 2013 and similar) use one byte per axis instead:

| Command | Class/ID | Size | Arguments |
|---|---|---|---|
| Get DPI (byte) | `04/81` | 3 | response `arg[0]` = X, `arg[1]` = Y |
| Set DPI (byte) | `04/01` | 3 | `arg[0]` = X, `arg[1]` = Y, `arg[2]` = `0` |

The byte is scaled over 0 to 6750 DPI: `byte = dpi * 255 / 6750`.

### Scroll wheel (class `0x02`, Basilisk V3 family)

| Command | Class/ID | Size | Arguments |
|---|---|---|---|
| Get scroll mode | `02/94` | 2 | request `arg[0]` = `VARSTORE`; response `arg[1]`: `0` = tactile, `1` = free spin |
| Set scroll mode | `02/14` | 2 | `arg[0]` = `VARSTORE`, `arg[1]` as above |
| Get scroll acceleration | `02/96` | 2 | response `arg[1]` = `0` or `1` |
| Set scroll acceleration | `02/16` | 2 | `arg[0]` = `VARSTORE`, `arg[1]` = `0` or `1` |
| Get Smart Reel | `02/97` | 2 | response `arg[1]` = `0` or `1` |
| Set Smart Reel | `02/17` | 2 | `arg[0]` = `VARSTORE`, `arg[1]` = `0` or `1` |

### Power and battery (class `0x07`)

| Command | Class/ID | Size | Arguments |
|---|---|---|---|
| Get battery level | `07/80` | 2 | response `arg[1]` = level, 0 to 255 |
| Get low-battery threshold | `07/81` | 1 | response `arg[0]` = raw level, 0 to 255 |
| Set low-battery threshold | `07/01` | 1 | `arg[0]` = raw level |
| Get sleep timer | `07/83` | 2 | response `arg[0..2]` = seconds |
| Set sleep timer | `07/03` | 2 | `arg[0..2]` = seconds, 60 to 900 |
| Get charging status | `07/84` | 2 | response `arg[1]` = `0` or `1` (always `0` on AA mice) |
| Get cell voltage | `07/85` | 3 | **Raisin finding**, see below |
| Get battery type | `07/94` | 1 | **Raisin finding**, see below |
| Set battery type | `07/14` | 1 | **Raisin finding**, see below |

**Low-battery threshold.** This is the level at which the mouse blinks its LED red. The raw value maps to a percentage as `raw * 100 / 255` (OpenRazer: `0x0C` = 5%, `0x26` = 15%, `0x3F` = 25%). On the Viper V3 HyperSpeed the firmware stores any value but only acts on roughly 5 to 30%: `0` does not turn the warning off, and 50% does not trigger it. Raisin limits the setting to 5 to 30%.

**Battery type** (`07/94` get, `07/14` set). Selects the discharge curve the firmware uses to turn cell voltage into the battery level:

| Value | Curve |
|---|---|
| `0` | Alkaline (the default) |
| `1` | NiMH (inferred: the only remaining option, and a fresh alkaline reads 100% on it as expected) |
| `2` | Lithium |
| `3` and up | rejected with status `0x03` |

Verified with a fresh alkaline AA: it reads 39% (raw 100) on the Lithium curve and 100% (raw 255) on the Alkaline curve.

**Cell voltage** (`07/85`). Response `arg[1..3]` is a big-endian value that tracks the cell voltage live (`arg[0]` is `0`). Treated as a 12-bit ADC reading with a 1.8 V reference:

```text
millivolts = raw * 1800 / 4095
```

Checked against a multimeter: a fresh alkaline read 1.59 V (meter about 1.60 V), a used alkaline 1.35 V (meter 1.38 V) and a Li-ion AA 1.50 V (meter 1.53 V). It reads slightly low because the mouse measures under load.

### Other responses seen on the Viper V3 HyperSpeed

Read-only probing (`raisin scan`) also found these, with meanings not yet known:

| Class/ID | Example response |
|---|---|
| `07/82` | `55` (OpenRazer: dock brightness; this mouse has no dock) |
| `07/8D` | `00 00 28 01 9C 01 4F 05`: constant across batteries, possibly calibration data |

## Events from the mouse

The mouse also sends small input reports by itself, on the receiver's vendor collections (interface `mi_01`, the generic-desktop collections with usage `0x0000`; collection 6 on the Viper V3 HyperSpeed). They can be read with a plain `ReadFile`, with no polling.

| Report | Meaning |
|---|---|
| `05 02 XX XX YY YY` | DPI changed (the DPI button): X and Y DPI, big-endian. `05 02 0C 80 0C 80` = 3200 DPI. |

Seen on the Viper V3 HyperSpeed by pressing the DPI button through all five stages; each press sent exactly one report. Raisin's DPI popup is built on this (`src/events.rs`).

## Exploring safely

`raisin scan` only sends "get" commands (high bit set), which have not been seen to change anything. Writing to unknown commands can change settings you can't easily read back, so test writes one at a time, read the value back, and restore it.
