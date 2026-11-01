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
import re
import selectors
import stat
import subprocess
import tempfile
import time

REPORT_LIMIT = 2 * 1024 ** 2
CONSOLE_LIMIT = 64 * 1024
NATIVE_SECONDS = 10
CHILD_ARGS_SECONDS = 3
THREAD_SECONDS = 8
TOTAL_SECONDS = 20
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
    # PROC_PPID_ONLY, not an all-process inventory. This returns byte count.
    native.proc_listpids.argtypes = [ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_int]
    native.proc_listpids.restype = ctypes.c_int
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
    actual = _process_path(native, pid)
    if not actual.is_absolute() or not actual.samefile(expected):
        raise ValueError("Native Java image is not the prepared executable")
    image = expected.stat()
    if _process_info(native, pid) != java or _process_info(native, monitor_pid) != monitor:
        raise ValueError("Java/monitor identity changed during admission")
    return {"java": java, "monitor": monitor, "executable": str(actual),
            "executable_device": image.st_dev, "executable_inode": image.st_ino}


def _process_path(native, pid: int) -> Path:
    path = ctypes.create_string_buffer(4096)
    length = native.proc_pidpath(pid, path, len(path))
    if not 0 < length < len(path):
        raise OSError("Cannot read the selected executable's native path")
    return Path(os.fsdecode(path.value))


def _command(args: list[str], deadline: float, stdout_limit: int, env=None) -> tuple[dict, bytes]:
    """Drain bounded pipe prefixes; terminate/reap only this helper on timeout."""
    selector, process = selectors.DefaultSelector(), None
    buffers = {"stdout": bytearray(), "stderr": bytearray()}
    counts = {"stdout": 0, "stderr": 0}
    timed_out = terminated = False
    try:
        process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, env=env)
        selector.register(process.stdout, selectors.EVENT_READ, "stdout")
        selector.register(process.stderr, selectors.EVENT_READ, "stderr")
        while selector.get_map() or process.poll() is None:
            if time.monotonic() >= deadline - 0.2:
                timed_out = True
                break
            for key, _ in selector.select(min(0.05, max(0, deadline - 0.2 - time.monotonic()))):
                data = os.read(key.fileobj.fileno(), 8192)
                if not data:
                    selector.unregister(key.fileobj)
                stream = key.data
                counts[stream] += len(data)
                cap = stdout_limit if stream == "stdout" else CONSOLE_LIMIT
                buffers[stream].extend(data[:max(0, cap - len(buffers[stream]))])
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
            process.stderr.close()
    console = bytes(buffers["stderr"]).decode("utf-8", errors="replace").encode()[:CONSOLE_LIMIT].decode("utf-8", errors="ignore")
    return {"helper_exit_code": process.poll(), "helper_timed_out": timed_out,
            "helper_terminated": terminated, "helper_reaped": process.poll() is not None,
            "stdout_source_bytes": counts["stdout"], "stdout_truncated": counts["stdout"] > stdout_limit,
            "console": console, "console_source_bytes": counts["stderr"],
            "console_truncated": counts["stderr"] > CONSOLE_LIMIT}, bytes(buffers["stdout"])


def _children(native, parent_pid: int) -> list[int]:
    pids = (ctypes.c_int * 17)()
    # Apple's wrapper maps __proc_info errors to zero; errno distinguishes a
    # failed query from a genuinely empty child inventory.
    ctypes.set_errno(0)
    size = native.proc_listpids(6, parent_pid, pids, ctypes.sizeof(pids))
    if size == 0 and ctypes.get_errno():
        raise OSError("Native owned-child query failed")
    if size < 0 or size % ctypes.sizeof(ctypes.c_int) or size >= ctypes.sizeof(pids):
        raise ValueError("Owned child inventory is unavailable or exceeds sixteen processes")
    return [pid for pid in pids[:size // ctypes.sizeof(ctypes.c_int)] if pid > 0]


def _child_arguments(native, root: dict, deadline: float) -> dict:
    result = {"status": "observed", "method": "libproc PROC_PPID_ONLY + explicit owned /bin/ps",
              "children": [], "errors": [], "text_limit_bytes": CONSOLE_LIMIT, "truncated": False}
    queue, seen, kept = [(root, 1)], set(), 0
    while queue:
        if time.monotonic() >= deadline:
            result["status"] = "partial"
            result["errors"].append("Owned child inspection deadline reached")
            break
        parent, depth = queue.pop(0)
        if _process_info(native, parent["pid"]) != parent:
            raise ValueError("Owned child parent identity changed")
        for pid in _children(native, parent["pid"]):
            if pid in seen:
                continue
            if len(seen) >= 16:
                result["status"] = "partial"
                result["errors"].append("Owned child inspection count reached sixteen")
                return result
            seen.add(pid)
            try:
                before = _process_info(native, pid)
                if before["ppid"] != parent["pid"] or before["uid"] != root["uid"] \
                        or (before["start_sec"], before["start_usec"]) < (parent["start_sec"], parent["start_usec"]):
                    raise ValueError("Selected descendant is not the same native owned child")
                if depth < 3:
                    queue.append((before, depth + 1))
                image = _process_path(native, pid)
                allowed = any(image.samefile(name) for name in ("/bin/sh", "/bin/bash", "/usr/bin/osascript"))
                if not allowed or kept >= CONSOLE_LIMIT:
                    continue
                info, body = _command(["/bin/ps", "-ww", "-p", str(pid), "-o", "command="],
                                      min(deadline, time.monotonic() + 1), CONSOLE_LIMIT - kept)
                after = _process_info(native, pid)
                if before != after or _process_info(native, parent["pid"]) != parent \
                        or not _process_path(native, pid).samefile(image):
                    raise ValueError("Selected descendant changed during argv observation")
                if info["helper_exit_code"] != 0 or info["helper_timed_out"]:
                    raise ValueError("Owned argv helper failed or timed out")
                text = body.decode("utf-8", errors="replace").encode()[:CONSOLE_LIMIT - kept].decode("utf-8", errors="ignore")
                kept += len(text.encode())
                result["truncated"] |= info["stdout_truncated"]
                result["children"].append({"identity_before": before, "identity_after": after,
                    "executable": str(image), "command": text, "truncated": info["stdout_truncated"]})
            except (OSError, ValueError) as error:
                result["errors"].append(str(error)[:256])
    result["text_kept_bytes"] = kept
    return result


def _jdk_tool(home: Path, trusted_roots: list[Path]) -> Path | None:
    try:
        home = home.resolve(strict=True)
        if not any(home.is_relative_to(root) for root in trusted_roots):
            return None
        release = home / "release"
        stamp = release.lstat()
        if not stat.S_ISREG(stamp.st_mode) or stamp.st_size > CONSOLE_LIMIT:
            return None
        with release.open("rb") as source:
            version = source.read(CONSOLE_LIMIT + 1)
        if len(version) > CONSOLE_LIMIT or not re.search(rb'^JAVA_VERSION="21(?:[.+-][^"\n]*)?"\s*$', version, re.MULTILINE):
            return None
        tool = (home / "bin/jcmd").resolve(strict=True)
        info = tool.stat()
        if not tool.is_relative_to(home) or not stat.S_ISREG(info.st_mode) or not info.st_mode & 0o111 \
                or info.st_mode & 0o022 or info.st_uid not in (0, os.geteuid()):
            return None
        return tool
    except (OSError, ValueError, UnicodeError):
        return None


def _select_jcmd(expected: Path) -> dict | None:
    home = expected.parent.parent
    tool = _jdk_tool(home, [home])
    if tool:
        return {"path": str(tool), "source": "verified_runtime_adjacent", "java_major": 21}
    roots = [Path("/Library/Java/JavaVirtualMachines").resolve()]
    if os.environ.get("RUNNER_TOOL_CACHE") and Path(os.environ["RUNNER_TOOL_CACHE"]).is_absolute():
        roots.append(Path(os.environ["RUNNER_TOOL_CACHE"]).resolve())
    for key in sorted(key for key in os.environ if key.startswith("JAVA_HOME_21_")):
        tool = _jdk_tool(Path(os.environ[key]), roots)
        if tool:
            return {"path": str(tool), "source": key, "java_major": 21}
    base = roots[0]
    if base.is_dir():
        candidates = sorted(base.iterdir())
        if len(candidates) > 64:
            return None
        for candidate in candidates:
            tool = _jdk_tool(candidate / "Contents/Home", roots)
            if tool:
                return {"path": str(tool), "source": "system_managed_jdk21", "java_major": 21}
    return None


def _java_threads(native, pid: int, monitor_pid: int, expected: Path, output: Path,
                  baseline: dict, deadline: float) -> dict:
    result = {"status": "not_available", "timeout_seconds": THREAD_SECONDS, "java_signal_requested": False}
    try:
        selected = _select_jcmd(expected)
        if not selected:
            result["reason"] = "No trusted preinstalled JDK 21 jcmd is available"
            return result
        result.update({"status": "failed", "tool": selected})
        if os.path.lexists(output):
            raise ValueError("Java thread output already exists; refusing to replace it")
        if _identity(native, pid, monitor_pid, expected) != baseline:
            raise ValueError("Java identity changed before thread dump admission")
        if time.monotonic() + 0.3 >= deadline:
            raise ValueError("Combined diagnostic budget has no time left for jcmd")
        environment = os.environ.copy()
        for key in ("JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH"):
            environment.pop(key, None)
        info, body = _command([selected["path"], str(pid), "Thread.print", "-l"], deadline, REPORT_LIMIT, environment)
        result.update(info)
        result["identity_after"] = _identity(native, pid, monitor_pid, expected)
        result["identity_unchanged"] = result["identity_after"] == baseline
        if not result["identity_unchanged"]:
            raise ValueError("Java identity changed; refusing to publish the thread dump")
        if body:
            descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            with os.fdopen(descriptor, "wb") as destination:
                destination.write(body)
            result["report"] = {"path": str(output), "source_bytes": info["stdout_source_bytes"],
                "kept_bytes": len(body), "truncated": info["stdout_truncated"], "sha256": hashlib.sha256(body).hexdigest()}
        if info["helper_exit_code"] == 0 and info["helper_reaped"] and not info["helper_timed_out"] \
                and body.startswith(f"{pid}:\n".encode()) and b"Full thread dump" in body[:4096]:
            result["status"] = "captured"
        else:
            result["reason"] = "jcmd did not return a successful bounded Java thread dump"
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        result["status"] = "failed"
        result["reason"] = str(error)[:1024]
    return result


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
    """Capture only the fresh ACKed CI Java child; native/argv/Java diagnostics.

    The top-level status describes the native sample. Auxiliary command and
    Java-thread stages have their own statuses and remain separate evidence.
    """
    started = time.monotonic()
    result = {"schema_version": 1, "kind": "macos_owned_java_native_sample", "status": "blocked",
              "pid": pid, "monitor_pid": monitor_pid, "duration_seconds": 1,
              "total_timeout_seconds": TOTAL_SECONDS, "native_sample_timeout_seconds": NATIVE_SECONDS,
              "owned_argv_timeout_seconds": CHILD_ARGS_SECONDS, "java_threads_timeout_seconds": THREAD_SECONDS,
              "diagnostic_only": True,
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
            result.update(_sample(pid, raw, started + NATIVE_SECONDS))
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
        result["native_sample_elapsed_seconds"] = time.monotonic() - started
        if result["native_sample_elapsed_seconds"] > NATIVE_SECONDS:
            result["status"] = "failed"
            result["reason"] = "Native sample exceeded its separate ten-second deadline"
        combined_deadline = started + TOTAL_SECONDS
        try:
            result["child_arguments"] = _child_arguments(native, before["java"],
                min(combined_deadline, time.monotonic() + CHILD_ARGS_SECONDS))
            if _identity(native, pid, monitor_pid, expected) != before:
                raise ValueError("Java/monitor identity changed during owned argv inspection")
            result["child_arguments"]["java_identity_unchanged_after"] = True
        except (OSError, ValueError) as error:
            result["child_arguments"] = {"status": "failed", "reason": str(error)[:1024]}
        result["java_threads"] = _java_threads(native, pid, monitor_pid, expected,
            output.with_suffix(".threads.log"), before, min(combined_deadline, time.monotonic() + THREAD_SECONDS))
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        result["reason"] = str(error)[:1024]
    result["elapsed_seconds"] = time.monotonic() - started
    result["total_limit_exceeded"] = result["elapsed_seconds"] > TOTAL_SECONDS
    if result["total_limit_exceeded"]:
        result["status"] = "failed"
        result["reason"] = "Native sampling exceeded its total diagnostic deadline"
    return result
