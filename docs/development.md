# Development

[← README](../README.md)

## Build from source

Install rustup. The repository pins Rust in `rust-toolchain.toml` and locks dependencies
in `Cargo.lock`. No C compiler, cross-GCC, Android SDK/NDK, JDK, Python, or GUI library
is needed for the release build. The build downloads Rust's target standard libraries
and uses the bundled `rust-lld` linker. First builds require network access to Rust/crate
registries; subsequent builds use their caches.

```sh
cargo xtask build --target x86_64-unknown-linux-musl
./target/x86_64-unknown-linux-musl/release/adb-input run

# Cross-build an ARM64 Linux release from the same source and build host:
cargo xtask build --target aarch64-unknown-linux-musl
```

`cargo xtask build` without a target builds for the Rust host triple. Both Android
agents are statically linked and embedded into the desktop executable. `cargo build`
and `cargo test` alone build development binaries without embedded agents; use
`--agent PATH` for development or `xtask build` for a complete executable.
Custom Cargo target directories are resolved through Cargo metadata.

## Verify / package

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
sh tests/install.sh
cargo xtask dist --target x86_64-unknown-linux-musl
cargo xtask dist --target aarch64-unknown-linux-musl
```

Packaging additionally uses GNU tar and sha256sum. Archives and checksums are written to
`dist/`. Release CI builds both host architectures, validates the tag against the Cargo
version, and attaches the archives, checksums and installer to a GitHub release.
Publishing a `vX.Y.Z` tag is a separate action; creating these files does not publish.

`adb-input probe` tests agent startup/device creation and cleanup without capturing
or typing. Before this Rust implementation, the basic UHID path was tested on an S25;
the Rust agent, CLI keyboard/mouse forwarding and the original Ctrl+Alt+R switching were then
confirmed on the same device over a VPN from a remote desktop session. Other vendors
and ARM64 Linux hosts have not received interactive hardware testing yet.

## Workspace

- `crates/cli`: commands, terminal UI, ADB lifecycle and desktop input.
  - `input/mod.rs`: input switching, held keys, event loop and forwarding.
  - `input/source.rs`: evdev discovery, capabilities and device grabs.
  - `input/motion.rs`: pointer buttons, relative/absolute movement and report boundaries.
  - `control.rs`: process signals and session lifetime.
- `crates/protocol`: wire packets and HID keyboard/mouse report encoding.
- `crates/agent`: Android UHID devices and transport watchdog.
- `xtask`: cross-builds and release packaging.
