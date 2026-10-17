#!/usr/bin/env python3
"""Execute only synthetic API fixtures; never load Minecraft or real LWJGL."""
import argparse
import struct
from pathlib import Path
import subprocess
import tempfile
import zipfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
AGENT = ROOT / "crates/core/assets/game-title-agent.jar"
TITLE = '测试游戏 🦀 "literal" \\ $(no shell)'


def unknown_code_attribute(content: bytes) -> bytes:
    """Rename a known debug attribute to exercise safe rejection, not deletion."""
    count = int.from_bytes(content[8:10], "big")
    result = bytearray(content[:10])
    cursor = 10
    index = 1
    while index < count:
        tag = content[cursor]
        result.append(tag)
        cursor += 1
        if tag == 1:
            length = int.from_bytes(content[cursor:cursor + 2], "big")
            value = content[cursor + 2:cursor + 2 + length]
            cursor += 2 + length
            if value == b"LineNumberTable":
                value = b"UnknownCodeAttribute"
            result.extend(struct.pack(">H", len(value)))
            result.extend(value)
        else:
            lengths = {3: 4, 4: 4, 5: 8, 6: 8, 7: 2, 8: 2, 9: 4, 10: 4,
                       11: 4, 12: 4, 15: 3, 16: 2, 17: 4, 18: 4, 19: 2, 20: 2}
            length = lengths[tag]
            result.extend(content[cursor:cursor + length])
            cursor += length
            if tag in [5, 6]:
                index += 1
        index += 1
    result.extend(content[cursor:])
    return bytes(result)


def run(java: str, javac: str) -> None:
    work = ROOT / "work/game-title-agent"
    work.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(AGENT) as archive:
        for name in archive.namelist():
            if name.endswith(".class"):
                content = archive.read(name)
                assert content[:4] == bytes.fromhex("cafebabe")
                assert int.from_bytes(content[6:8], "big") == 52, name
    for release in [8, 17]:
        with tempfile.TemporaryDirectory(dir=work) as temporary:
            classes = Path(temporary) / "classes"
            subprocess.run([javac, "--release", str(release), "-Xlint:-options",
                            "-g", "-proc:none", "-encoding", "UTF-8", "-d", str(classes),
                            *map(str, sorted((HERE / "tests").rglob("*.java")))], check=True)
            encoded = TITLE.encode("utf-8").hex()
            completed = subprocess.run([java, "-Xverify:all", f"-javaagent:{AGENT}={encoded}",
                                       "-cp", str(classes), "FixtureMain", TITLE, str(classes)],
                                      check=True, text=True, capture_output=True, timeout=20)
            assert "Custom game window title enabled." in completed.stderr, completed.stderr
            assert "could not" not in completed.stderr, completed.stderr
            print(f"{java}: Java {release} bytecode, modern/legacy APIs, branches, exceptions, "
                  "Unicode, rendering-thread ownership and isolated class loaders passed.")
            for invalid in ["not-hex", "0a", "ff", "61" * 257]:
                completed = subprocess.run([
                    java, "-Xverify:all", f"-javaagent:{AGENT}={invalid}",
                    "-cp", str(classes), "FallbackMain"],
                    check=True, text=True, capture_output=True, timeout=20)
                assert "could not initialize" in completed.stderr, completed.stderr
            display = classes / "org/lwjgl/opengl/Display.class"
            display.write_bytes(unknown_code_attribute(display.read_bytes()))
            completed = subprocess.run([
                java, "-Xverify:all", f"-javaagent:{AGENT}={encoded}",
                "-cp", str(classes), "FallbackMain"],
                check=True, text=True, capture_output=True, timeout=20)
            assert "Unsupported graphics class" in completed.stderr, completed.stderr
            print(f"{java}: malformed options and unknown code attributes retain original behavior.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--java", default="java")
    parser.add_argument("--javac", default="javac")
    arguments = parser.parse_args()
    run(arguments.java, arguments.javac)
