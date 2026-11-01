"""Bounded native stack capture for one ACKed disposable macOS CI Java child.

This is diagnostic evidence only. It neither stops Java nor changes the strict
game verdict. libproc observations fence PID reuse before/after sampling; they
are observations, not a kernel process handle or an atomic sampling admission.
"""
from __future__ import annotations

import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import selectors
import stat
import subprocess
import tempfile
import time

REPORT_LIMIT = 2 * 1024 ** 2
CONSOLE_LIMIT = 64 * 1024
TOTAL_SECONDS = 10
SAMPLE = "/usr/bin/sample"


class ProcBSDInfo(ctypes.Structure):
    # Native ABI, not a packed wire record. Source: Apple's XNU proc_info.h,
    # struct proc_bsdinfo (PROC_PIDTBSDINFO=3); uid_t/gid_t are uint32 on Darwin.
    # https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h
    _fields_ = [(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid", "rgid",
        "svuid", "svgid", "reserved")]
    _fields_ += [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)]
    _fields_ += [(name, ctypes.c_uint32) for name in ("nfiles", "pgid", "pjobc", "tdev", "tpgid")]
    _fields_ += [("nice", ctypes.c_int32), ("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64)]


def _host_guard() -> None:
    if platform.system() != "Darwin" or os.environ.get("GITHUB_ACTIONS") != "true" \
            or os.environ.get("RUNNER_OS") != "macOS" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise ValueError("Native sampling is limited to a disposable GitHub-hosted macOS runner")


def _paths(expected: Path, output: Path) -> tuple[Path, Path]:
    workspace = Path(os.environ["GITHUB_WORKSPACE"]).resolve(strict=True)
    parent = Path(os.environ["PCL_ACCEPTANCE_PARENT"]).resolve(strict=True)
    expected = expected.resolve(strict=True)
    relative = expected.relative_to(parent)
    if len(relative.parts) < 5 or not relative.parts[0].startswith(".install-") \
            or relative.parts[1:3] != (".pcl-rust", "java") or expected.name != "java" or not expected.is_file():
        raise ValueError("Expected Java is not inside the disposable acceptance runtime store")
    marker = parent / relative.parts[0] / "game-acceptance-owner.json"
    info = marker.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > 1024 \
            or json.loads(marker.read_bytes()) != {"schema_version": 1, "purpose": "disposable-game-acceptance"}:
        raise ValueError("Expected Java lacks the newly allocated acceptance owner record")
    base = workspace / "target" / "game-evidence"
    output = output if output.is_absolute() else workspace / output
    # Check literal parents before resolve: a symlink must not grant an output
    # outside the one explicitly selected CI artifact directory.
    output.relative_to(base)
    if any(part in (".", "..") or part.startswith(".") for part in output.relative_to(base).parts) \
            or not output.name or len(output.name) > 180:
        raise ValueError("Sample output must use nonhidden artifact path components")
    for directory in [base, *output.parents]:
        if directory == workspace:
            break
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError("Sample artifact parent must be an existing literal directory")
    if base.resolve(strict=True) != base or output.parent.resolve(strict=True) != output.parent:
        raise ValueError("Sample output parent is not a physical artifact directory")
    if os.path.lexists(output):
        raise ValueError("Sample output already exists; refusing to replace it")
    return expected, output


def _libproc():
    native = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    native.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    native.proc_pidinfo.restype = ctypes.c_int
    native.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    native.proc_pidpath.restype = ctypes.c_int
    return native


def _process_info(native, pid: int) -> dict:
    info = ProcBSDInfo()
    size = ctypes.sizeof(info)
    if size != 136 or native.proc_pidinfo(pid, 3, 0, ctypes.byref(info), size) != size:
        raise OSError("Cannot read the exact native BSD process identity")
    if info.pid != pid or info.status == 5 or info.flags & 4 or info.start_sec == 0 or info.start_usec >= 1_000_000:
        raise ValueError("The selected process is absent, exiting, or has an invalid native identity")
    return {"pid": pid, "ppid": info.ppid, "uid": info.uid,
            "start_sec": info.start_sec, "start_usec": info.start_usec}


def _identity(native, pid: int, monitor_pid: int, expected: Path) -> dict:
    java, monitor = _process_info(native, pid), _process_info(native, monitor_pid)
    if java["ppid"] != monitor_pid or java["uid"] != os.geteuid() or monitor["uid"] != os.geteuid():
        raise ValueError("Java is not the selected monitor's live same-user child")
    age = time.time() - java["start_sec"] - java["start_usec"] / 1_000_000
    if not -2 <= age <= 300 or (monitor["start_sec"], monitor["start_usec"]) > (java["start_sec"], java["start_usec"]):
        raise ValueError("The selected Java/monitor pair is not a fresh acceptance launch")
    path = ctypes.create_string_buffer(4096)
    length = native.proc_pidpath(pid, path, len(path))
    if not 0 < length < len(path):
        raise OSError("Cannot read the selected Java executable's native path")
    actual = Path(os.fsdecode(path.value))
    if not actual.is_absolute() or not actual.samefile(expected):
        raise ValueError("Native Java image is not the prepared executable")
    image = expected.stat()
    if _process_info(native, pid) != java or _process_info(native, monitor_pid) != monitor:
        raise ValueError("Java/monitor identity changed during admission")
    return {"java": java, "monitor": monitor, "executable": str(actual),
            "executable_device": image.st_dev, "executable_inode": image.st_ino}


def _sample(pid: int, raw: Path, deadline: float) -> dict:
    # No shell, process-name matching, -wait, TextEdit, or game signalling. The
    # two explicit numeric arguments request one second at one-ms intervals.
    console, source_bytes = bytearray(), 0
    selector = selectors.DefaultSelector()
    process = None
    timed_out, terminated, report_limit_reached = False, False, False
    try:
        process = subprocess.Popen([SAMPLE, str(pid), "1", "1", "-file", str(raw)],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        selector.register(process.stdout, selectors.EVENT_READ)
        while selector.get_map() or process.poll() is None:
            # Reserve the last second for reaping only this Popen-owned helper.
            if time.monotonic() >= deadline - 1:
                timed_out = True
                break
            for key, _ in selector.select(min(0.05, max(0, deadline - 1 - time.monotonic()))):
                data = os.read(key.fileobj.fileno(), 8192)
                if not data:
                    selector.unregister(key.fileobj)
                source_bytes += len(data)
                console.extend(data[:max(0, CONSOLE_LIMIT - len(console))])
            if raw.exists() and raw.stat().st_size > REPORT_LIMIT:
                # Bound the generated scratch report as soon as it crosses
                # the retained evidence limit; publish only a capped prefix.
                report_limit_reached = True
                break
    finally:
        selector.close()
        if process is not None:
            if process.poll() is None:
                process.kill()
                terminated = True
            try:
                process.wait(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass
            process.stdout.close()
    decoded = console.decode("utf-8", errors="replace").encode("utf-8")
    text = decoded[:CONSOLE_LIMIT].decode("utf-8", errors="ignore")
    return {"helper_exit_code": process.poll(), "helper_timed_out": timed_out,
            "helper_terminated": terminated, "helper_reaped": process.poll() is not None,
            "report_limit_reached": report_limit_reached,
            "console": text, "console_source_bytes": source_bytes,
            "console_kept_bytes": len(text.encode("utf-8")),
            "console_truncated": source_bytes > CONSOLE_LIMIT or len(decoded) > CONSOLE_LIMIT}


def _publish(raw: Path, output: Path) -> dict:
    info = raw.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError("Native sampler did not create an ordinary single-link report")
    incoming_fd = os.open(raw, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(incoming_fd, "rb") as incoming:
        before = os.fstat(incoming.fileno())
        body = incoming.read(REPORT_LIMIT + 1)
        after = os.fstat(incoming.fileno())
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) \
            != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
        raise ValueError("Native sample changed while its bounded report was read")
    kept = body[:REPORT_LIMIT]
    descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as destination:
        destination.write(kept)
        destination.flush()
        os.fsync(destination.fileno())
    return {"path": str(output), "source_bytes": before.st_size, "kept_bytes": len(kept),
            "truncated": before.st_size > REPORT_LIMIT, "sha256": hashlib.sha256(kept).hexdigest()}


def capture(pid: int, monitor_pid: int, expected_executable: Path, output: Path) -> dict:
    """Capture only the fresh ACKed CI Java child; return diagnostic status."""
    started = time.monotonic()
    result = {"schema_version": 1, "kind": "macos_owned_java_native_sample", "status": "blocked",
              "pid": pid, "monitor_pid": monitor_pid, "duration_seconds": 1,
              "total_timeout_seconds": TOTAL_SECONDS, "diagnostic_only": True,
              "affects_game_acceptance": False, "java_stop_requested": False}
    try:
        _host_guard()
        if type(pid) is not int or type(monitor_pid) is not int or not 0 < pid <= 2 ** 31 - 1 \
                or not 0 < monitor_pid <= 2 ** 31 - 1 or pid == monitor_pid:
            raise ValueError("A distinct ACKed Java PID and monitor PID are required")
        expected, output = _paths(Path(expected_executable), Path(output))
        native = _libproc()
        before = _identity(native, pid, monitor_pid, expected)
        result["identity_before"] = before
        result["status"] = "failed"
        with tempfile.TemporaryDirectory(prefix="macos-native-sample-", dir=output.parent) as temporary:
            raw = Path(temporary) / "sample.log"
            result.update(_sample(pid, raw, started + TOTAL_SECONDS))
            after = _identity(native, pid, monitor_pid, expected)
            result["identity_after"] = after
            result["identity_unchanged"] = before == after
            if before != after:
                raise ValueError("Native Java/monitor identity changed; refusing to publish the sample")
            if raw.exists():
                result["report"] = _publish(raw, output)
            if result.get("helper_exit_code") == 0 and result.get("helper_reaped") \
                    and not result.get("helper_timed_out") and result.get("report", {}).get("kept_bytes", 0) > 0:
                result["status"] = "captured"
            else:
                result["reason"] = "Native sampler failed, timed out, or did not produce a nonempty bounded report"
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        result["reason"] = str(error)[:1024]
    result["elapsed_seconds"] = time.monotonic() - started
    result["total_limit_exceeded"] = result["elapsed_seconds"] > TOTAL_SECONDS
    if result["total_limit_exceeded"]:
        result["status"] = "failed"
        result["reason"] = "Native sampling exceeded its total diagnostic deadline"
    return result
