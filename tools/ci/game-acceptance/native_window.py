"""Read window metadata for one caller-owned game PID. No images or focus IO."""
from __future__ import annotations

import ctypes as c
import ctypes.util
from functools import lru_cache
import platform


@lru_cache(maxsize=1)
def x11_library():
    name = ctypes.util.find_library("X11")
    if not name:
        raise OSError("libX11 is unavailable; Wayland window metadata requires a compositor-specific observer")
    return c.CDLL(name)


def observe(pid: int) -> dict:
    if type(pid) is not int or not 0 < pid < 2 ** 32:
        raise ValueError("Expected an actual spawned game PID")
    try:
        method = {"Linux": x11, "Windows": windows, "Darwin": macos}.get(platform.system())
        if method is None:
            return {"method": "unavailable", "windows": [], "error": "Unsupported host"}
        return method(pid)
    except (OSError, AttributeError, ValueError) as error:
        return {"method": "unavailable", "windows": [], "error": str(error)[:512]}


def windows(pid: int) -> dict:
    from ctypes import wintypes as w
    user = c.WinDLL("user32", use_last_error=True)
    callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    user.EnumWindows.argtypes = [callback_type, w.LPARAM]
    user.GetWindowThreadProcessId.argtypes = [w.HWND, c.POINTER(w.DWORD)]
    user.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
    user.GetClassNameW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
    user.IsWindowVisible.argtypes = [w.HWND]
    user.GetClientRect.argtypes = [w.HWND, c.POINTER(w.RECT)]
    found = []
    @callback_type
    def visit(window, _):
        owner = w.DWORD()
        user.GetWindowThreadProcessId(window, c.byref(owner))
        # Never read another process's title, class or dimensions.
        if owner.value == pid:
            title, klass, rect = c.create_unicode_buffer(512), c.create_unicode_buffer(128), w.RECT()
            user.GetWindowTextW(window, title, len(title))
            user.GetClassNameW(window, klass, len(klass))
            sized = bool(user.GetClientRect(window, c.byref(rect)))
            found.append({"id": str(window), "owner_pid": pid, "title": title.value,
                          "class": klass.value, "visible": bool(user.IsWindowVisible(window)),
                          "width": rect.right - rect.left if sized else 0,
                          "height": rect.bottom - rect.top if sized else 0})
        return True
    if not user.EnumWindows(visit, 0):
        raise OSError("EnumWindows could not inspect the owned game PID")
    return {"method": "Win32 EnumWindows/GetClientRect", "windows": found}


def x11(pid: int) -> dict:
    x = x11_library()
    window = c.c_ulong
    x.XOpenDisplay.argtypes, x.XOpenDisplay.restype = [c.c_char_p], c.c_void_p
    x.XDefaultRootWindow.argtypes, x.XDefaultRootWindow.restype = [c.c_void_p], window
    x.XCloseDisplay.argtypes = [c.c_void_p]
    x.XFree.argtypes = [c.c_void_p]
    x.XInternAtom.argtypes, x.XInternAtom.restype = [c.c_void_p, c.c_char_p, c.c_int], c.c_ulong
    x.XQueryTree.argtypes = [c.c_void_p, window, c.POINTER(window), c.POINTER(window),
                            c.POINTER(c.POINTER(window)), c.POINTER(c.c_uint)]
    x.XGetWindowProperty.argtypes = [c.c_void_p, window, c.c_ulong, c.c_long, c.c_long,
        c.c_int, c.c_ulong, c.POINTER(c.c_ulong), c.POINTER(c.c_int), c.POINTER(c.c_ulong),
        c.POINTER(c.c_ulong), c.POINTER(c.POINTER(c.c_ubyte))]
    class Attributes(c.Structure):
        _fields_ = [(key, typ) for key, typ in (
            ("x", c.c_int), ("y", c.c_int), ("width", c.c_int), ("height", c.c_int),
            ("border_width", c.c_int), ("depth", c.c_int), ("visual", c.c_void_p),
            ("root", window), ("klass", c.c_int), ("bit_gravity", c.c_int),
            ("win_gravity", c.c_int), ("backing_store", c.c_int), ("backing_planes", c.c_ulong),
            ("backing_pixel", c.c_ulong), ("save_under", c.c_int), ("colormap", c.c_ulong),
            ("map_installed", c.c_int), ("map_state", c.c_int), ("all_event_masks", c.c_long),
            ("your_event_mask", c.c_long), ("do_not_propagate_mask", c.c_long),
            ("override_redirect", c.c_int), ("screen", c.c_void_p))]
    x.XGetWindowAttributes.argtypes = [c.c_void_p, window, c.POINTER(Attributes)]
    x.XFetchName.argtypes = [c.c_void_p, window, c.POINTER(c.c_void_p)]
    # A window can disappear between XQueryTree and a property read. Xlib's
    # default handler terminates the process; keep the race local to observation.
    error_type = c.CFUNCTYPE(c.c_int, c.c_void_p, c.c_void_p)
    x.XSetErrorHandler.argtypes, x.XSetErrorHandler.restype = [c.c_void_p], c.c_void_p
    @error_type
    def ignore_disappeared(_display, _error):
        return 0
    display = x.XOpenDisplay(None)
    if not display:
        raise OSError("No X11 display is accessible; no owned game window was observed")
    previous = x.XSetErrorHandler(c.cast(ignore_disappeared, c.c_void_p))
    found = []
    try:
        atom = x.XInternAtom(display, b"_NET_WM_PID", False)
        todo = [(x.XDefaultRootWindow(display), 0)]
        visited = set()
        while todo and len(visited) < 4096:
            item, depth = todo.pop()
            if item in visited:
                continue
            visited.add(item)
            kind, fmt, count, after, data = c.c_ulong(), c.c_int(), c.c_ulong(), c.c_ulong(), c.POINTER(c.c_ubyte)()
            ok = x.XGetWindowProperty(display, item, atom, 0, 1, False, 0,
                c.byref(kind), c.byref(fmt), c.byref(count), c.byref(after), c.byref(data))
            owner = 0
            if ok == 0 and data and fmt.value == 32 and count.value == 1:
                owner = c.cast(data, c.POINTER(c.c_ulong))[0]
            if data:
                x.XFree(data)
            if owner == pid:
                attributes = Attributes()
                if x.XGetWindowAttributes(display, item, c.byref(attributes)):
                    title = c.c_void_p()
                    x.XFetchName(display, item, c.byref(title))
                    text = c.string_at(title)[:2048].decode("utf-8", errors="replace") if title else ""
                    if title:
                        x.XFree(title)
                    found.append({"id": str(item), "owner_pid": pid, "title": text,
                                  "visible": attributes.map_state == 2,
                                  "width": attributes.width, "height": attributes.height})
            if depth < 3:
                root, parent, children, size = window(), window(), c.POINTER(window)(), c.c_uint()
                if x.XQueryTree(display, item, c.byref(root), c.byref(parent), c.byref(children), c.byref(size)):
                    if size.value <= 4096:
                        todo.extend((children[index], depth + 1) for index in range(size.value))
                    if children:
                        x.XFree(children)
        return {"method": "X11 _NET_WM_PID/XGetWindowAttributes", "windows": found}
    finally:
        x.XCloseDisplay(display)
        x.XSetErrorHandler(previous)


def macos(pid: int) -> dict:
    core = c.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    graphics = c.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    ptr = c.c_void_p
    graphics.CGWindowListCopyWindowInfo.argtypes, graphics.CGWindowListCopyWindowInfo.restype = [c.c_uint32, c.c_uint32], ptr
    core.CFArrayGetCount.argtypes, core.CFArrayGetCount.restype = [ptr], c.c_long
    core.CFArrayGetValueAtIndex.argtypes, core.CFArrayGetValueAtIndex.restype = [ptr, c.c_long], ptr
    core.CFDictionaryGetValue.argtypes, core.CFDictionaryGetValue.restype = [ptr, ptr], ptr
    core.CFStringCreateWithCString.argtypes, core.CFStringCreateWithCString.restype = [ptr, c.c_char_p, c.c_uint32], ptr
    core.CFStringGetCString.argtypes = [ptr, c.c_void_p, c.c_long, c.c_uint32]
    core.CFNumberGetValue.argtypes = [ptr, c.c_int, c.c_void_p]
    core.CFRelease.argtypes = [ptr]
    keys = {}
    def get(dictionary, key):
        if key not in keys:
            keys[key] = core.CFStringCreateWithCString(None, key.encode(), 0x08000100)
        return core.CFDictionaryGetValue(dictionary, keys[key])
    def number(value):
        result = c.c_longlong()
        return result.value if value and core.CFNumberGetValue(value, 4, c.byref(result)) else 0
    windows_list = graphics.CGWindowListCopyWindowInfo(1 | 16, 0)  # OnScreenOnly, ExcludeDesktopElements
    if not windows_list:
        raise OSError("CoreGraphics did not expose an on-screen window list")
    found = []
    try:
        count = core.CFArrayGetCount(windows_list)
        if count > 8192:
            raise ValueError("Native window inventory exceeded its bounded observation budget")
        for index in range(count):
            info = core.CFArrayGetValueAtIndex(windows_list, index)
            if number(get(info, "kCGWindowOwnerPID")) != pid:
                continue
            bounds = get(info, "kCGWindowBounds")
            title = c.create_string_buffer(2048)
            name = get(info, "kCGWindowName")
            if name:
                core.CFStringGetCString(name, title, len(title), 0x08000100)
            found.append({"id": str(number(get(info, "kCGWindowNumber"))), "owner_pid": pid,
                          "title": title.value.decode("utf-8", errors="replace"),
                          "visible": number(get(info, "kCGWindowLayer")) == 0,
                          "width": number(get(bounds, "Width")) if bounds else 0,
                          "height": number(get(bounds, "Height")) if bounds else 0})
        return {"method": "CoreGraphics CGWindowListCopyWindowInfo owner-PID filter", "windows": found}
    finally:
        core.CFRelease(windows_list)
        for key in keys.values():
            core.CFRelease(key)
