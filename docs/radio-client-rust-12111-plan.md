# Radio Client Rust — Minecraft Java 1.21.11 port

Status: **planning / compatibility audit**, not yet a playable 1.21.11 build.

## Goals
- Shared native Rust client for Android ARM64 and desktop (Windows/Linux; macOS later).
- Target **Minecraft Java Edition 1.21.11**, not the inherited 26.3 version.
- Keep the original `starling` branch unchanged.
- Prioritize low-end Android graphics compatibility, input and memory use.
- Retain upstream MIT license and copyright notices. Obtain Minecraft assets through legitimate user-owned game files.

## Confirmed starting points
- Workspace includes `horizon-client`, `horizon-android`, `horizon-protocol`, and shared world/render crates.
- `horizon-protocol/Cargo.toml` currently describes the **26.3 protocol (777)**.
- Existing Android workflow triggers on `android`, `starling`, and `integration`, **not** this branch.
- Desktop client exists as a separate crate. Android builds use `packaging/android/build_apk.py`.
- Native engine mods are not Fabric Java mod binaries.

## First implementation milestones
1. **Version inventory:** find all 26.3-specific protocol IDs, registries, asset preparation paths, data fixers, worldgen assumptions, UI/version strings, and game rules. Record 1.21.11 equivalents from reliable versioned sources.
2. **Build baseline:** verify `cargo check -p horizon-protocol` and `cargo check -p horizon-client` on a suitable desktop runner before changing version-dependent code. Record failures.
3. **1.21.11 protocol adapter:** implement version-specific handshake, configuration, play packets and registries with fixture tests. Do not simply change the protocol number.
4. **Asset preparation:** allow legally obtained 1.21.11 manifests/assets, with validation and isolated cache paths.
5. **Gameplay/world parity:** reconcile block/item registries, entity metadata, worldgen and saves with 1.21.11; run tests before joining live servers.
6. **Android baseline:** create a branch-scoped preview workflow for ARM64, using a distinct app identifier and debug signing; verify on a real Android device.
7. **Performance:** profile FPS, frame times, memory, GPU compatibility and input latency on a 6 GB RAM Android tablet; tune based on measurements.

## Guardrails
- Do not claim 1.21.11 support until a version-specific handshake and join test pass.
- Do not replace or rewrite upstream code wholesale before the audit.
- Keep any new workflow scoped to `radio-client-rust`; avoid release publishing and upstream signing secrets.
- No automatic claims of Fabric mod compatibility or 60 FPS.
