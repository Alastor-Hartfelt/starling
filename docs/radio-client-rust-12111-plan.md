# Radio Client Rust — Minecraft Java 1.21.11 port

The branch has an experimental Minecraft 1.21.11 multiplayer session using prepared 26.3 engine resources. The Linux engine has been tested against a local vanilla 1.21.11 server. GitHub Actions builds Linux x86-64 and Android ARM64 previews.

## Goals
- Shared native Rust client for Android ARM64 and desktop (Windows/Linux; macOS later).
- Target **Minecraft Java Edition 1.21.11**, not the inherited 26.3 version.
- Keep the original `starling` branch unchanged.
- Prioritize low-end Android graphics compatibility, input and memory use.
- Retain upstream MIT license and copyright notices. Obtain Minecraft assets through legitimate user-owned game files.

## Replace the 26.3 resource and engine dependencies

Generate the target reports in a separate development directory with Java 21 or newer.

```sh
HORIZON_VERSIONS=/path/to/radio-version-data python3 tools/version_port.py prepare 1.21.11 --reports-only
```

The command verifies Mojang's manifest and server jar and writes reports under `1.21.11/datagen/reports`. For class and API comparisons, prepare Mojang's official unobfuscated client and server with JDK 21 or newer.

```sh
HORIZON_VERSIONS=/path/to/radio-version-data python3 tools/version_port.py prepare 1.21.11_unobfuscated --download
```

This uses the launcher profile from [Mojang's 1.21.11 release notes](https://feedback.minecraft.net/hc/en-us/articles/41809981427213-Minecraft-Java-Edition-1-21-11-Mounts-of-Mayhem). The tool verifies the profile archive and both JARs, then generates reports and indexes the classes in a separate version directory.

The initial JDK 21 audit of the resource exporters against those JARs found 75 server-side errors and 26 client-side errors. Server-side blockers include changed world constructors, registry interfaces, and command APIs. The original client build also pulled in a 26.3 model parity probe that the production exporter does not need. Splitting the blockstate schema export from that probe allows the entire client exporter bundle to compile against 1.21.11. The Rust asset loader accepted all 1,168 blockstate definitions in the official client JAR with the exported schemas. The mob-data and gameplay-facts exporters now compile and run against those JARs. Their 1.21.11 output covers all 29,671 block states and all 157 entity spawn placement entries. The collision exporter also generates the 1.21.11 registry and cached collision tables. The Rust loaders validated 29,671 states and 29,492 cached shapes by state, using DataVersion 4671 and rejecting mismatched table versions. The creative inventory exporter can also load the 1.21.11 registry layers. Radio read the resulting tab and search data using its existing Rust creative inventory loader. Using an isolated research lock for Mojang’s named 1.21.11 profile and JDK 21, the standard Rust preparer completed a private DataVersion 4671 bundle with 17,866 client resources. Its item table matched all 1,505 entries from the vanilla server data generator. The test used the client and server JARs and launcher libraries. Optional launcher asset objects such as sounds, fonts and panorama were unavailable in this run. World generation still needs runtime verification of the inline 1.21.11 surface rules and a world creation, save and reload test.

The native block registry now carries its game version through the local cache. Multiplayer chunks, individual and batched block updates, and falling blocks select block-state IDs for the loaded registry. The two-client headless test runs one client with each registry and teleports the native-registry client to the Nether.

The official 1.21.11 release defines ocean-floor and no-leaves heightmaps per state. Its fluid-flow calculation uses the same motion predicate as the ocean floor. To check all 29,671 states through the Rust terrain asset loader, including waterlogged leaves, run the following with JDK 21 and Rust installed.

```sh
python3 tools/radio_12111_heightmaps.py
```

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

The probe and engine run against a fresh local server with an isolated world and player data. The headless test checks that both clients receive the other player’s roster entry, that one can enter the Nether, and that the other receives roster removal when the first disconnects. The engine test verifies initial terrain, light, sign text and system messages, authoritative movement, block breaking and placement, hotbar updates, enchanted items in chests, unsigned commands, mob tracking, and a dimension change on the same connection.

To repeat the desktop focus regression, run the full engine command with `--x11-focus` on a dedicated X11 display with `xwininfo`, libX11, and working Vulkan. The test moves input focus away while the game is still visible, then unmaps the window for 35 seconds. It verifies server traffic during both stages and restores the window. The graphical run also verifies a chest shift-click transfer on the server. Xvfb and Mesa lavapipe work for this check.

For a direct desktop test, start the client with `--server HOST[:PORT] --name NAME --resources PATH`. An offline-mode 1.21.11 server reproduces the tested setup. The session uses the shared encrypted login path and accepts the launcher account. The encryption and compression handshake has a loopback test. Microsoft authentication still needs a live test.

## Android ARM64 device test

Open the Android preview from its app icon and finish resource preparation. Add or join a Java 1.21.11 server from the Multiplayer menu. Direct multiplayer connections use 1.21.11 by default.

The branch's preview workflow packages a debug APK with application ID `io.github.juanluci.starling.preview`. Install it and finish the in-app 26.3 resource setup. The optional automated launch below uses USB debugging. If the device and server are both connected to the test host, `adb reverse tcp:25565 tcp:25565` routes the device's loopback port to the host's test server.

```sh
adb shell am force-stop io.github.juanluci.starling.preview
printf '%s\n' --server 127.0.0.1:25565 --name RadioAndroid \
  | adb shell -T 'run-as io.github.juanluci.starling.preview sh -c "cat > files/horizon-args.txt"'
adb shell am start -n io.github.juanluci.starling.preview/io.github.juanluci.starling.LauncherActivity
```

After the server records the join, press Home for at least 35 seconds, return from Recents, and confirm that the same server connection responds to a teleport without a second join. Use `adb logcat -s starling` for packet or lifecycle errors. Repeat on Wi-Fi and record frame time, memory, device, and Vulkan driver at a fixed view distance on a 6 GB Adreno device. Remove `files/horizon-args.txt` with `adb shell run-as io.github.juanluci.starling.preview rm files/horizon-args.txt` after the test.

## Remaining compatibility work

1. Expand the 1.21.11 item component codec beyond the basic supported types, and finish creative inventory. Exercise potions, shulker boxes, crafting, and other menus against vanilla.
2. Decode entity metadata and equipment, add player info, and test two clients tracking each other.
3. Exercise the shared login path against a Minecraft 1.21.11 server with online mode enabled and a signed-in Microsoft account. Add player chat, command suggestions, and signed commands.
4. Verify the default 1.21.11 join from the Android server UI. Test Android backgrounding on hardware and profile frame time, memory, and thermals before choosing device-specific optimizations.

## Guardrails
- Do not claim 1.21.11 support until a version-specific handshake and join test pass.
- Do not replace or rewrite upstream code wholesale before the audit.
- Keep any new workflow scoped to `radio-client-rust`; avoid release publishing and upstream signing secrets.
- No automatic claims of Fabric mod compatibility or 60 FPS.
