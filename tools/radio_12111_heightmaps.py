"""Compare Rust terrain facts against the official 1.21.11 server in a private tempdir."""
import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
from zipfile import ZipFile

import lab
import version_port

ROOT = Path(__file__).resolve().parents[1]


def main():
    with TemporaryDirectory(prefix="radio-heightmaps-") as tmp:
        work = Path(tmp)
        version = version_port.UNOBFUSCATED_12111["id"]
        _, manifest = version_port.download_version_manifest(version, work)
        artifact = manifest["downloads"]["server"]
        jar = lab.download(artifact["url"], work / "server.jar", artifact["sha1"])
        game, libraries = lab.unpack_server(jar, work, version)
        classpath = os.pathsep.join(map(str, [game, *libraries]))
        package = "dev.horizonlab.parity.VanillaHeightmapOracle"
        source = ROOT / "mods/horizon/src/test/java" / (package.replace(".", "/") + ".java")
        classes = work / "classes"
        classes.mkdir()
        subprocess.run(["javac", "-cp", classpath, "-d", classes, source], check=True, cwd=work)
        subprocess.run(["java", "-Xmx640m", "-cp", str(classes) + os.pathsep + classpath,
                        package, "export", work / "facts.json"], check=True, cwd=work)
        prefix = "data/minecraft/tags/block/"
        with ZipFile(game) as archive:
            data_version = json.loads(archive.read("version.json"))["world_version"]
            tags = {"minecraft:" + name[len(prefix):-5]: archive.read(name).decode()
                    for name in archive.namelist() if name.startswith(prefix) and name.endswith(".json")}
        (work / "block-tags.json").write_text(json.dumps(tags))
        (work / "horizon-resources.json").write_text(json.dumps({"data_version": data_version}))
        subprocess.run(["cargo", "test", "--locked", "-p", "horizon-client-render",
                        "--no-default-features", "--lib", "official_12111_heightmaps_and_flow_match_every_state",
                        "--", "--ignored"], cwd=ROOT, check=True,
                       env={**os.environ, "RADIO_12111_RESOURCES": str(work)})


if __name__ == "__main__":
    main()
