#!/usr/bin/env python3
"""Build the embedded agent without Maven, Gradle, network, or game files.

The checked-in Java 8 bytecode keeps launcher builds and end-user launches free
of a JDK dependency. Regenerate when changing the Java sources; ZIP ordering,
timestamps and permissions are fixed. --check compares a local regeneration.
"""

import argparse
from pathlib import Path
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
OUTPUT = ROOT / "crates/core/assets/game-title-agent.jar"
MANIFEST = b"Manifest-Version: 1.0\r\nPremain-Class: org.pclrh.title.TitleAgent\r\n\r\n"


def build(destination: Path) -> None:
    work = ROOT / "work/game-title-agent"
    work.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=work) as temporary:
        classes = Path(temporary) / "classes"
        subprocess.run([
            "javac", "--release", "8", "-Xlint:-options", "-g:none",
            "-proc:none", "-encoding", "UTF-8", "-d", str(classes),
            *map(str, sorted((HERE / "src").rglob("*.java"))),
        ], check=True)
        destination.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_STORED) as jar:
            entries = {"META-INF/MANIFEST.MF": MANIFEST}
            entries.update({path.relative_to(classes).as_posix(): path.read_bytes()
                            for path in classes.rglob("*.class")})
            for name, content in sorted(entries.items()):
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = 0o100644 << 16
                jar.writestr(info, content)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    if arguments.check:
        work = ROOT / "work/game-title-agent"
        work.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=work) as temporary:
            candidate = Path(temporary) / "agent.jar"
            build(candidate)
            if candidate.read_bytes() != OUTPUT.read_bytes():
                raise SystemExit("Embedded game title agent differs; regenerate and review it.")
        print("Embedded game title agent matches Java sources.")
    else:
        build(OUTPUT)
        print(f"Generated {OUTPUT.relative_to(ROOT)} ({OUTPUT.stat().st_size} bytes)")
