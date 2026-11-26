# Contributing

Thanks for helping. Bug reports, device reports and pull requests are all welcome.

## Building

You need the Rust MSVC toolchain (`rustup default stable-msvc`) and the Windows SDK, which provides `rc.exe` for embedding the icon.

```powershell
cargo build --release
```

## Before opening a pull request

CI runs these, so run them locally first:

```powershell
cargo fmt --check
cargo clippy --release --all-targets -- -D warnings
cargo test --release
```

Add a line to `CHANGELOG.md` under "Unreleased" for anything a user would notice.

## Keeping it light

Raisin's point is to be small. Please keep the tray process free of heavy dependencies; check its memory (Task Manager, "Memory" column) before and after a change. It idles at about 2 MB. The tray may draw small things with plain GDI, like the DPI popup. Direct2D and anything bigger belong in the settings window, which runs as its own process. Don't add polling: prefer events the mouse or Windows sends, as the DPI popup and automatic switching do.

## Protocol changes

[docs/protocol.md](docs/protocol.md) documents every command Raisin sends. When adding or changing one:

- Test on real hardware and describe how you checked the result (read the value back, compare with Synapse, a multimeter, and so on).
- Prefer read-only exploration (`raisin scan`). If you must test an unknown write, change one value at a time, read it back, and restore the original.
- Update `docs/protocol.md`.

## Adding or updating mice

The device table is generated from OpenRazer, so new models usually only need a regeneration:

```powershell
git clone --depth 1 https://github.com/openrazer/openrazer ..\openrazer
python -I tools\openrazer_devices.py ..\openrazer src\devices_table.rs docs\devices.md
```

The tool only reads OpenRazer's files; it doesn't run any of their code. Review the diff of `src/devices_table.rs` before committing.

**Confirming a model works:** if you've tested Raisin on a mouse, add its USB product ID to `TESTED` in `src/devices.rs` and in `tools/openrazer_devices.py`, regenerate, and mention in the pull request what you checked (DPI, polling rate, battery and so on).

**A mouse that isn't in OpenRazer:** open a [device request](https://github.com/bladestech/Raisin/issues/new?template=device_request.yml) with the output of `raisin probe`.

## Releases

Bump `version` in `Cargo.toml`, move the "Unreleased" notes in `CHANGELOG.md` under the new version, then push a matching tag (for example `v0.2.0`). The release workflow builds `raisin.exe` and publishes it with a SHA-256 checksum.
