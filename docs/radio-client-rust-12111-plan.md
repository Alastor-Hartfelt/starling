# Radio Client Rust — Minecraft Java 1.21.11 port

The branch has an experimental offline-mode Minecraft 1.21.11 multiplayer session using prepared 26.3 engine resources. The Linux engine has been tested against a local vanilla 1.21.11 server. GitHub Actions builds Linux x86-64 and Android ARM64 previews.

## Goals
- Shared native Rust client for Android ARM64 and desktop (Windows/Linux; macOS later).
- Target **Minecraft Java Edition 1.21.11**, not the inherited 26.3 version.
- Keep the original `starling` branch unchanged.
- Prioritize low-end Android graphics compatibility, input and memory use.
- Retain upstream MIT license and copyright notices. Obtain Minecraft assets through legitimate user-owned game files.

## Reproduce the multiplayer tests

Use Rust stable, Python 3, Java 21 or newer, and the Mojang 1.21.11 server jar. The baseline workflow downloads the jar and verifies its SHA-1. The full client also needs prepared 26.3 resources. Follow the source build instructions in the [README](../README.md) to prepare them with JDK 25.

```sh
cargo test --locked -p horizon-protocol --lib
cargo test --locked -p horizon-client-lib --no-default-features --lib net::tests::
cargo build --locked -p horizon-client-lib --example radio_12111_probe
python3 tools/radio_12111_smoke.py "$SERVER_JAR" target/debug/examples/radio_12111_probe
cargo build --locked -p horizon-client --no-default-features
python3 tools/radio_12111_smoke.py "$SERVER_JAR" target/debug/horizon-client \
  --engine-resources .local/horizon-client/26.3
```

The probe and engine run against a fresh local server with an isolated world and player data. The engine test verifies initial terrain, light, sign text and system messages, authoritative movement, block breaking and placement, hotbar updates, unsigned commands, mob tracking, and a dimension change on the same connection.

To repeat the desktop focus regression, run the full engine command with `--x11-focus` on a dedicated X11 display with `xwininfo`, libX11, and working Vulkan. The test moves input focus away while the game is still visible, then unmaps the window for 35 seconds. It verifies server traffic during both stages and restores the window. Xvfb and Mesa lavapipe work for this check.

For a direct desktop test, start the client with `--radio-1.21.11 --server HOST[:PORT] --name NAME --resources PATH`. Use an offline-mode 1.21.11 server.

## Android ARM64 device test

The branch's preview workflow packages a debug APK with application ID `io.github.juanluci.starling.preview`. Install it and finish the in-app 26.3 resource setup. Use a test device with USB debugging. If the device and server are both connected to the test host, `adb reverse tcp:25565 tcp:25565` routes the device's loopback port to the host's test server.

```sh
adb shell am force-stop io.github.juanluci.starling.preview
printf '%s\n' --radio-1.21.11 --server 127.0.0.1:25565 --name RadioAndroid \
  | adb shell -T 'run-as io.github.juanluci.starling.preview sh -c "cat > files/horizon-args.txt"'
adb shell am start -n io.github.juanluci.starling.preview/io.github.juanluci.starling.LauncherActivity
```

After the server records the join, press Home for at least 35 seconds, return from Recents, and confirm that the same server connection responds to a teleport without a second join. Use `adb logcat -s starling` for packet or lifecycle errors. Repeat on Wi-Fi and record frame time, memory, device, and Vulkan driver at a fixed view distance on a 6 GB Adreno device. Remove `files/horizon-args.txt` with `adb shell run-as io.github.juanluci.starling.preview rm files/horizon-args.txt` after the test.

## Remaining compatibility work

1. Expand the 1.21.11 item component codec beyond the basic supported types, then translate menu types, container transactions, and creative inventory. Exercise a chest and a 1.21.11 enchanted item against vanilla.
2. Decode entity metadata and equipment, add player info, and test two clients tracking each other.
3. Finish player chat, command suggestions and signed commands, and 1.21.11 online-mode login. Exercise the shared encrypted session against an authenticated test server once implemented.
4. Test Android backgrounding on hardware and profile frame time, memory, and thermals before choosing device-specific optimizations.

## Guardrails
- Do not claim 1.21.11 support until a version-specific handshake and join test pass.
- Do not replace or rewrite upstream code wholesale before the audit.
- Keep any new workflow scoped to `radio-client-rust`; avoid release publishing and upstream signing secrets.
- No automatic claims of Fabric mod compatibility or 60 FPS.
