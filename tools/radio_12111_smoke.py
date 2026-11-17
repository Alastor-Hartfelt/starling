#!/usr/bin/env python3
"""Run the Radio probe against a scratch, loopback-only Minecraft 1.21.11 server."""

import argparse
import socket
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server_jar", type=Path, help="Mojang's Minecraft 1.21.11 server jar")
    parser.add_argument("probe", type=Path, help="compiled radio_12111_probe example")
    args = parser.parse_args()
    server_jar = args.server_jar.resolve(strict=True)
    probe = args.probe.resolve(strict=True)

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]

    with tempfile.TemporaryDirectory(prefix="radio-12111-") as directory:
        root = Path(directory)
        (root / "eula.txt").write_text("eula=true\n")
        (root / "server.properties").write_text(
            "online-mode=false\n"
            "enforce-secure-profile=false\n"
            "server-ip=127.0.0.1\n"
            f"server-port={port}\n"
            "view-distance=3\n"
            "simulation-distance=3\n"
            "generate-structures=false\n"
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
                subprocess.run([probe, f"127.0.0.1:{port}"], check=True, timeout=75)
            except (RuntimeError, subprocess.SubprocessError):
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
