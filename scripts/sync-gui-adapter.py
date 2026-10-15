#!/usr/bin/env python3
"""AT-SPI GUI control adapter for sync pairing gates (#23-owned).

Drives the REAL application process through native accessibility: finds
controls by accessible name/text, actuates them, and asserts results.
No production hooks, no SQL writes, no seeded trust reported as pairing.

#18 wiring (their file, proposed snippet — this module is imported):
    import sys
    sys.path.insert(0, "<repo>/scripts")
    from sync_gui_adapter import SyncGuiDriver, AdapterError
    from sync_2guest_gate import Fail  # their Fail type

    class BoundDriver(SyncGuiDriver):
        def control(self, guest, control, *args):
            try:
                return super().control(guest, control, *args)
            except AdapterError as error:
                raise Fail(str(error))

    guests = [... Guest(..., ui=BoundDriver(adapter="/opt/gate/scripts/sync-gui-adapter.py")) ...]
Guest.start on ssh guests must export the Sway session WAYLAND_DISPLAY +
DBUS_SESSION_BUS_ADDRESS for the app AND the adapter ssh command inherits
them from the guest login shell. Local backend runs headless service only
(no GUI), so UI controls fail closed there by contract.

Guest image deps for #15 (target side): python3, python3-gobject
(gi.repository.Atspi client), at-spi2-core (bus launcher + registry
daemon), a running session bus + compositor. No app changes needed:
switches carry explicit accessible labels, buttons expose label text,
entries expose label text. English UI required: pre-seed
preferences.ui_language="en" via stopped-app bootstrap fixture (the
adapter gates on the English "Devices" nav and fails closed otherwise).

CLI contract (runs locally or over ssh; single JSON line on stdout):
    sync-gui-adapter.py [--timeout SEC] [--log PATH] [--check] \\
        <control> [args...]
    controls:
      trust <node-id>
      send-request <node-id> [name] [address]
      accept <node-id> | decline <node-id>
      sync-now <node-id>
      remove <node-id>
      sync-enable <on|off>
    -> {"ok": true, "control": ..., "evidence": {...}} (exit 0)
    -> {"ok": false, "control": ..., "error": "..."} (exit 1)

Selector map (English; short_id() replicated exactly from device.rs):
  nav Devices | sync-enable switch "Enable device sync"
  nearby row / peer card / request row: static text containing short_id
  trust switch "Trusted device" | send "Send request" | accept "Accept"
  decline "Decline" | sync-now "Sync now" | remove "Remove"
  confirm dialog "Remove this paired device?" + "Remove"
  id entry "Other device’s ID" (U+2019), expander "Manual address & name (optional)"
Expected app log lines (secondary evidence with --log):
  trust "sync: trust", send "sync: pairing requested",
  accept "sync.accepted", remove "sync: removed peer"
"""

import argparse
import json
import os
import subprocess
import sys
import time

READY_TIMEOUT = 30
FIND_TIMEOUT = 15
RESULT_TIMEOUT = 30
DIALOG_TIMEOUT = 10

try:
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi

    HAVE_ATSPI = True
except Exception:
    HAVE_ATSPI = False
    Atspi = None

LOG_LINES = {
    "trust": "sync: trust",
    "send-request": "sync: pairing requested",
    "accept": "sync.accepted",
    "remove": "sync: removed peer",
}

# Accessible names / visible text in the required English UI.
T_NAV_DEVICES = "Devices"
T_SYNC_ENABLE = "Enable device sync"
T_TRUST = "Trusted device"
T_SEND = "Send request"
T_ACCEPT = "Accept"
T_DECLINE = "Decline"
T_SYNC_NOW = "Sync now"
T_REMOVE = "Remove"
T_REMOVE_TITLE = "Remove this paired device?"
T_ID_ENTRY = "Other device’s ID"
T_ADDR_ENTRY = "Address override, e.g. 192.168.1.31:42973 (optional)"
T_NAME_ENTRY = "Name (optional)"
T_OPTIONAL = "Manual address & name (optional)"


class AdapterError(Exception):
    """Fail-closed control error; message already carries guest context."""


def short_id(node_id):
    """Exact replica of device::short_id (chars, head8+…+tail4 over 16)."""
    chars = list(node_id)
    if len(chars) <= 16:
        return node_id
    return "".join(chars[:8]) + "…" + "".join(chars[-4:])


def check_prereqs(want_app=True):
    """Return list of missing prerequisites (empty = runnable)."""
    missing = []
    if not HAVE_ATSPI:
        missing.append(
            "python gi.repository.Atspi (python3-gobject + at-spi2-core typelib)"
        )
    if not os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
        missing.append("display (DISPLAY or WAYLAND_DISPLAY)")
    if not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        missing.append("session bus (DBUS_SESSION_BUS_ADDRESS)")
    else:
        out = subprocess.run(
            ["busctl", "--user", "list"],
            capture_output=True,
            text=True,
        )
        if "org.a11y.Bus" not in out.stdout:
            missing.append(
                "AT-SPI bus owner (start /usr/lib/at-spi-bus-launcher "
                "and /usr/lib/at-spi2-registryd on the session bus)"
            )
    if want_app and not missing:
        try:
            desktop = Atspi.get_desktop(0)
            desktop.get_child_count()
        except Exception as error:
            missing.append(f"AT-SPI desktop query: {error}")
    return missing


def iter_tree(root):
    stack = [root]
    while stack:
        node = stack.pop()
        yield node
        try:
            for index in range(node.get_child_count()):
                try:
                    child = node.get_child_at_index(index)
                    if child is not None:
                        stack.append(child)
                except Exception:
                    pass
        except Exception:
            pass


def find_node(root, role_sub=None, name=None, timeout=FIND_TIMEOUT):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            for node in iter_tree(root):
                try:
                    role = node.get_role_name() or ""
                    text = node.get_name() or ""
                except Exception:
                    continue
                if (role_sub is None or role_sub in role) and (
                    name is None or text == name
                ):
                    return node
        except Exception:
            pass
        time.sleep(0.5)
    return None


def find_text_containing(root, fragment, timeout=FIND_TIMEOUT):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            for node in iter_tree(root):
                try:
                    role = node.get_role_name() or ""
                    text = node.get_name() or ""
                except Exception:
                    continue
                if role in ("label", "static text", "text") and fragment in text:
                    return node
        except Exception:
            pass
        time.sleep(0.5)
    return None


def ancestors(node, limit=8):
    out = []
    current = node
    for _ in range(limit):
        try:
            current = current.get_parent()
        except Exception:
            break
        if current is None:
            break
        out.append(current)
    return out


def subtree_has_named(container, text, role_sub="button"):
    try:
        for node in iter_tree(container):
            try:
                if (
                    role_sub in (node.get_role_name() or "")
                    and (node.get_name() or "") == text
                ):
                    return node
            except Exception:
                continue
    except Exception:
        pass
    return None


def find_all_text_containing(root, fragment):
    """All label/static/text nodes containing the fragment (one sweep)."""
    found = []
    try:
        for node in iter_tree(root):
            try:
                role = node.get_role_name() or ""
                text = node.get_name() or ""
            except Exception:
                continue
            if role in ("label", "static text", "text") and fragment in text:
                found.append(node)
    except Exception:
        pass
    return found


def scope_button(desktop, peer_id, button_text, timeout=FIND_TIMEOUT, role="button"):
    """Find the peer card/row (by short_id text) then the control inside it.

    Every text node carrying the id is tried: status lines also echo the
    id, so the first marker alone may scope the wrong card. role selects
    the control kind (switches share no substring with buttons).
    """
    sid = short_id(peer_id)
    deadline = time.time() + timeout
    while time.time() < deadline:
        for marker in find_all_text_containing(desktop, sid):
            for container in [marker] + ancestors(marker):
                button = subtree_has_named(container, button_text, role)
                if button is not None:
                    return button
        time.sleep(0.5)
    return None


def click(node, what):
    try:
        action = node.get_action_iface()
    except Exception as error:
        raise AdapterError(f"no action interface on {what}: {error}")
    if action is None or action.get_n_actions() < 1:
        raise AdapterError(f"no clickable action on {what}")
    try:
        if not action.do_action(0):
            raise AdapterError(f"action failed on {what}")
    except AdapterError:
        raise
    except Exception as error:
        raise AdapterError(f"action error on {what}: {error}")


def switch_checked(node):
    try:
        return node.get_state_set().contains(Atspi.StateType.CHECKED)
    except Exception as error:
        raise AdapterError(f"switch state unreadable: {error}")


def wait_gone(node, timeout=RESULT_TIMEOUT):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            node.get_role_name()
        except Exception:
            return True
        time.sleep(0.5)
    return False


def wait_log(path, fragment, timeout=RESULT_TIMEOUT):
    if not path:
        return None
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                if fragment in handle.read():
                    return True
        except OSError:
            pass
        time.sleep(1)
    return False


class Adapter:
    def __init__(self, timeout=120, log=None):
        self.timeout = timeout
        self.log = log
        self.desktop = None

    def ready(self):
        missing = check_prereqs()
        if missing:
            raise AdapterError("unavailable UI prerequisites: " + "; ".join(missing))
        self.desktop = Atspi.get_desktop(0)
        nav = find_node(self.desktop, "button", T_NAV_DEVICES, timeout=READY_TIMEOUT)
        if nav is None:
            raise AdapterError(
                "English Devices nav missing (pre-seed preferences.ui_language=en)"
            )
        click(nav, "Devices nav")
        page = find_node(self.desktop, "label", T_NAV_DEVICES, timeout=FIND_TIMEOUT)
        if page is None:
            raise AdapterError("Devices page never appeared")
        return {"app": "mywhisper"}

    def evidence(self, control, extra=None):
        data = {"control": control}
        if self.log and control in LOG_LINES:
            data["log_matched"] = bool(
                wait_log(self.log, LOG_LINES[control], timeout=RESULT_TIMEOUT)
            )
            if not data["log_matched"]:
                raise AdapterError(f"{control}: transition log line never appeared")
        if extra:
            data.update(extra)
        return data

    def do_trust(self, peer_id):
        button = scope_button(self.desktop, peer_id, T_TRUST, role="switch")
        if button is None:
            raise AdapterError(f"trust switch for peer {short_id(peer_id)} not found")
        # Idempotent: manual adds already arrive trusted; only toggle a
        # switch that reads off, then assert the checked state.
        before = switch_checked(button)
        if not before:
            click(button, "trust switch")
            deadline = time.time() + RESULT_TIMEOUT
            while time.time() < deadline:
                if switch_checked(button):
                    break
                time.sleep(0.5)
            else:
                raise AdapterError("trust switch never settled checked")
        return self.evidence(
            "trust", {"peer": short_id(peer_id), "before": before, "after": True}
        )

    def do_send_request(self, peer_id, name="", address=""):
        button = scope_button(self.desktop, peer_id, T_SEND)
        if button is not None:
            click(button, "nearby send-request")
            return self.evidence("send-request", {"peer": short_id(peer_id)})
        entry = find_node(self.desktop, "text", T_ID_ENTRY)
        if entry is None:
            raise AdapterError("manual id entry not found")
        try:
            editable = entry.get_editable_text_iface()
            if editable is None or not editable.set_text_contents(peer_id):
                raise AdapterError("entry text rejected")
        except AdapterError:
            raise
        except Exception as error:
            raise AdapterError(f"entry typing failed: {error}")
        if name or address:
            expander = find_node(self.desktop, None, T_OPTIONAL, timeout=FIND_TIMEOUT)
            if expander is not None:
                try:
                    expander.get_action_iface().do_action(0)
                except Exception:
                    pass
                time.sleep(1)
            for label, value in ((T_ADDR_ENTRY, address), (T_NAME_ENTRY, name)):
                if not value:
                    continue
                field = find_node(self.desktop, "text", label, timeout=FIND_TIMEOUT)
                if field is None:
                    raise AdapterError(f"manual entry {label!r} not found")
                try:
                    editable = field.get_editable_text_iface()
                    if editable is None or not editable.set_text_contents(value):
                        raise AdapterError(f"entry {label!r} rejected text")
                except AdapterError:
                    raise
                except Exception as error:
                    raise AdapterError(f"entry {label!r} typing failed: {error}")
        button = find_node(self.desktop, "button", T_SEND)
        if button is None:
            raise AdapterError("manual send-request button not found")
        click(button, "manual send-request")
        marker = find_text_containing(
            self.desktop, short_id(peer_id), timeout=RESULT_TIMEOUT
        )
        if marker is None:
            raise AdapterError("peer card never appeared after send-request")
        return self.evidence("send-request", {"peer": short_id(peer_id)})

    def do_accept(self, peer_id, accept=True):
        label = T_ACCEPT if accept else T_DECLINE
        button = scope_button(self.desktop, peer_id, label)
        if button is None:
            raise AdapterError(f"request {label} for {short_id(peer_id)} not found")
        click(button, f"request {label}")
        return self.evidence(
            "accept" if accept else "decline", {"peer": short_id(peer_id)}
        )

    def do_sync_now(self, peer_id):
        # Sensitivity follows trust/targets/syncing through the 2s list
        # repaint; re-resolve boundedly instead of judging one snapshot.
        deadline = time.time() + RESULT_TIMEOUT
        while time.time() < deadline:
            button = scope_button(self.desktop, peer_id, T_SYNC_NOW, timeout=2)
            if button is None:
                time.sleep(0.5)
                continue
            try:
                sensitive = button.get_state_set().contains(Atspi.StateType.SENSITIVE)
            except Exception:
                sensitive = False
            if sensitive:
                click(button, "sync-now")
                return self.evidence("sync-now", {"peer": short_id(peer_id)})
            time.sleep(0.5)
        raise AdapterError("sync-now insensitive (untrusted or no targets)")

    def do_remove(self, peer_id):
        # Find-click-verify with re-resolve: the 2s list repaint can
        # replace widgets between find and click (e.g. right after a
        # trust toggle), leaving a dead node that clicks nowhere.
        # A click that produces no dialog is retried from a fresh find.
        # The confirm surface is an alert titled T_REMOVE_TITLE (role
        # "alert", not "dialog"). The confirm button is scoped to it so
        # a peer card's own Remove button can never satisfy this step.
        dialog = None
        for _ in range(3):
            button = scope_button(self.desktop, peer_id, T_REMOVE)
            if button is None:
                raise AdapterError(f"remove for {short_id(peer_id)} not found")
            click(button, "remove")
            deadline = time.time() + DIALOG_TIMEOUT
            while time.time() < deadline and dialog is None:
                try:
                    for node in iter_tree(self.desktop):
                        try:
                            role = node.get_role_name() or ""
                            text = node.get_name() or ""
                        except Exception:
                            continue
                        if "alert" in role and text == T_REMOVE_TITLE:
                            dialog = node
                            break
                except Exception:
                    pass
                if dialog is None:
                    time.sleep(0.5)
            if dialog is not None:
                break
        if dialog is None:
            raise AdapterError("remove confirm dialog never appeared")
        confirm = subtree_has_named(dialog, T_REMOVE) or dialog
        click(confirm, "remove confirm")
        if not wait_gone(dialog):
            raise AdapterError("remove confirm dialog never closed")
        return self.evidence("remove", {"peer": short_id(peer_id)})

    def do_sync_enable(self, want):
        want_on = want.lower() in ("on", "true", "1", "yes")
        switch = find_node(self.desktop, "switch", T_SYNC_ENABLE)
        if switch is None:
            raise AdapterError("sync-enable switch not found")
        before = switch_checked(switch)
        if before != want_on:
            click(switch, "sync-enable switch")
            deadline = time.time() + RESULT_TIMEOUT
            while time.time() < deadline:
                if switch_checked(switch) == want_on:
                    break
                time.sleep(0.5)
            else:
                raise AdapterError("sync-enable never settled")
        return self.evidence(
            "sync-enable", {"before": before, "after": switch_checked(switch)}
        )


def run_cli(argv):
    parser = argparse.ArgumentParser()
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--log", default="")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("control", nargs="?")
    parser.add_argument("args", nargs="*")
    options = parser.parse_args(argv)
    if options.check:
        missing = check_prereqs(want_app=False)
        print(json.dumps({"ok": not missing, "missing": missing}))
        return 0 if not missing else 1
    if not options.control:
        print(json.dumps({"ok": False, "error": "control required"}))
        return 1
    adapter = Adapter(timeout=options.timeout, log=options.log or None)
    handlers = {
        "trust": lambda a: adapter.do_trust(a[0]),
        "send-request": lambda a: adapter.do_send_request(a[0], *a[1:3]),
        "accept": lambda a: adapter.do_accept(a[0], True),
        "decline": lambda a: adapter.do_accept(a[0], False),
        "sync-now": lambda a: adapter.do_sync_now(a[0]),
        "remove": lambda a: adapter.do_remove(a[0]),
        "sync-enable": lambda a: adapter.do_sync_enable(a[0]),
    }
    handler = handlers.get(options.control)
    if handler is None:
        print(
            json.dumps(
                {"ok": False, "control": options.control, "error": "unknown control"}
            )
        )
        return 1
    try:
        adapter.ready()
        evidence = handler(options.args)
        print(json.dumps({"ok": True, "evidence": evidence}))
        return 0
    except AdapterError as error:
        print(
            json.dumps({"ok": False, "control": options.control, "error": str(error)})
        )
        return 1
    except Exception as error:
        print(
            json.dumps(
                {"ok": False, "control": options.control, "error": f"adapter: {error}"}
            )
        )
        return 1


class SyncGuiDriver:
    """Drop-in UI driver for #18 Guest(ui=...).

    Runs scripts/sync-gui-adapter.py against the guest's session: locally
    (dev/selftest only) or over ssh (guest sessions). Raises AdapterError;
    #18 wraps it into Fail at its call sites.
    """

    def __init__(self, adapter, timeout=180, ssh_env=None):
        self.adapter = adapter
        self.timeout = timeout
        self.ssh_env = ssh_env or {}

    def _invoke(self, guest, control, args):
        command = [
            "python3",
            self.adapter,
            "--timeout",
            str(self.timeout),
        ]
        if guest.log_path:
            command += ["--log", guest.log_path]
        command += [control] + [str(value) for value in args]
        if guest.ssh:
            remote = " ".join(f"{key}='{value}'" for key, value in self.ssh_env.items())
            prefix = ["ssh", guest.ssh]
            if remote:
                command = ["ssh", guest.ssh, remote + " " + " ".join(command)]
            else:
                command = prefix + command
            result = subprocess.run(
                command, capture_output=True, text=True, timeout=self.timeout + 60
            )
        else:
            env = dict(os.environ)
            env.update(guest.base_env())
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=self.timeout + 60,
                env=env,
            )
        try:
            payload = json.loads(result.stdout.strip().splitlines()[-1])
        except Exception:
            raise AdapterError(
                f"{guest.name}: adapter output unparseable: {result.stdout[-300:]}"
                f" stderr: {result.stderr[-300:]}"
            )
        if result.returncode != 0 or not payload.get("ok"):
            raise AdapterError(
                f"{guest.name}: UI control '{control}' failed: "
                f"{payload.get('error', 'unknown')}"
            )
        return payload.get("evidence", {})

    def control(self, guest, control, *args):
        return self._invoke(guest, control, args)


if __name__ == "__main__":
    sys.exit(run_cli(sys.argv[1:]))
