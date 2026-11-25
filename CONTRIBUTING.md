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

Raisin's point is to be small. Please keep the tray process free of heavy dependencies; check its memory (Task Manager, "Memory" column) before and after a change. Anything graphical belongs in the settings window, which runs as its own process.

## Protocol changes

[docs/protocol.md](docs/protocol.md) documents every command Raisin sends. When adding or changing one:

- Test on real hardware and describe how you checked the result (read the value back, compare with Synapse, a multimeter, and so on).
- Prefer read-only exploration (`raisin scan`). If you must test an unknown write, change one value at a time, read it back, and restore the original.
- Update `docs/protocol.md`.

## Adding a mouse

Open a [device request](https://github.com/bladestech/Raisin/issues/new?template=device_request.yml) with the output of `raisin probe`, or send a pull request with the model's USB ID and details, plus what you tested.

## Releases

Bump `version` in `Cargo.toml`, move the "Unreleased" notes in `CHANGELOG.md` under the new version, then push a matching tag (for example `v0.2.0`). The release workflow builds `raisin.exe` and publishes it with a SHA-256 checksum.
