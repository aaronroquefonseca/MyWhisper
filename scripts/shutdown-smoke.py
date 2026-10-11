#!/usr/bin/env python3
"""Verify normal close, signals and recovery in the actual native binary.

Run inside disposable.py, a private D-Bus session and a native X11 display.
No real settings, keyring, clipboard, microphone, hotkeys or peers are used.
"""
import argparse
import ctypes
import ctypes.util
import json
import os
from pathlib import Path
import signal
import subprocess
import time


def close_window(window):
    class Data(ctypes.Union):
        _fields_ = [("l", ctypes.c_long * 5), ("pad", ctypes.c_char * 20)]

    class Message(ctypes.Structure):
        _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong),
                    ("send_event", ctypes.c_int), ("display", ctypes.c_void_p),
                    ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                    ("format", ctypes.c_int), ("data", Data)]

    class Event(ctypes.Union):
        _fields_ = [("message", Message), ("pad", ctypes.c_long * 24)]

    xlib = ctypes.CDLL(ctypes.util.find_library("X11"))
    xlib.XOpenDisplay.argtypes = [ctypes.c_char_p]
    xlib.XOpenDisplay.restype = ctypes.c_void_p
    xlib.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    xlib.XInternAtom.restype = ctypes.c_ulong
    xlib.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int,
                               ctypes.c_long, ctypes.POINTER(Event)]
    xlib.XFlush.argtypes = [ctypes.c_void_p]
    xlib.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = xlib.XOpenDisplay(os.environ["DISPLAY"].encode())
    assert display, "native display unavailable"
    try:
        event = Event()
        event.message.type = 33
        event.message.send_event = 1
        event.message.display = display
        event.message.window = window
        event.message.message_type = xlib.XInternAtom(display, b"WM_PROTOCOLS", 0)
        event.message.format = 32
        event.message.data.l[0] = xlib.XInternAtom(display, b"WM_DELETE_WINDOW", 0)
        assert xlib.XSendEvent(display, window, 0, 0, ctypes.byref(event))
        xlib.XFlush(display)
    finally:
        xlib.XCloseDisplay(display)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("fixture", type=Path)
    parser.add_argument("--case", choices=["gui-close", "gui-term", "gui-int", "service-term", "service-int", "service-repeat"])
    args = parser.parse_args()
    root = Path(os.environ["MYWHISPER_AUDIT_ROOT"])
    assert root.name.startswith("mywhisper-shutdown-audit.")
    assert (root / ".disposable").is_file() and root.stat().st_mode & 0o777 == 0o700
    assert root.stat().st_uid == os.getuid()
    cases = [(False, "close", []), (False, "term", [signal.SIGTERM]),
             (False, "int", [signal.SIGINT]), (True, "term", [signal.SIGTERM]),
             (True, "int", [signal.SIGINT]),
             (True, "repeat", [signal.SIGTERM, signal.SIGINT, signal.SIGTERM])]
    for service, action, signals in cases:
        name = ("service-" if service else "gui-") + action
        if args.case and args.case != name:
            continue
        case = root / name
        case.mkdir(mode=0o700)
        env = os.environ.copy()
        env["MYWHISPER_DB_KEY"] = "00" * 32
        for key, directory in [("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
                               ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"),
                               ("XDG_CACHE_HOME", "cache"), ("XDG_RUNTIME_DIR", "runtime")]:
            path = case / directory
            path.mkdir(mode=0o700)
            env[key] = str(path)
        # GTK portal configuration belongs to this test, not the real desktop.
        portal = case / "config/xdg-desktop-portal"
        portal.mkdir(mode=0o700)
        (portal / "portals.conf").write_text("[preferred]\ndefault=none\n")
        subprocess.run([str(args.fixture), "prepare"], env=env, check=True, timeout=10)
        logpath = case / "application.log"
        with logpath.open("xb") as log:
            command = [str(args.binary)] + (["--service"] if service else [])
            process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                       stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 15
                while True:
                    assert process.poll() is None, logpath.read_text(errors="replace")[-4000:]
                    if service:
                        ready = "[service] activate, headless engine" in logpath.read_text(errors="replace")
                    else:
                        windows = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(process.pid)],
                                                 capture_output=True, text=True, timeout=2)
                        ready = windows.returncode == 0 and windows.stdout.strip()
                    if ready:
                        break
                    assert time.monotonic() < deadline, "native startup deadline exceeded"
                    time.sleep(0.02)
                began = time.monotonic()
                if signals:
                    for signum in signals:
                        process.send_signal(signum)
                else:
                    close_window(int(windows.stdout.split()[0]))
                assert process.wait(timeout=3) == 0, name + ": exit was not graceful"
                text = logpath.read_text(errors="replace")
                assert "[app] owned shutdown complete; database checkpointed" in text, name + ": completion/checkpoint missing"
                assert "CRITICAL" not in text and "panicked" not in text, text[-4000:]
                subprocess.run([str(args.fixture), "verify"], env=env, check=True, timeout=10)
                print(json.dumps({"case": name, "result": "PASS", "elapsed": round(time.monotonic()-began, 3),
                                  "encrypted_data_preserved": True, "checkpoint": True}), flush=True)
            except Exception:
                print(logpath.read_text(errors="replace")[-6000:], flush=True)
                raise
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=3)


if __name__ == "__main__":
    main()
