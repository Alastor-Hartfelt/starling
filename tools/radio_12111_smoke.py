#!/usr/bin/env python3
"""Test the Radio probe or headless game against a scratch Minecraft 1.21.11 server."""

import argparse
import json
import math
import os
import socket
import subprocess
import tempfile
import time
from pathlib import Path


def run_probe(probe, address, server, log):
    with log.open("w") as out, subprocess.Popen(
        [probe, address, "--require-nether"], stdout=out, stderr=subprocess.STDOUT
    ) as client:
        crossed = False
        deadline = time.monotonic() + 85
        try:
            while client.poll() is None:
                if time.monotonic() >= deadline:
                    raise RuntimeError("headless probe timed out")
                if not crossed and "ready in minecraft:overworld" in log.read_text(errors="replace"):
                    server.stdin.write(b"execute in minecraft:the_nether run teleport RadioProbe 0 128 0\n")
                    server.stdin.flush()
                    crossed = True
                time.sleep(0.25)
            if client.returncode:
                raise RuntimeError(f"headless probe failed (exit {client.returncode})")
        finally:
            if client.poll() is None:
                client.kill()
            print(log.read_text(errors="replace"), end="")


def server_command(server, command):
    server.stdin.write((command + "\n").encode())
    server.stdin.flush()


def run_engine(binary, resources, address, server, server_log, root):
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
    args = [
        binary, "--headless", "--radio-1.21.11", "--resources", resources,
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

    def console():
        return server_log.read_text(errors="replace")

    server_command(server, "forceload add 0 0")
    server_command(server, "fill 0 70 0 12 70 12 minecraft:stone")
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
            server_command(server, "execute in minecraft:the_nether run teleport RadioEngine 0 128 0")
            wait_for(is_nether, "the Nether player", client)
            time.sleep(30)
            if not is_nether() or console().count("RadioEngine joined the game") != 1:
                raise RuntimeError("game lost its connection during the Nether soak")
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
    args = parser.parse_args()
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
                    run_engine(client, resources, address, server, log, root)
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
