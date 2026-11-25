#!/usr/bin/env python3
"""Test the Radio probe or game against a scratch Minecraft 1.21.11 server."""

import argparse
import ctypes
import json
import math
import os
import re
import socket
import subprocess
import tempfile
import time
from contextlib import ExitStack
from pathlib import Path


def run_probe(probe, address, server, log):
    scenarios = [
        ("RadioProbe", "RadioPeer", "--require-nether", log, ["--native-blocks"]),
        ("RadioPeer", "RadioProbe", "--overworld", log.with_name("peer.log"), []),
    ]
    with ExitStack() as stack:
        clients = []
        for name, peer, mode, path, options in scenarios:
            output = stack.enter_context(path.open("w"))
            client = stack.enter_context(subprocess.Popen(
                [probe, address, mode, name, peer, *options], stdout=output, stderr=subprocess.STDOUT,
            ))
            clients.append(client)
            stack.callback(client.kill)
        crossed = False
        spawned_gear_mobs = False
        deadline = time.monotonic() + 85
        try:
            while True:
                statuses = [client.poll() for client in clients]
                if any(status not in (None, 0) for status in statuses):
                    raise RuntimeError(f"multiplayer probe failed with statuses {statuses}")
                if statuses == [0, 0] and crossed:
                    return
                if time.monotonic() >= deadline:
                    raise RuntimeError("two-player probe timed out")
                if not crossed:
                    logs = [path.read_text(errors="replace") for _, _, _, path, _ in scenarios]
                    if all("ready in minecraft:overworld" in text and "peer listed" in text
                           for text in logs):
                        if not spawned_gear_mobs:
                            for mob in ("zombie", "skeleton"):
                                server_command(server, f'execute at RadioProbe run summon minecraft:{mob} ~ ~ ~ '
                                               '{NoAI:1b,NoGravity:1b,equipment:{head:{id:"minecraft:iron_helmet"}}}')
                            server_command(server, "recipe give @a *")
                            spawned_gear_mobs = True
                        if all("zombie and skeleton equipment verified" in text
                               and "vanilla recipe displays verified" in text for text in logs):
                            server_command(server, "execute in minecraft:the_nether run teleport RadioProbe 0 128 0")
                            crossed = True
                time.sleep(0.25)
        finally:
            for _, _, _, path, _ in scenarios:
                print(path.read_text(errors="replace"), end="")


def server_command(server, command):
    server.stdin.write((command + "\n").encode())
    server.stdin.flush()


def x11_window_info(*selection):
    result = subprocess.run(["xwininfo", *selection], capture_output=True, text=True, timeout=5)
    return result.stdout if result.returncode == 0 else ""


def x11_window_action(window, action):
    x11 = ctypes.CDLL("libX11.so.6")
    display_type, window_type = ctypes.c_void_p, ctypes.c_ulong
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = display_type
    x11.XMapWindow.argtypes = [display_type, window_type]
    x11.XUnmapWindow.argtypes = [display_type, window_type]
    x11.XSetInputFocus.argtypes = [display_type, window_type, ctypes.c_int, ctypes.c_ulong]
    x11.XGetInputFocus.argtypes = [
        display_type, ctypes.POINTER(window_type), ctypes.POINTER(ctypes.c_int),
    ]
    x11.XSync.argtypes = [display_type, ctypes.c_int]
    x11.XCloseDisplay.argtypes = [display_type]
    display = x11.XOpenDisplay(None)
    if not display:
        raise RuntimeError("cannot open the X11 display")
    try:
        if action == "hide":
            x11.XUnmapWindow(display, window)
        elif action == "blur":
            x11.XSetInputFocus(display, 1, 2, 0)  # PointerRoot, RevertToParent, CurrentTime
        else:
            x11.XMapWindow(display, window)
            x11.XSetInputFocus(display, window, 2, 0)
        x11.XSync(display, 0)
        if action != "hide":
            focus, revert_to = window_type(), ctypes.c_int()
            x11.XGetInputFocus(display, ctypes.byref(focus), ctypes.byref(revert_to))
            if (focus.value == window) != (action == "focus"):
                raise RuntimeError(f"could not {action} the Horizon window")
    finally:
        x11.XCloseDisplay(display)


def run_engine(binary, resources, address, server, server_log, root, x11_focus=False):
    logs = root / "engine-console.log"
    endpoint = root / "control.json"
    env = os.environ.copy()
    env.update(
        HORIZON_HOME=str(root / "home"),
        HORIZON_LAUNCHER_HOME=str(root / "launcher"),
        XDG_DATA_HOME=str(root / "data"),
        XDG_CONFIG_HOME=str(root / "config"),
        HORIZON_LAUNCHER_NO_SELF_UPDATE="1",
        HORIZON_NO_AUDIO="1",
    )
    game = root / "client"
    game.mkdir()
    display_args = ["--width", "480", "--height", "320"] if x11_focus else ["--headless"]
    if x11_focus:
        env["WINIT_UNIX_BACKEND"] = "x11"
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("WAYLAND_SOCKET", None)
    args = [
        binary, *display_args, "--resources", resources,
        "--game-dir", game, "--control-socket", endpoint, "--server", address,
        "--name", "RadioEngine", "--render-distance", "3",
    ]

    def request(command):
        info = json.loads(endpoint.read_text())
        with socket.create_connection(("127.0.0.1", info["port"]), timeout=5) as conn, conn.makefile() as r:
            conn.sendall(("auth " + info["token"] + "\n").encode())
            if json.loads(r.readline()) != {"ok": True}:
                raise RuntimeError("game control socket rejected its token")
            conn.sendall((command + "\n").encode())
            reply = json.loads(r.readline())
            return reply.get("result", reply)

    def wait_for(check, description, client=None, timeout=55):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and (client is None or client.poll() is None):
            try:
                result = check()
            except (OSError, ValueError):
                result = None
            if result:
                return result
            time.sleep(0.25)
        raise RuntimeError(f"timed out waiting for {description}")

    def has_block(name, state=None, x=2):
        block = request(f"block {x} 70 2")
        return block.get("loaded") and block.get("block") == name and (
            state is None or block.get("state") == state
        )

    def is_nether():
        state = request("state")
        return state.get("dimension") == "minecraft:the_nether" and state.get("player") is not None

    def ready_on_platform():
        state = request("state")
        player = state.get("player")
        if state.get("screen") == "playing" and player is not None:
            x, y, z = player["position"]
            if math.hypot(x - 2.5, z - 2.5) < 0.1 and abs(y - 71.0) < 0.2:
                return [x, y, z]
        return None

    def check_window_focus(client):
        def get_window_id():
            info = x11_window_info("-name", "Horizon")
            match = re.search(r"Window id: (0x[0-9a-f]+)", info)
            return int(match.group(1), 16) if match else None

        window = wait_for(get_window_id, "the Horizon window", client)

        def map_state():
            return x11_window_info("-id", hex(window))

        x11_window_action(window, "focus")
        x11_window_action(window, "blur")
        if "Map State: IsViewable" not in map_state():
            raise RuntimeError("the game window ceased to be viewable after focus loss")
        server_command(server, "setblock 3 70 9 minecraft:glowstone")
        wait_for(
            lambda: request("block 3 70 9").get("block") == "minecraft:glowstone",
            "a server block update without window focus", client,
        )

        x11_window_action(window, "hide")
        wait_for(lambda: "Map State: IsUnMapped" in map_state(), "the unmapped window", client)
        hidden_until = time.monotonic() + 35
        server_command(server, "teleport RadioEngine 4 71 9 0 0")
        server_command(server, "setblock 4 70 9 minecraft:glowstone")

        def continued_while_hidden():
            player = request("state").get("player")
            return (
                player is not None
                and math.hypot(player["position"][0] - 4.5, player["position"][2] - 9.5) < 0.12
                and request("block 4 70 9").get("block") == "minecraft:glowstone"
            )

        wait_for(continued_while_hidden, "server packets while the window is hidden", client)
        time.sleep(max(0, hidden_until - time.monotonic()))
        if not continued_while_hidden() or console().count("RadioEngine joined the game") != 1:
            raise RuntimeError("the original server connection did not survive a hidden window")
        x11_window_action(window, "focus")
        wait_for(lambda: "Map State: IsViewable" in map_state(), "the remapped window", client)
        print("Verified live traffic during focus loss and 35 seconds hidden on the same connection", flush=True)

    def console():
        return server_log.read_text(errors="replace")

    server_command(server, "forceload add 0 0")
    server_command(server, "fill 0 70 0 12 70 12 minecraft:stone")
    server_command(server, 'setblock 6 71 2 minecraft:oak_sign{front_text:{messages:["Radio initial","","",""]}}')
    server_command(server, "setblock 2 70 2 minecraft:oak_leaves[persistent=true]")
    wait_for(lambda: "Changed the block at 2, 70, 2" in console(), "the test block")
    with logs.open("w") as out, subprocess.Popen(
        args, env=env, cwd=game, stdout=out, stderr=subprocess.STDOUT
    ) as client:
        try:
            wait_for(lambda: "RadioEngine joined the game" in console(), "the game to join", client, timeout=120)
            server_command(server, "teleport RadioEngine 2 71 2 0 0")
            wait_for(
                lambda: has_block("minecraft:oak_leaves",
                                  "minecraft:oak_leaves[distance=7,persistent=true,waterlogged=false]"),
                "the oak leaf state from the initial chunk", client,
            )
            print("Loaded the exact 1.21.11 oak leaf state in the 26.3 engine", flush=True)
            def sign_text_is(text):
                block = request("block 6 71 2")
                sign = block.get("sign", {})
                return sign.get("front", [])[:1] == [text]

            wait_for(lambda: sign_text_is("Radio initial"), "sign text in the initial chunk", client)
            server_command(server, 'data modify block 6 71 2 front_text.messages[0] set value "Radio updated"')
            wait_for(lambda: sign_text_is("Radio updated"), "the live sign update", client)
            print("Verified initial and updated 1.21.11 sign text", flush=True)
            server_command(server, 'tellraw RadioEngine {"text":"Radio 1.21.11 system message"}')
            wait_for(
                lambda: "Radio 1.21.11 system message" in json.dumps(request("messages")),
                "the server system message", client,
            )
            print("Verified a 1.21.11 server system message", flush=True)

            server_command(server, "scoreboard objectives add radio_smoke trigger")
            server_command(server, "scoreboard players enable RadioEngine radio_smoke")
            wait_for(lambda: "Enabled trigger [radio_smoke] for RadioEngine" in console(),
                     "the server's command fixture", client)
            response = request("chat /trigger radio_smoke set 774")
            if response.get("error"):
                raise RuntimeError(f"game command failed: {response['error']}")
            wait_for(
                lambda: "Triggered [radio_smoke]" in json.dumps(request("messages")),
                "the server's response to the game command", client,
            )
            server_command(server, "scoreboard players get RadioEngine radio_smoke")
            wait_for(lambda: "RadioEngine has 774 [radio_smoke]" in console(),
                     "the score set by the game command", client)
            print("Verified that the 1.21.11 server executed an unsigned player command", flush=True)

            server_command(server, "setblock 2 70 2 minecraft:polished_deepslate")
            wait_for(lambda: has_block("minecraft:polished_deepslate"), "the block update", client)
            print("Received the live 1.21.11 block update", flush=True)
            server_command(server, "fill 2 70 2 4 70 2 minecraft:glowstone")
            wait_for(
                lambda: all(has_block("minecraft:glowstone", x=x) for x in range(2, 5)),
                "the batched section block update", client,
            )
            wait_for(
                lambda: request("light 2 70 2 4 70 2").get("block") == "fff",
                "glowstone block light", client,
            )
            print("Received the batched block update and block light", flush=True)
            before = wait_for(ready_on_platform, "the player on the test platform", client)
            movement = request("move forward 0.8")
            if movement.get("error"):
                raise RuntimeError(f"game movement failed: {movement['error']}")
            server_command(server, "data get entity RadioEngine Pos")
            report_prefix = "RadioEngine has the following entity data: ["

            def server_position():
                for line in console().splitlines():
                    if report_prefix in line:
                        coords = line.split(report_prefix, 1)[1].rstrip("]").split(",")
                        return [float(value.strip().rstrip("dD")) for value in coords]
                return None

            position = wait_for(server_position, "the server's player position", client)
            if math.hypot(position[0] - before[0], position[2] - before[2]) <= 0.4:
                raise RuntimeError(f"server did not receive horizontal movement: {before} -> {position}")
            print("Verified local movement on the 1.21.11 server", flush=True)

            server_command(server, "teleport RadioEngine 2 71 2 0 0")
            server_command(server, "setblock 2 72 4 minecraft:stone")
            wait_for(ready_on_platform, "the player back at the block breaking position", client)
            target = "block 2 72 4"
            wait_for(lambda: request(target).get("block") == "minecraft:stone",
                     "the creative breaking target", client)
            click = request("mouse left click")
            if click.get("error"):
                raise RuntimeError(f"game attack failed: {click['error']}")
            wait_for(lambda: request(target).get("block") == "minecraft:air",
                     "the creative block break", client)
            server_command(server, "execute if block 2 72 4 minecraft:air run say Radio confirmed creative break")
            wait_for(lambda: "Radio confirmed creative break" in console(),
                     "server confirmation of the creative break", client)

            server_command(server, "gamemode survival RadioEngine")
            server_command(server, "setblock 2 72 4 minecraft:oak_planks")
            wait_for(lambda: request(target).get("block") == "minecraft:oak_planks",
                     "the survival breaking target", client)
            down = request("mouse left down")
            if down.get("error"):
                raise RuntimeError(f"game attack failed: {down['error']}")
            wait_for(lambda: request(target).get("block") == "minecraft:air",
                     "the survival block break", client, timeout=12)
            request("mouse left up")
            server_command(server, "execute if block 2 72 4 minecraft:air run say Radio confirmed survival break")
            wait_for(lambda: "Radio confirmed survival break" in console(),
                     "server confirmation of the survival break", client)
            print("Verified server-authoritative block breaking in creative and survival", flush=True)

            server_command(server, "setblock 2 72 4 minecraft:stone")
            server_command(server, "item replace entity RadioEngine hotbar.1 with minecraft:gold_block 2")
            request("hotbar 2")

            def gold_in_hand(count):
                held = request("state").get("held") or {}
                return held.get("item") == "minecraft:gold_block" and held.get("count") == count

            wait_for(lambda: gold_in_hand(2), "the translated server hotbar stack", client)
            wait_for(lambda: request(target).get("block") == "minecraft:stone",
                     "the block placement target", client)
            click = request("mouse right click")
            if click.get("error"):
                raise RuntimeError(f"game block placement failed: {click['error']}")
            wait_for(lambda: request("block 2 72 3").get("block") == "minecraft:gold_block",
                     "the placed gold block", client)
            def server_confirmed_placement():
                if "Radio confirmed placement" in console():
                    return True
                server_command(server, "execute if block 2 72 3 minecraft:gold_block run say Radio confirmed placement")
                return False

            wait_for(server_confirmed_placement, "server confirmation of block placement",
                     client, timeout=10)
            wait_for(lambda: gold_in_hand(1), "the survival stack count after placement", client)
            print("Verified server hotbar updates and authoritative survival block placement", flush=True)

            server_command(server, "setblock 2 72 3 minecraft:air")
            server_command(server, "setblock 2 72 4 minecraft:chest")
            server_command(server, "item replace block 2 72 4 container.0 with minecraft:diamond_block 3")
            server_command(
                server, "item replace block 2 72 4 container.1 with "
                "minecraft:golden_pickaxe[damage=11,enchantments={efficiency:4}]",
            )
            server_command(
                server, "item replace block 2 72 4 container.2 with "
                "minecraft:enchanted_book[stored_enchantments={mending:1}]",
            )
            wait_for(lambda: request("block 2 72 3").get("block") == "minecraft:air"
                     and request(target).get("block") == "minecraft:chest",
                     "the server chest", client)
            click = request("mouse right click")
            if click.get("error"):
                raise RuntimeError(f"game chest opening failed: {click['error']}")

            def chest_contains_stacks():
                menu = request("menu")
                if menu["screen"] != "inventory":
                    return False
                stacks = {slot["slot"]: slot["stack"] for slot in menu["slots"]}
                diamonds = stacks.get(300, {})
                pickaxe = stacks.get(301, {})
                book = stacks.get(302, {})
                return (
                    diamonds.get("item") == "minecraft:diamond_block"
                    and diamonds.get("count") == 3
                    and pickaxe.get("item") == "minecraft:golden_pickaxe"
                    and pickaxe.get("damage") == 11
                    and pickaxe.get("enchantments") == [["minecraft:efficiency", 4]]
                    and book.get("item") == "minecraft:enchanted_book"
                    and book.get("stored_enchantments") == [["minecraft:mending", 1]]
                )

            wait_for(chest_contains_stacks, "the chest's blocks, enchanted pickaxe, and enchanted book", client, timeout=10)
            if x11_focus:
                click = request("click-slot 300 shift")
                if click.get("error"):
                    raise RuntimeError(f"game chest transfer failed: {click['error']}")

                def server_confirmed_transfer():
                    if "Radio confirmed chest transfer" in console():
                        return True
                    for slots in ("inventory.*", "hotbar.*"):
                        server_command(server, f"execute if items entity RadioEngine {slots} minecraft:diamond_block"
                                       " unless items block 2 72 4 container.0 minecraft:diamond_block"
                                       " run say Radio confirmed chest transfer")
                    return False

                wait_for(server_confirmed_transfer, "the server-side chest transfer", client, timeout=10)
            request("key esc tap")
            wait_for(lambda: request("state").get("screen") == "playing", "closing the chest", client)
            print("Verified opening and closing a 1.21.11 chest with blocks and enchanted items", flush=True)
            if x11_focus:
                print("Verified the server-side chest transfer", flush=True)
            server_command(server, "gamemode creative RadioEngine")

            def pig_at(x, entity_id=None):
                return next((
                    entity for entity in request("entities 24 pig").get("entities", [])
                    if abs(entity["pos"][0] - x) < 0.12
                    and abs(entity["pos"][2] - 6.5) < 0.12
                    and (entity_id is None or entity["id"] == entity_id)
                ), None)

            server_command(server, 'summon minecraft:pig 6.5 71 6.5 {NoAI:1b,Tags:["radio_smoke"]}')
            pig = wait_for(lambda: pig_at(6.5), "the 1.21.11 pig spawn", client)
            server_command(server, "teleport @e[tag=radio_smoke,limit=1] 8.5 71 6.5")
            wait_for(lambda: pig_at(8.5, pig["id"]), "the pig's updated position", client)
            server_command(server, "kill @e[tag=radio_smoke,limit=1]")
            wait_for(
                lambda: all(entity["id"] != pig["id"] for entity in request("entities 24 pig")["entities"]),
                "the pig's removal", client,
            )
            print("Verified a remapped entity spawn, teleport, and removal", flush=True)
            if x11_focus:
                check_window_focus(client)
            server_command(server, "execute in minecraft:the_nether run teleport RadioEngine 0 128 0")
            wait_for(is_nether, "the Nether player", client)
            time.sleep(30)
            if not is_nether() or console().count("RadioEngine joined the game") != 1:
                raise RuntimeError("game lost its connection during the Nether soak")
            decode_errors = [
                line for line in logs.read_text(errors="replace").splitlines()
                if "skipped undecodable packet" in line
            ]
            if decode_errors:
                raise RuntimeError(f"game skipped a supported packet: {decode_errors[0]}")
            print("The same game connection stayed in the Nether for 30 seconds", flush=True)
            request("quit")
            client.wait(timeout=10)
            if client.returncode != 0:
                raise RuntimeError(f"game exited with status {client.returncode}")
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError):
            print("\n".join(logs.read_text(errors="replace").splitlines()[-80:]))
            raise
        finally:
            if client.poll() is None:
                client.kill()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server_jar", type=Path, help="Mojang's Minecraft 1.21.11 server jar")
    parser.add_argument("client", type=Path, help="radio_12111_probe example or horizon-client executable")
    parser.add_argument("--engine-resources", type=Path, help="prepared 26.3 resources for the full client test")
    parser.add_argument(
        "--x11-focus", action="store_true",
        help="test focus loss and an unmapped window on a dedicated X11 display (requires xwininfo)",
    )
    args = parser.parse_args()
    if args.x11_focus and not (args.engine_resources and os.environ.get("DISPLAY")):
        parser.error("--x11-focus requires --engine-resources and an X11 DISPLAY")
    server_jar = args.server_jar.resolve(strict=True)
    client = args.client.resolve(strict=True)
    resources = args.engine_resources and args.engine_resources.resolve(strict=True)

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]

    with tempfile.TemporaryDirectory(prefix="radio-12111-") as directory:
        root = Path(directory)
        (root / "eula.txt").write_text("eula=true\n")
        (root / "server.properties").write_text(
            "online-mode=false\n"
            "white-list=false\n"
            "gamemode=creative\n"
            "enforce-secure-profile=false\n"
            "server-ip=127.0.0.1\n"
            f"server-port={port}\n"
            "view-distance=3\n"
            "simulation-distance=3\n"
            "generate-structures=false\n"
            + ("level-type=minecraft:flat\n" if resources else "")
            +
            "use-native-transport=false\n"
        )
        log = root / "server-console.log"
        with log.open("w") as out, subprocess.Popen(
            ["java", "-Xmx768m", "-XX:ActiveProcessorCount=2", "-jar", server_jar, "nogui"],
            cwd=root,
            stdin=subprocess.PIPE,
            stdout=out,
            stderr=subprocess.STDOUT,
        ) as server:
            try:
                deadline = time.monotonic() + 120
                while "Done (" not in log.read_text(errors="replace"):
                    if server.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("vanilla server did not finish starting")
                    time.sleep(0.5)
                address = f"127.0.0.1:{port}"
                if resources:
                    run_engine(client, resources, address, server, log, root, args.x11_focus)
                else:
                    run_probe(client, address, server, root / "probe.log")
            except (OSError, RuntimeError, ValueError, subprocess.SubprocessError):
                print("\n".join(log.read_text(errors="replace").splitlines()[-60:]))
                raise
            finally:
                if server.poll() is None:
                    try:
                        server.communicate(b"stop\n", timeout=10)
                    except (BrokenPipeError, subprocess.TimeoutExpired):
                        server.kill()


if __name__ == "__main__":
    main()
