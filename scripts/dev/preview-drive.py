#!/usr/bin/env python3
"""Drives the app inside the nested KWin of preview-session.sh.

usage: preview-drive.py ACTION...   (preview-session.sh runs it)

  sleep S              wait S seconds
  move X Y             put the pointer at X,Y (screen pixels)
  click X Y            left click at X,Y
  rclick X Y           right click at X,Y
  scroll N             turn the wheel N notches, down when positive
  key NAME[+NAME...]   press keys together: key esc, key ctrl+a, key shift+tab
  type TEXT            type TEXT on a US layout
  geometry X Y W H     move and resize the app's window frame
  shot PATH            screenshot the whole nested screen into PATH (PNG)

Pointer and keys go through KWin's EIS (org.kde.KWin.EIS.RemoteDesktop)
with libei; the window through a KWin script; shots through spectacle.
All of it is reached over DBus, so the script runs only on the private
session bus that preview-session.sh starts: it refuses a bus other than
the one named in GSE_SESSION_BUS, and a KWin that is not the virtual
(nested) one. Needs the host's libei and python3-dbus.
"""

import ctypes as C
import os
import select
import struct
import subprocess
import sys
import tempfile
import time

import dbus

APP_CLASS = "io.github.sovgut.gse"

ei = C.CDLL("libei.so.1")
P = C.c_void_p
for _name, _res, _args in [
    ("ei_new_sender", P, [P]),
    ("ei_configure_name", None, [P, C.c_char_p]),
    ("ei_setup_backend_fd", C.c_int, [P, C.c_int]),
    ("ei_get_fd", C.c_int, [P]),
    ("ei_dispatch", None, [P]),
    ("ei_get_event", P, [P]),
    ("ei_event_get_type", C.c_int, [P]),
    ("ei_event_type_to_string", C.c_char_p, [C.c_int]),
    ("ei_event_get_seat", P, [P]),
    ("ei_event_get_device", P, [P]),
    ("ei_event_unref", P, [P]),
    ("ei_seat_ref", P, [P]),
    ("ei_device_ref", P, [P]),
    ("ei_device_has_capability", C.c_bool, [P, C.c_int]),
    ("ei_device_start_emulating", None, [P, C.c_uint32]),
    ("ei_device_pointer_motion_absolute", None, [P, C.c_double, C.c_double]),
    ("ei_device_button_button", None, [P, C.c_uint32, C.c_bool]),
    ("ei_device_keyboard_key", None, [P, C.c_uint32, C.c_bool]),
    ("ei_device_scroll_discrete", None, [P, C.c_int32, C.c_int32]),
    ("ei_device_frame", None, [P, C.c_uint64]),
    ("ei_now", C.c_uint64, [P]),
]:
    _f = getattr(ei, _name)
    _f.restype = _res
    _f.argtypes = _args
ei.ei_seat_bind_capabilities.restype = None

# libei's enum ei_device_capability.
CAP_POINTER, CAP_ABSOLUTE, CAP_KEYBOARD, CAP_TOUCH, CAP_SCROLL, CAP_BUTTON = (
    1 << i for i in range(6)
)
# KWin's RemoteDesktop capabilities: keyboard and pointer.
KWIN_KEYBOARD_POINTER = 3
BTN_LEFT, BTN_RIGHT = 0x110, 0x111

# Linux evdev key codes, read through the nested KWin's keymap.
KEYS = {
    "esc": 1, "1": 2, "2": 3, "3": 4, "4": 5, "5": 6, "6": 7, "7": 8, "8": 9,
    "9": 10, "0": 11, "-": 12, "=": 13, "backspace": 14, "tab": 15, "q": 16,
    "w": 17, "e": 18, "r": 19, "t": 20, "y": 21, "u": 22, "i": 23, "o": 24,
    "p": 25, "[": 26, "]": 27, "enter": 28, "ctrl": 29, "a": 30, "s": 31,
    "d": 32, "f": 33, "g": 34, "h": 35, "j": 36, "k": 37, "l": 38, ";": 39,
    "'": 40, "`": 41, "shift": 42, "\\": 43, "z": 44, "x": 45, "c": 46,
    "v": 47, "b": 48, "n": 49, "m": 50, ",": 51, ".": 52, "/": 53, "alt": 56,
    "space": 57, " ": 57, "f1": 59, "f2": 60, "f3": 61, "f4": 62, "f5": 63,
    "f6": 64, "f7": 65, "f8": 66, "f9": 67, "f10": 68, "f11": 87, "f12": 88,
    "home": 102, "up": 103, "pageup": 104, "left": 105, "right": 106,
    "end": 107, "down": 108, "pagedown": 109, "insert": 110, "delete": 111,
    "super": 125,
}
SHIFTED = {
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7",
    "*": "8", "(": "9", ")": "0", "_": "-", "+": "=", "{": "[", "}": "]",
    ":": ";", '"': "'", "~": "`", "|": "\\", "<": ",", ">": ".", "?": "/",
}


def nested_bus():
    """The session bus, once it is known to be the nested KWin's."""
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    if not address or os.environ.get("GSE_SESSION_BUS") != address:
        sys.exit("preview-drive.py: not inside preview-session.sh's nested session")
    bus = dbus.SessionBus()
    kwin = bus.get_object("org.kde.KWin", "/KWin")
    info = kwin.supportInformation(dbus_interface="org.kde.KWin")
    if "KWin::VirtualBackend" not in info:
        sys.exit("preview-drive.py: the KWin on this bus is not a nested virtual one")
    return bus


class Input:
    """A pointer and a keyboard emulated through KWin's EIS."""

    def __init__(self, bus):
        remote = bus.get_object("org.kde.KWin", "/org/kde/KWin/EIS/RemoteDesktop")
        fd, _cookie = remote.connectToEIS(
            dbus.Int32(KWIN_KEYBOARD_POINTER),
            dbus_interface="org.kde.KWin.EIS.RemoteDesktop",
        )
        self.ctx = ei.ei_new_sender(None)
        ei.ei_configure_name(self.ctx, b"preview-drive")
        if ei.ei_setup_backend_fd(self.ctx, fd.take()) != 0:
            sys.exit("preview-drive.py: libei could not use KWin's EIS socket")
        self.devices = {}
        self.sequence = 1
        deadline = time.time() + 5
        while time.time() < deadline and not (self.pointer() and self.keyboard()):
            self.pump(0.2)
        if not (self.pointer() and self.keyboard()):
            sys.exit("preview-drive.py: KWin's EIS gave no pointer and keyboard")

    def device(self, capability):
        return next(
            (d for d, resumed in self.devices.items()
             if resumed and ei.ei_device_has_capability(d, capability)),
            None,
        )

    def pointer(self):
        return self.device(CAP_ABSOLUTE)

    def keyboard(self):
        return self.device(CAP_KEYBOARD)

    def pump(self, timeout=0.05):
        select.select([ei.ei_get_fd(self.ctx)], [], [], timeout)
        ei.ei_dispatch(self.ctx)
        while event := ei.ei_get_event(self.ctx):
            kind = ei.ei_event_type_to_string(ei.ei_event_get_type(event)).decode()
            if kind.endswith("SEAT_ADDED"):
                seat = ei.ei_seat_ref(ei.ei_event_get_seat(event))
                caps = (CAP_POINTER, CAP_ABSOLUTE, CAP_KEYBOARD, CAP_SCROLL, CAP_BUTTON)
                ei.ei_seat_bind_capabilities(
                    P(seat), *(C.c_int(c) for c in caps), P(None)
                )
            elif kind.endswith("DEVICE_ADDED"):
                self.devices[ei.ei_device_ref(ei.ei_event_get_device(event))] = False
            elif kind.endswith("DEVICE_RESUMED"):
                device = ei.ei_event_get_device(event)
                self.devices[device] = True
                ei.ei_device_start_emulating(device, self.sequence)
                self.sequence += 1
            elif kind.endswith("DEVICE_PAUSED"):
                self.devices[ei.ei_event_get_device(event)] = False
            ei.ei_event_unref(event)

    def frame(self, device):
        ei.ei_device_frame(device, ei.ei_now(self.ctx))
        self.pump(0.03)

    def move(self, x, y):
        ei.ei_device_pointer_motion_absolute(self.pointer(), x, y)
        self.frame(self.pointer())

    def click(self, x, y, button):
        self.move(x, y)
        for down in (True, False):
            ei.ei_device_button_button(self.pointer(), button, down)
            self.frame(self.pointer())

    def scroll(self, notches):
        wheel = self.device(CAP_SCROLL)
        ei.ei_device_scroll_discrete(wheel, 0, notches * 120)
        self.frame(wheel)

    def chord(self, names):
        unknown = [n for n in names if n not in KEYS]
        if unknown:
            sys.exit(f"preview-drive.py: unknown key {unknown[0]!r}")
        codes = [KEYS[n] for n in names]
        for down, order in ((True, codes), (False, reversed(codes))):
            for code in order:
                ei.ei_device_keyboard_key(self.keyboard(), code, down)
                self.frame(self.keyboard())

    def type(self, text):
        for ch in text:
            if ch in SHIFTED:
                self.chord(["shift", SHIFTED[ch]])
            elif ch.isupper():
                self.chord(["shift", ch.lower()])
            else:
                self.chord([ch])
            time.sleep(0.02)


def kwin_script(bus, body):
    """Runs JavaScript once in the nested KWin."""
    with tempfile.NamedTemporaryFile("w", suffix=".js", delete=False) as f:
        f.write(body)
    scripting = dbus.Interface(
        bus.get_object("org.kde.KWin", "/Scripting"), "org.kde.kwin.Scripting"
    )
    name = f"preview-drive-{time.time_ns()}"
    try:
        number = scripting.loadScript(f.name, name, signature="ss")
        script = bus.get_object("org.kde.KWin", f"/Scripting/Script{number}")
        script.run(dbus_interface="org.kde.kwin.Script")
        time.sleep(0.3)
        scripting.unloadScript(name)
    finally:
        os.unlink(f.name)


def shot(path):
    """Screenshots the nested screen and checks a PNG came out."""
    if os.path.exists(path):
        os.unlink(path)
    try:
        subprocess.run(
            ["spectacle", "--background", "--nonotify", "--fullscreen", "--output", path],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=False,
        )
    except subprocess.TimeoutExpired:
        sys.exit(f"preview-drive.py: spectacle did not write {path} within 15 s")
    try:
        with open(path, "rb") as f:
            head = f.read(24)
    except OSError:
        sys.exit(f"preview-drive.py: spectacle wrote no {path}")
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"preview-drive.py: {path} is not a PNG")
    width, height = struct.unpack(">II", head[16:24])
    print(f"{path} {width}x{height}", file=sys.stderr)


def main(argv):
    bus = nested_bus()
    inp = None
    arity = {"sleep": 1, "move": 2, "click": 2, "rclick": 2, "scroll": 1,
             "key": 1, "type": 1, "geometry": 4, "shot": 1}
    i = 0
    while i < len(argv):
        action = argv[i]
        if action not in arity:
            sys.exit(f"preview-drive.py: unknown action {action!r}")
        args = argv[i + 1:i + 1 + arity[action]]
        if len(args) < arity[action]:
            sys.exit(f"preview-drive.py: {action} takes {arity[action]} argument(s)")
        i += 1 + arity[action]
        if action in ("move", "click", "rclick", "scroll", "key", "type") and inp is None:
            inp = Input(bus)
        if action == "sleep":
            time.sleep(float(args[0]))
        elif action == "move":
            inp.move(float(args[0]), float(args[1]))
        elif action == "click":
            inp.click(float(args[0]), float(args[1]), BTN_LEFT)
        elif action == "rclick":
            inp.click(float(args[0]), float(args[1]), BTN_RIGHT)
        elif action == "scroll":
            inp.scroll(int(args[0]))
        elif action == "key":
            inp.chord(args[0].lower().split("+"))
        elif action == "type":
            inp.type(args[0])
        elif action == "geometry":
            x, y, w, h = (int(a) for a in args)
            kwin_script(bus, (
                "for (const w of workspace.windowList())"
                f" if (w.resourceClass == '{APP_CLASS}')"
                f" w.frameGeometry = {{x: {x}, y: {y}, width: {w}, height: {h}}};"
            ))
        elif action == "shot":
            shot(args[0])


if __name__ == "__main__":
    main(sys.argv[1:])
