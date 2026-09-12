#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Drive the server's own two surfaces: the web interface and the state file.

Neither is reached by the goldens, the transport probe or the G-1 harness, and
2.0.0~alpha-6 shipped with both of them broken. This script is what keeps them
fixed.

1. **Every web page is served and the server survives it** (decision Q-25).
   PAPPL 1.3.1 looks the footer HTML up in its localisation table on every page
   it serves, and looks up a null rather than skipping it, so a server that was
   given no footer segfaults the first time anyone opens
   `http://localhost:8631/` — the address README recommends. Each page below
   must answer 200 and the server must still be running afterwards.
2. **PAPPL restores and advertises its printers**, and the normal server
   listens on wildcard addresses (Q-26). The file is saved and loaded through
   PAPPL's own mainloop, preserving existing installations. Registration is
   checked in the log; where Avahi is available its resolved IPP record must
   name this test port and printer. Network availability is reported explicitly.

One check is conditional and says so when it skips. The "Add Printer" page is
the only page that lists devices, and PAPPL hands its DNS-SD library a null
client when D-Bus cannot be reached at all, which aborts the process — an
upstream defect (S-6 in docs/DEBIAN-BUG-DRAFT.md) that this application cannot
prevent, since the page is PAPPL's own. In an environment with no system bus
the page is therefore not fetched, and the run says why.

Where the state file goes is not this script's choice, and that is the point of
checking it: PAPPL chooses its own state path, so an
ordinary user's run lands in the scoped XDG_CONFIG_HOME below and a **root**
run lands in `/var/lib/<base name>.state`, because PAPPL ignores
XDG_CONFIG_HOME as root. CI's container runs as root and therefore exercises
that branch. An existing file there is copied to `<file>.probe-backup`, put
back at the end, and this script's own file is removed — a backup left behind
means the run was killed part way through, and the file beside it is the one
to restore.

Requires: cargo build -p ml216x-printer-app. Optional: avahi-browse.
XDG_CONFIG_HOME and TMPDIR are scoped to a temporary directory, so nothing
else this script does can reach the developer's own PAPPL state.
"""
import argparse
import http.client
import os
import shutil
import re
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PRINTER = "probeq"
# Every page PAPPL registers for a system with one printer, less the two that
# act on a POST (`cancelall`, `delete`); the GET side of those is a form, but a
# probe has no business asking for pages whose purpose is to destroy something.
# `addprinter` is handled separately: it is the only one that lists devices.
PAGES = [
    "/",
    "/config",
    f"/{PRINTER}/",
    f"/{PRINTER}/config",
    f"/{PRINTER}/jobs",
    f"/{PRINTER}/media",
    f"/{PRINTER}/printing",
    "/style.css",
    "/favicon.png",
]
DEVICE_PAGE = "/addprinter"
SYSTEM_BUS = "/run/dbus/system_bus_socket"


def state_path(application, work):
    """Where the application will keep its state file, by PAPPL's own rules.

    Root keeps it in `/var/lib` and ignores XDG_CONFIG_HOME; everyone else
    gets the directory this script scoped. Deliberately derived here rather
    than read out of the server's log, so that the log line can be compared
    against it: agreeing with PAPPL's mainloop is what keeps an upgrade from
    losing a user's printers, and it is the one thing a unit test cannot check
    end to end.
    """
    directory = Path("/var/lib") if os.geteuid() == 0 else work
    return directory / f"{application.name}.state"


def fetch(port, page, timeout):
    """(status, None) for an answer, (None, reason) for anything else."""
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}{page}", timeout=timeout) as f:
            f.read()
            return f.status, None
    except urllib.error.HTTPError as error:
        return error.code, None
    except (urllib.error.URLError, http.client.HTTPException, OSError) as error:
        # A body that stops half way through is how a server dying mid-page
        # arrives here, which is the whole point of the first property: PAPPL
        # writes the page and then segfaults in the footer, so the status line
        # is 200 and the response is never finished.
        return None, f"{type(error).__name__}: {error}"


def died(server):
    """How the server exited, or None if it is still running."""
    code = server.poll()
    if code is None:
        return None
    return f"signal {-code}" if code < 0 else f"exit status {code}"


def start(application, port, work, log):
    """A real server: no `--probe-output`, so PAPPL persists its state."""
    server = subprocess.Popen(
        [str(application), "--listen-port", str(port),
         "--spool-directory", str(work / "spool"), "server"],
        stdout=log, stderr=log,
        env=dict(os.environ, XDG_CONFIG_HOME=str(work), TMPDIR=str(work)))
    for _ in range(200):
        if server.poll() is not None:
            raise RuntimeError("the server exited during startup; see the log")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=.1):
                return server
        except OSError:
            time.sleep(.05)
    server.kill()
    raise RuntimeError("the server did not open its loopback listener")


def stop(application, server, port, work):
    subprocess.run([str(application), "-u", f"ipp://127.0.0.1:{port}/", "shutdown"],
                   capture_output=True, timeout=60,
                   env=dict(os.environ, XDG_CONFIG_HOME=str(work), TMPDIR=str(work)))
    try:
        server.wait(timeout=30)
    except subprocess.TimeoutExpired:
        server.terminate()
        try:
            server.wait(timeout=15)
        except subprocess.TimeoutExpired:
            server.kill()


def client(application, port, work, *args):
    return subprocess.run(
        [str(application), "-u", f"ipp://127.0.0.1:{port}/", *args],
        capture_output=True, text=True, timeout=120,
        env=dict(os.environ, XDG_CONFIG_HOME=str(work), TMPDIR=str(work)))


def advertised(port, window=10):
    """Resolved records for this test only; None means browsing unavailable."""
    deadline = time.monotonic() + window
    while True:
        try:
            found = subprocess.run(["avahi-browse", "-rtpk", "_ipp._tcp"], capture_output=True,
                                   text=True, timeout=20)
        except FileNotFoundError:
            return None
        except subprocess.TimeoutExpired:
            return None
        if found.returncode != 0:
            return None
        records = [line for line in found.stdout.splitlines()
                   if line.startswith("=;") and f";{port};" in line
                   and "_ipp._tcp" in line and f"rp=ipp/print/{PRINTER}" in line]
        if records or time.monotonic() >= deadline:
            return records


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8633)
    parser.add_argument("--application", type=Path,
                        default=ROOT / "target/debug/ml216x-printer-app",
                        help="the binary to drive; another release is an injection")
    args = parser.parse_args()
    if not args.application.exists():
        raise SystemExit(f"build first: cargo build -p ml216x-printer-app "
                         f"(no {args.application})")

    failures = []
    known = []
    with tempfile.TemporaryDirectory(prefix="ml216x-server-") as temp:
        work = Path(temp)
        (work / "spool").mkdir(mode=0o700)
        first = (work / "first.log").open("w+")
        second = (work / "second.log").open("w+")
        state = state_path(args.application, work)
        # A root run shares `/var/lib` with whatever is installed there, so the
        # run starts from no file of its own and puts back the one it found.
        backup = state.with_name(state.name + ".probe-backup")
        if state.exists():
            shutil.copy2(state, backup)
            state.unlink()
        try:
            run(args, work, state, first, second, failures, known)
        finally:
            if state.exists():
                state.unlink()
            if backup.exists():
                backup.replace(state)

    for note in known:
        print(f"KNOWN {note}")
    if failures:
        raise SystemExit("FAIL:\n  " + "\n  ".join(failures))
    print(f"PASS server: {len(PAGES)} page(s) served with the server still running, "
          f"a printer restored from state, and DNS-SD registration requested")


def run(args, work, state, first, second, failures, known):
    with_state = f"Loading system state from '{state}'"

    # Pass one exists to put a printer in the state file. Adding one over
    # IPP does not advertise it — PAPPL registers a printer when a server
    # starts with it already in state, which is why a restart is the only
    # scenario that tests the decision.
    server = start(args.application, args.port, work, first)
    try:
        added = client(args.application, args.port, work, "add", "-d", PRINTER,
                       "-m", "samsung_ml216x", "-v", "socket://127.0.0.1:9100")
        if added.returncode != 0:
            failures.append(f"the printer could not be added: {added.stderr.strip()}")
    finally:
        stop(args.application, server, args.port, work)

    if not state.exists():
        failures.append(
            f"no state file at {state}: either nothing was saved or the path "
            f"this application computes is not the one PAPPL's mainloop used, "
            f"which loses the printers of an existing installation")

    # Pass two is the one that matters: the printer is in the file when the
    # server starts, so this is where PAPPL would advertise it.
    server = start(args.application, args.port, work, second)
    try:
        listed = client(args.application, args.port, work, "printers")
        if PRINTER not in listed.stdout:
            failures.append(
                f"the printer did not survive the restart; `printers` said "
                f"{listed.stdout.strip()!r}")

        if with_state not in (work / "second.log").read_text():
            failures.append(
                f"the restarted server did not report loading {state}; the path "
                f"it computes has drifted from PAPPL's mainloop, which loses "
                f"the printers of an existing installation")

        registrations = [line for line in (work / "second.log").read_text().splitlines()
                         if re.search(r"Registering DNS-SD name", line) and f"[Printer {PRINTER}]" in line]
        if not registrations:
            failures.append("PAPPL did not attempt DNS-SD registration after restore")
        # Linux exposes the actual listening address independently of the log.
        # A loopback-only listener cannot honour a network announcement.
        listeners = []
        for table in ("/proc/net/tcp", "/proc/net/tcp6"):
            if Path(table).exists():
                for line in Path(table).read_text().splitlines()[1:]:
                    fields = line.split()
                    address, port = fields[1].split(":")
                    if int(port, 16) == args.port and fields[3] == "0A":
                        listeners.append(address)
        if not any(set(address) == {"0"} for address in listeners):
            failures.append(f"no wildcard IPP listener on port {args.port}: {listeners}")
        on_the_network = advertised(args.port)
        if on_the_network is None:
            known.append("Avahi browsing unavailable; DNS-SD registration was checked in the log only")
        elif not on_the_network:
            failures.append("Avahi is available but no resolved IPP record names this printer and port")
        if on_the_network:
            print(f"PASS DNS-SD: {len(on_the_network)} resolved record(s) for printer {PRINTER} on port {args.port}")
            addresses = [record.split(";")[7] for record in on_the_network
                         if record.split(";")[2] == "IPv4"
                         and not record.split(";")[7].startswith("127.")]
            if addresses:
                connection = http.client.HTTPConnection(addresses[0], args.port, timeout=5)
                try:
                    connection.request("GET", "/")
                    response = connection.getresponse()
                    response.read()
                    if response.status != 200:
                        failures.append(f"network address HTTP status: {response.status}")
                    else:
                        print("PASS network HTTP: advertised non-loopback address served the web interface")
                except (OSError, http.client.HTTPException) as error:
                    failures.append(f"advertised network address is unreachable: {error}")
                finally:
                    connection.close()
            else:
                known.append("no non-loopback IPv4 announcement to test direct network HTTP")
        # The pages come last: one of them can end the process, and the
        # question above is about what the server did when it started.
        for page in PAGES:
            status, reason = fetch(args.port, page, timeout=30)
            gone = died(server)
            if gone:
                failures.append(
                    f"{page}: the server died of {gone} while serving it")
                break
            if status != 200:
                failures.append(f"{page}: {reason or f'HTTP {status}'}")

        if died(server) is None:
            if not Path(SYSTEM_BUS).exists():
                known.append(
                    f"{DEVICE_PAGE} was not fetched: there is no system bus at "
                    f"{SYSTEM_BUS}, and PAPPL aborts on a null DNS-SD client "
                    f"while listing devices (upstream S-6)")
            else:
                for attempt in (1, 2):
                    # Twice on purpose. PAPPL returns from a failed device
                    # browse still holding the Avahi lock, so where browsing
                    # cannot work the second view never completes (upstream
                    # S-7). That is a hang, not a crash, and not ours to fix.
                    status, reason = fetch(args.port, DEVICE_PAGE, timeout=40)
                    gone = died(server)
                    if gone:
                        failures.append(
                            f"{DEVICE_PAGE}: the server died of {gone} on view "
                            f"{attempt}")
                        break
                    if status != 200:
                        known.append(
                            f"{DEVICE_PAGE} view {attempt}: "
                            f"{reason or f'HTTP {status}'} — DNS-SD browsing is "
                            f"failing in this environment (upstream S-7)")
                        break

    finally:
        stop(args.application, server, args.port, work)


main()
