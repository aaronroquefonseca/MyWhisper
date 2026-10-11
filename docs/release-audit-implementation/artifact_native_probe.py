"""Run the actual Nix-packaged GTK artifact in disposable XDG and read-only root.
Invoke through scripts/disposable.py + dbus-run-session + xvfb-run. No devices,
service, capture, provider credentials, real settings or host keyring are used.
"""
import ctypes
import ctypes.util
import os
import ipaddress
from pathlib import Path
import subprocess
import sys
import time

# Resolve the actual application's icon data with a fresh native GTK theme.
# The inspection helper uses the host GTK4/GI, while the visible application
# above runs the package's GTK4. Both run against exactly the wrapper paths;
# the screenshot/booted NixOS gate covers the package's renderer separately.
ICON_PROBE = r'''
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, Gdk
Gtk.init()
theme = Gtk.IconTheme.get_for_display(Gdk.Display.get_default())
icons = ["audio-input-microphone-symbolic", "view-list-symbolic",
         "accessories-dictionary-symbolic", "computer-symbolic",
         "preferences-system-symbolic"]
missing = [name for name in icons if not theme.has_icon(name)]
assert not missing, "Packaged fallback icons missing: " + repr(missing)
for name in icons:
    paintable = theme.lookup_icon(name, [], 24, 1, Gtk.TextDirection.NONE, Gtk.IconLookupFlags.FORCE_SYMBOLIC)
    assert paintable.is_symbolic(), name
print("All five packaged navigation icons resolve as symbolic icons without host icon files")
'''

root = Path(os.environ['MYWHISPER_AUDIT_ROOT']).resolve()
assert root.name.startswith('mywhisper-artifactnative-audit.') and (root / '.disposable').is_file()
assert root.stat().st_mode & 0o777 == 0o700
for field in ['HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'XDG_CACHE_HOME']:
    assert Path(os.environ[field]).resolve().is_relative_to(root)
store, artifact = map(Path, sys.argv[1:])
assert (store / 'store' / artifact.name / 'bin/.mywhisper-wrapped').is_file()
assert str(artifact).startswith('/nix/store/')
env = {**os.environ, 'MYWHISPER_DB_KEY': '00' * 32, 'GDK_BACKEND': 'x11', 'GTK_A11Y': 'none', 'G_DEBUG': 'fatal-criticals'}
(root / 'empty-icon-data').mkdir(mode=0o700)
env['XDG_DATA_DIRS'] = str(root / 'empty-icon-data')
command = ['bwrap', '--ro-bind', '/', '/', '--bind', str(root), str(root), '--ro-bind', str(store), '/nix',
           '--tmpfs', '/usr/share/icons', '--tmpfs', '/usr/share/pixmaps',
           '--unshare-user', '--unshare-pid', '--proc', '/proc', '--dev', '/dev', '--die-with-parent',
           str(artifact / 'bin/mywhisper')]
with (root / 'application.log').open('w') as log:
    process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 15
        window = None
        while time.monotonic() < deadline:
            assert process.poll() is None, (process.returncode, (root / 'application.log').read_text())
            result = subprocess.run(['xdotool', 'search', '--onlyvisible', '--name', '^MyWhisper$'], capture_output=True, text=True, timeout=2)
            if result.returncode == 0 and result.stdout.strip():
                window = int(result.stdout.splitlines()[0]); break
            time.sleep(.05)
        assert window is not None, 'Packaged native window did not become visible'
        geometry = subprocess.check_output(['xdotool', 'getwindowgeometry', '--shell', str(window)], text=True, timeout=2)
        dimensions = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
        assert int(dimensions['WIDTH']) > 0 and int(dimensions['HEIGHT']) > 0
        # Inspect only descendants of our disposable bubblewrap process. The
        # application must obtain getent from the package's actual wrapper PATH,
        # rather than accidentally finding a host executable outside NixOS.
        pending = [process.pid]
        application_path = None
        icon_data = None
        for _ in range(64):
            if not pending:
                break
            pid = pending.pop()
            proc = Path('/proc') / str(pid)
            if (proc / 'exe').readlink().name == '.mywhisper-wrapped':
                for field in (proc / 'environ').read_bytes().split(b'\0'):
                    if field.startswith(b'PATH='):
                        application_path = os.fsdecode(field[5:])
                    if field.startswith(b'XDG_DATA_DIRS='):
                        icon_data = os.fsdecode(field.split(b'=', 1)[1])
                break
            pending.extend(map(int, (proc / 'task' / str(pid) / 'children').read_text().split()))
        assert application_path is not None, 'Owned application environment was not found'
        assert icon_data is not None, 'Owned application icon paths were not found'
        subprocess.run(command[:-1] + ['/usr/bin/python3', '-c', ICON_PROBE],
                       env={**env, 'XDG_DATA_DIRS': icon_data}, check=True, timeout=10)
        resolver = None
        for directory in application_path.split(os.pathsep):
            candidate = Path(directory) / 'getent'
            if candidate.is_relative_to('/nix/store'):
                # Nix tool outputs can contain absolute store symlinks. Test
                # execution inside the same store mapping as the application.
                result = subprocess.run(command[:-1] + [str(candidate), 'ahosts', 'localhost'],
                                        env=env, text=True, capture_output=True, timeout=4)
                if result.returncode == 0:
                    assert any(ipaddress.ip_address(line.split()[0]).is_loopback
                               for line in result.stdout.splitlines())
                    resolver = candidate
                    break
        assert resolver is not None, ('Packaged wrapper has no getent resolver executable',
                                     [p for p in application_path.split(os.pathsep) if p.startswith('/nix/store/')])
        # Request normal GTK close through WM_DELETE_WINDOW, not XDestroyWindow
        # or killing the client. No window manager is needed for this event.
        class MessageData(ctypes.Union):
            _fields_ = [('b', ctypes.c_char * 20), ('s', ctypes.c_short * 10), ('l', ctypes.c_long * 5)]
        class ClientMessage(ctypes.Structure):
            _fields_ = [('type', ctypes.c_int), ('serial', ctypes.c_ulong), ('send_event', ctypes.c_int),
                        ('display', ctypes.c_void_p), ('window', ctypes.c_ulong), ('message_type', ctypes.c_ulong),
                        ('format', ctypes.c_int), ('data', MessageData)]
        class Event(ctypes.Union):
            _fields_ = [('client', ClientMessage), ('padding', ctypes.c_long * 24)]
        x = ctypes.CDLL(ctypes.util.find_library('X11'))
        x.XOpenDisplay.argtypes = [ctypes.c_char_p]; x.XOpenDisplay.restype = ctypes.c_void_p
        x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]; x.XInternAtom.restype = ctypes.c_ulong
        x.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.POINTER(Event)]
        x.XFlush.argtypes = [ctypes.c_void_p]; x.XCloseDisplay.argtypes = [ctypes.c_void_p]
        display = x.XOpenDisplay(None); assert display
        event = Event(); event.client.type = 33; event.client.display = display; event.client.window = window
        event.client.message_type = x.XInternAtom(display, b'WM_PROTOCOLS', 0); event.client.format = 32
        event.client.data.l[0] = x.XInternAtom(display, b'WM_DELETE_WINDOW', 0)
        assert x.XSendEvent(display, window, 0, 0, ctypes.byref(event)) != 0
        x.XFlush(display); x.XCloseDisplay(display)
        assert process.wait(timeout=10) == 0, (root / 'application.log').read_text()
        databases = list((root / 'config').rglob('*.db')) + list((root / 'data').rglob('*.db'))
        assert len(databases) == 1, databases
        assert not databases[0].read_bytes().startswith(b'SQLite format 3'), 'Artifact stored plaintext SQLite'
        print('Actual Nix-packaged GTK artifact passed: visible window, executable resolver from actual wrapper PATH, normal close/zero exit, encrypted disposable database, no input/audio devices or capture')
    finally:
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=3)
            except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=3)
