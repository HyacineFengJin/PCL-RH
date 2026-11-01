#!/usr/bin/env python3
"""Inspect a game-acceptance host, or explicitly drive a disposable real client.

The preflight result is deliberately separate from game evidence. A desktop
session, an OpenGL query, a successful launch plan and a live Java PID establish
different facts; none of them alone establishes a rendered Minecraft menu.
The default preflight downloads nothing and starts no game. --probe-opengl may
execute glxinfo; --desktop explicitly enables the production acceptance role.
"""
from __future__ import annotations

import argparse
import ctypes
import datetime as dt
import json
import os
from pathlib import Path
import platform
import queue
import re
import selectors
import shutil
import subprocess
import sys
import threading
import time

GIB = 1024 ** 3
MIN_DISK = 4 * GIB
MIN_MEMORY = 3 * GIB
MAX_PROBE_OUTPUT = 64 * 1024
SOFTWARE_RENDERERS = ("llvmpipe", "softpipe", "software rasterizer", "gdi generic", "swrast")
STAGES = (
    "official_install", "official_java_download", "launch_readiness", "launch_plan",
    "world_prepare", "supervised_spawn", "game_initialization", "visible_render", "singleplayer_join", "owned_shutdown",
    "microsoft_login", "authenticated_launch",
)


def stage(status: str, reason: str, **details: object) -> dict:
    return {"status": status, "reason": reason, **details}


def memory_available(system: str) -> int | None:
    if system == "Linux":
        try:
            values = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
            return int(values["MemAvailable"].split()[0]) * 1024
        except (OSError, ValueError, KeyError):
            return None
    if system == "Windows":
        class MemoryStatus(ctypes.Structure):
            _fields_ = [("length", ctypes.c_uint32), ("load", ctypes.c_uint32),
                        *[(key, ctypes.c_uint64) for key in (
                            "total_physical", "available_physical", "total_page", "available_page",
                            "total_virtual", "available_virtual", "available_extended")]]
        value = MemoryStatus()
        value.length = ctypes.sizeof(value)
        if ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(value)):
            return value.available_physical
    # Darwin's free-page count excludes reclaimable memory. Do not present that
    # unrelated value as an available-memory budget or start another process.
    return None


def linux_session() -> dict:
    display = bool(os.environ.get("DISPLAY"))
    runtime, wayland = os.environ.get("XDG_RUNTIME_DIR"), os.environ.get("WAYLAND_DISPLAY")
    wayland_socket = False
    if runtime and wayland:
        try:
            address = Path(wayland) if Path(wayland).is_absolute() else Path(runtime) / wayland
            wayland_socket = address.is_socket()
        except OSError:
            pass
    return stage("available" if display or wayland_socket else "blocked",
                 "Session hints require an actual game-window observation.",
                 x11_display_set=display, wayland_socket_present=wayland_socket,
                 session_type=os.environ.get("XDG_SESSION_TYPE")
                 if os.environ.get("XDG_SESSION_TYPE") in ("x11", "wayland", "tty") else None)


def windows_session() -> dict:
    # Query only this process's window station and the input desktop. Do not
    # enumerate user windows, take screenshots, change focus or create a window.
    from ctypes import wintypes
    user = ctypes.WinDLL("user32", use_last_error=True)
    user.GetProcessWindowStation.restype = wintypes.HANDLE
    user.GetUserObjectInformationW.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                              wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    user.OpenInputDesktop.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    user.OpenInputDesktop.restype = wintypes.HANDLE
    user.CloseDesktop.argtypes = [wintypes.HANDLE]
    class UserFlags(ctypes.Structure):
        _fields_ = [("inherit", wintypes.BOOL), ("reserved", wintypes.BOOL), ("flags", wintypes.DWORD)]
    flags, length = UserFlags(), wintypes.DWORD()
    queried = bool(user.GetUserObjectInformationW(user.GetProcessWindowStation(), 1,
                                                 ctypes.byref(flags), ctypes.sizeof(flags),
                                                 ctypes.byref(length)))
    desktop = user.OpenInputDesktop(0, False, 0x0001)  # DESKTOP_READOBJECTS
    accessible = bool(desktop)
    if desktop:
        user.CloseDesktop(desktop)
    visible = queried and bool(flags.flags & 1)  # WSF_VISIBLE
    return stage("available" if visible and accessible else "blocked",
                 "A visible input desktop does not establish an OpenGL context.",
                 window_station_visible=visible, input_desktop_readable=accessible,
                 remote_session=bool(user.GetSystemMetrics(0x1000)))


def macos_session() -> dict:
    try:
        graphics = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
        graphics.CGMainDisplayID.restype = ctypes.c_uint32
        graphics.CGDisplayIsActive.argtypes = [ctypes.c_uint32]
        graphics.CGDisplayIsActive.restype = ctypes.c_uint32
        display = graphics.CGMainDisplayID()
        active = bool(display and graphics.CGDisplayIsActive(display))
        return stage("available" if active else "blocked",
                     "An active CoreGraphics display does not establish an OpenGL context.",
                     active_main_display=active)
    except (OSError, AttributeError):
        return stage("blocked", "CoreGraphics display observation is unavailable.")


def session_observation(system: str) -> dict:
    try:
        return {"Linux": linux_session, "Windows": windows_session, "Darwin": macos_session}[system]()
    except (KeyError, OSError, AttributeError):
        return stage("blocked", "No native desktop observation is implemented for this host.")


def parse_glxinfo(output: str, returncode: int) -> dict:
    values = {}
    for label in ("OpenGL vendor string", "OpenGL renderer string", "OpenGL version string",
                  "OpenGL core profile version string"):
        match = re.search(r"^" + re.escape(label) + r":\s*(.+)$", output, re.MULTILINE)
        if match:
            values[label] = match.group(1)[:512]
    version = values.get("OpenGL core profile version string", values.get("OpenGL version string", ""))
    number = re.match(r"(\d+)\.(\d+)", version)
    supported = bool(number and tuple(map(int, number.groups())) >= (3, 2))
    renderer = values.get("OpenGL renderer string", "")
    software = any(name in renderer.lower() for name in SOFTWARE_RENDERERS)
    ok = returncode == 0 and bool(renderer) and supported
    return stage("available" if ok else "blocked",
                 "A real GLX context answered; Minecraft rendering remains unobserved." if ok
                 else "glxinfo did not establish an OpenGL 3.2 context.",
                 method="glxinfo -B", returncode=returncode, values=values,
                 software_renderer=software, opengl_3_2=supported)


def opengl_observation(system: str, requested: bool) -> dict:
    if not requested:
        return stage("not_probed", "Use --probe-opengl to request the available native context probe.")
    if system != "Linux":
        return stage("blocked", "This host still requires a native OpenGL context probe; display presence is insufficient.")
    executable = shutil.which("glxinfo")
    if not executable:
        return stage("blocked", "glxinfo is unavailable; install the host's OpenGL query tool or use native game evidence.")
    if not os.environ.get("DISPLAY"):
        return stage("blocked", "GLX probing requires DISPLAY; Wayland-only presence does not establish GLX support.")
    # A broken probe cannot retain unbounded output or extend its deadline.
    process = subprocess.Popen([executable, "-B"], stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    output = bytearray()
    until = time.monotonic() + 15
    try:
        while selector.get_map():
            remaining = until - time.monotonic()
            if remaining <= 0:
                return stage("blocked", "OpenGL query exceeded its 15-second deadline.")
            for key, _ in selector.select(min(remaining, 0.2)):
                chunk = os.read(key.fileobj.fileno(), MAX_PROBE_OUTPUT + 1 - len(output))
                if not chunk:
                    selector.unregister(key.fileobj)
                output.extend(chunk)
                if len(output) > MAX_PROBE_OUTPUT:
                    return stage("blocked", "OpenGL probe output exceeded its evidence limit.")
        process.wait(timeout=max(0.1, until - time.monotonic()))
        return parse_glxinfo(output.decode("utf-8", errors="replace"), process.returncode)
    except subprocess.TimeoutExpired:
        return stage("blocked", "OpenGL query exceeded its 15-second deadline.")
    finally:
        selector.close()
        if process.poll() is None:
            process.kill()  # only this Popen-owned, read-only probe
        process.wait()
        process.stdout.close()


def render_assessment(client: dict, observations: dict) -> dict:
    stages = client.get("stages", {})
    log = stages.get("game_initialization", {}).get("evidence") or {}
    initialized = "Created:" in log.get("initialization_markers", [])
    backend = "Backend library:" in log.get("initialization_markers", [])
    errors = log.get("renderer_errors", [])
    window = observations.get("sustained_visible_seconds", 0) >= 2
    cleanup = stages.get("owned_shutdown", {}).get("status") == "pass"
    world_join = stages.get("singleplayer_join", {}).get("status") == "pass"
    success = client.get("attempt_status") == "complete" and initialized and backend and not errors and window and cleanup and world_join
    return {
        "status": "pass" if success else "blocked",
        "claim": "Actual client window, render-resource initialization and disposable singleplayer join observed" if success
                 else "Client render initialization and disposable singleplayer join were not established",
        "window_sustained": window, "render_resource_created_marker": initialized,
        "lwjgl_backend_marker": backend, "renderer_errors": errors, "owned_shutdown": cleanup,
        "singleplayer_join": world_join,
        "interactive_gameplay": "not_run", "microsoft_login": "not_run", "authenticated_launch": "not_run",
    }


def validate_build(build: dict, expected_commit: str) -> None:
    if not isinstance(build, dict) or build.get("commit") != expected_commit \
            or build.get("product_id") != "dev.pcl-rh.desktop" or build.get("schema_version") != 2 \
            or build.get("runtime_layout") != "split-v1":
        raise ValueError("Desktop binary does not match the explicitly selected acceptance source and runtime protocol")


def run_client(desktop: Path, parent: Path, expected_commit: str, fabric: str | None,
               duration: int, prepare_only: bool, macos_sample: Path | None = None) -> dict:
    """Drive the real pre-GUI role, observing only its ACKed Java PID's windows."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("acceptance_native_window", Path(__file__).with_name("game-acceptance") / "native_window.py")
    native = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(native)
    desktop = desktop.resolve(strict=True)
    if not re.fullmatch(r"[0-9a-f]{40}", expected_commit):
        raise ValueError("--expected-commit must be the exact trusted test binary's 40-character source commit")
    identity = subprocess.run([str(desktop), "--build-info"], stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20, check=True)
    if len(identity.stdout) > 16 * 1024:
        raise ValueError("Desktop build identity exceeded 16 KiB")
    build = json.loads(identity.stdout)
    validate_build(build, expected_commit)
    request = {"schema_version": 1, "parent": str(parent.resolve(strict=True)), "minecraft": "1.21.4",
               "fabric": fabric, "mode": "prepare" if prepare_only else "run", "duration_seconds": duration}
    environment = os.environ.copy()
    for key in ("JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH"):
        environment.pop(key, None)
    process = subprocess.Popen([str(desktop), "--game-acceptance"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=sys.stderr, text=True,
                               encoding="utf-8", env=environment, bufsize=1)
    process.stdin.write(json.dumps(request))
    process.stdin.close()
    messages = queue.Queue(maxsize=64)
    def read_events():
        try:
            while True:
                line = process.stdout.readline(1024 * 1024 + 1)
                if not line:
                    break
                if len(line) > 1024 * 1024 or not line.endswith("\n"):
                    raise ValueError("Acceptance event exceeded its bounded line protocol")
                messages.put(json.loads(line), timeout=5)
        except (ValueError, OSError, queue.Full) as error:
            messages.put({"event": "protocol_error", "data": str(error)[:512]}, timeout=5)
        finally:
            messages.put(None, timeout=5)
    reader = threading.Thread(target=read_events, daemon=True)
    reader.start()
    pid, monitor_pid, prepared_java, spawned_at = None, None, None, None
    result, observations, errors, native_sample = None, [], [], None
    first_visible, longest, last_window = None, 0.0, None
    started = time.monotonic()
    # The Rust role owns the 900-second download cancellation and the at-most
    # 120-second game budget. Never kill the supervisor or a detached game by a
    # guessed PID when an observation deadline expires.
    eof = False
    while not eof or process.poll() is None:
        try:
            event = messages.get(timeout=0.25)
            if event is None:
                eof = True
            elif event.get("event") == "prepared":
                java = event["data"].get("java")
                if isinstance(java, str):
                    prepared_java = Path(java)
            elif event.get("event") == "spawned":
                raw_pid = event["data"].get("pid")
                if type(raw_pid) is not int or not 0 < raw_pid < 2 ** 32 or pid is not None:
                    errors.append("Invalid or duplicate spawned-PID event")
                else:
                    pid = raw_pid
                    monitor_pid = event["data"].get("monitor_pid")
                    spawned_at = time.monotonic()
            elif event.get("event") == "result":
                result = event["data"]
            elif event.get("event") in ("request_error", "protocol_error"):
                errors.append(event.get("data"))
        except queue.Empty:
            pass
        if pid is not None and result is None and process.poll() is None:
            observation = native.observe(pid)
            own = [item for item in observation["windows"] if item.get("owner_pid") == pid]
            visible = [item for item in own if item.get("visible") and item.get("width", 0) >= 640
                       and item.get("height", 0) >= 360]
            now = time.monotonic()
            if visible:
                signature = (observation["method"], visible[0]["id"])
                if first_visible is None or signature != last_window:
                    first_visible, last_window = now, signature
                longest = max(longest, now - first_visible)
            else:
                first_visible, last_window = None, None
            if not observations or observation != observations[-1]:
                if len(observations) < 64:
                    observations.append(observation)
            if macos_sample is not None and native_sample is None and spawned_at is not None \
                    and now - spawned_at >= 12 and longest < 2:
                # The optional CI diagnostic independently validates the exact
                # Java image, parent monitor and process birth identity. It
                # samples only that acknowledged child and never supplies game
                # success evidence or changes the rendered/world verdict.
                try:
                    if prepared_java is None:
                        raise ValueError("No prepared Java image was acknowledged")
                    sample_spec = importlib.util.spec_from_file_location("acceptance_macos_sample",
                        Path(__file__).with_name("game-acceptance") / "macos_sample.py")
                    sample = importlib.util.module_from_spec(sample_spec)
                    sample_spec.loader.exec_module(sample)
                    native_sample = sample.capture(pid, monitor_pid, prepared_java, macos_sample)
                except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                    native_sample = {"status": "diagnostic_failed", "reason": str(error)[:512]}
        if time.monotonic() - started > 1200:
            # Preserve the returned workspace and any exact session evidence.
            # Killing the role here could orphan its detached Java process.
            raise RuntimeError("Acceptance role exceeded 1200 seconds; do not treat this as successful cleanup")
    returncode = process.wait(timeout=20)
    process.stdout.close()
    reader.join(timeout=5)
    windows = {"game_pid": pid, "sustained_visible_seconds": longest, "observations": observations,
               "screenshots_taken": False, "focus_changed": False}
    client = result or {"attempt_status": "fail", "stages": {}, "reason": "The selected binary did not return acceptance evidence"}
    return {"build": build, "client": client, "native_window": windows, "protocol_errors": errors,
            "native_sample": native_sample,
            "role_exit_code": returncode, "render_assessment": render_assessment(client, windows)}


def preflight(parent: Path, probe_opengl: bool = False) -> dict:
    parent = parent.resolve(strict=True)
    if not parent.is_dir():
        raise ValueError("--parent must be an existing directory")
    system = platform.system()
    free_disk = shutil.disk_usage(parent).free
    available_memory = memory_available(system)
    session = session_observation(system)
    opengl = opengl_observation(system, probe_opengl)
    blockers = []
    if free_disk < MIN_DISK:
        blockers.append("Less than 4 GiB free disk space in the private-test parent.")
    if available_memory is not None and available_memory < MIN_MEMORY:
        blockers.append("Less than 3 GiB available physical memory for a 2 GiB game heap.")
    if session["status"] != "available":
        blockers.append("A native desktop session has not been established.")
    if opengl["status"] != "available":
        blockers.append("A native OpenGL 3.2 context has not been established.")
    return {
        "schema_version": 1,
        "kind": "game_acceptance_preflight",
        "observed_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "host": {"system": system, "machine": platform.machine(), "python": platform.python_version(),
                 "github_actions": os.environ.get("GITHUB_ACTIONS") == "true"},
        "resources": {"disk_free_bytes": free_disk, "memory_available_bytes": available_memory,
                      "minimum_disk_bytes": MIN_DISK, "minimum_memory_bytes": MIN_MEMORY,
                      "game_heap_gib": 2},
        "desktop": session,
        "opengl": opengl,
        "tools": {name: bool(shutil.which(name)) for name in ("glxinfo", "java", "python3")},
        "run_readiness": "blocked" if blockers else "candidate",
        "blockers": blockers,
        "stages": {name: stage("not_run", "Preflight does not execute this stage.") for name in STAGES},
        "game_acceptance": "not_run",
        "limitations": [
            "Preflight does not download files, run Java, access accounts or inspect user game roots.",
            "Software OpenGL, if observed, is recorded separately and does not prove hardware GPU support.",
            "Rendered menu and interaction require an observation of the actual owned game window.",
            "Microsoft login and authenticated launch require the user's explicit interactive authorization.",
        ],
    }


def write_report(report: dict, destination: Path | None) -> None:
    payload = json.dumps(report, ensure_ascii=True, indent=2) + "\n"
    if destination is None:
        print(payload, end="")
        return
    # Some Xvfb wrappers merge a command's stderr into stdout. Keep machine
    # evidence on an explicit, newly created file rather than parsing that
    # console stream, which legitimately also contains installer progress.
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as outgoing:
        outgoing.write(payload)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--parent", type=Path, required=True,
                        help="Existing parent for future private test roots; inspected only")
    parser.add_argument("--probe-opengl", action="store_true",
                        help="Run a bounded native GLX query on Linux (does not launch a game)")
    parser.add_argument("--desktop", type=Path, help="Explicitly selected trusted desktop binary containing the acceptance role")
    parser.add_argument("--expected-commit", help="Exact source commit of a binary known to include the acceptance role")
    parser.add_argument("--attempt-client", action="store_true", help="Attempt the real disposable client even when desktop/OpenGL preflight is blocked")
    parser.add_argument("--fabric", help="Exact official Fabric loader version; omitted selects vanilla")
    parser.add_argument("--duration", type=int, choices=range(30, 121), metavar="30..120", default=120)
    parser.add_argument("--prepare-only", action="store_true", help="Download and prepare through production engines without starting Java Minecraft")
    parser.add_argument("--report", type=Path, help="Write JSON evidence to a new file, independently of console output")
    parser.add_argument("--macos-sample", type=Path, help="On a disposable Mac CI host, sample the owned Java process if its window is not observed")
    args = parser.parse_args()
    try:
        result = preflight(args.parent, args.probe_opengl)
        if args.desktop:
            result["kind"] = "game_acceptance_report"
            result["preflight_stages"] = result.pop("stages")
            if not args.expected_commit:
                raise ValueError("--desktop requires --expected-commit for a trusted binary known to contain this role")
            resources = result["resources"]
            enough = resources["disk_free_bytes"] >= MIN_DISK and (resources["memory_available_bytes"] is None
                     or resources["memory_available_bytes"] >= MIN_MEMORY)
            if not enough:
                result["client_attempt"] = stage("blocked", "The current resource budget does not permit a real game attempt.")
            elif args.prepare_only or result["run_readiness"] == "candidate" or args.attempt_client:
                result["client_attempt"] = run_client(args.desktop, args.parent, args.expected_commit, args.fabric,
                                                      args.duration, args.prepare_only, args.macos_sample)
            else:
                result["client_attempt"] = stage("blocked", "Use --attempt-client for a bounded real attempt on an unverified graphics host.")
            attempt = result["client_attempt"]
            result["stages"] = attempt.get("client", {}).get("stages", result["preflight_stages"])
            result["game_acceptance"] = "singleplayer_join_observed" \
                if attempt.get("render_assessment", {}).get("status") == "pass" else "incomplete"
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Acceptance failed: {error}\n")
    write_report(result, args.report)
    # An observed limitation is a real result, not a fake success or a fatal
    # preflight crash. Consumers must check run_readiness before starting Java.
    if args.desktop:
        attempt = result.get("client_attempt", {})
        if args.prepare_only:
            return 0 if attempt.get("client", {}).get("attempt_status") == "complete" else 2
        return 0 if attempt.get("role_exit_code") == 0 and attempt.get("render_assessment", {}).get("status") == "pass" else 2
    return 0 if result["run_readiness"] == "candidate" else 2


if __name__ == "__main__":
    sys.exit(main())
