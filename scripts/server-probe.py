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
2. **A printer restored from the state file is not advertised, and is still
   there** (decision Q-24). The application loads its own state file so that
   it can clear every DNS-SD name before PAPPL's registration loop runs. That
   makes two things checkable in one run: the printer added before the restart
   comes back, which is what proves the path this application computes is the
   path PAPPL's mainloop was using, and the restarted server logs no
   registration at all. Where `avahi-browse` is installed, the network is
   asked directly as well.

`--application` runs the checks against another binary, which is how the
harness was shown to go red rather than asserted to. Against both binaries in
`dist/` — 2.0.0~alpha-5 and 2.0.0~alpha-6, which both carry Q-23's mechanism —
property 1 fails on the first page, `/`, with the server dead of SIGSEGV, and
property 2 fails because those releases still ask PAPPL to register the
printer: they stopped the announcement one layer lower, by cutting the process
off from D-Bus, so the attempt failed instead of never being made. That is the
evidence an `--inject` mode provides for the other probes; here the previous
releases are the injection. What the network does when nothing stops the
announcement at all was measured separately and is recorded under Q-24 in
docs/DECISIONS.md.

One check is conditional and says so when it skips. The "Add Printer" page is
the only page that lists devices, and PAPPL hands its DNS-SD library a null
client when D-Bus cannot be reached at all, which aborts the process — an
upstream defect (S-6 in docs/DEBIAN-BUG-DRAFT.md) that this application cannot
prevent, since the page is PAPPL's own. In an environment with no system bus
the page is therefore not fetched, and the run says why.

Requires: cargo build -p ml216x-printer-app. Optional: avahi-browse.
XDG_CONFIG_HOME and TMPDIR are scoped to a temporary directory, so the state
file this script creates is its own and the developer's printers are never
touched.
"""
import argparse
import http.client
import os
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
    """A real server: no `--probe-output`, so it owns its state file (Q-24)."""
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


def advertised(window=10):
    """Records on the network naming this system or its printer, if askable.

    `None` when there is no `avahi-browse` to ask. Otherwise the browse is
    repeated for up to `window` seconds and returns as soon as anything shows
    up, because a service registered a moment ago takes a query and a response
    to become visible and a single immediate browse finds nothing either way.
    Proving absence over mDNS is a matter of how long you waited, which is why
    the assertion this probe fails on is the server's own log; this corroborates
    it from the other side.
    """
    deadline = time.monotonic() + window
    while True:
        try:
            found = subprocess.run(["avahi-browse", "-aptr"], capture_output=True,
                                   text=True, timeout=20)
        except FileNotFoundError:
            return None
        except subprocess.TimeoutExpired:
            found = None
        records = [line for line in (found.stdout.splitlines() if found else [])
                   if PRINTER in line or "ML-216x" in line]
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

        state = work / f"{args.application.name}.state"
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

            registrations = [line for line in (work / "second.log").read_text().splitlines()
                             if re.search(r"Registering DNS-SD name", line)]
            if registrations:
                # Not "was advertised": the log line is written before the
                # attempt, so this catches a server that asked PAPPL to
                # register and got away with it only because something else
                # was broken. Q-24 is that the name is gone and the attempt is
                # never made.
                failures.append(
                    f"the server asked PAPPL to register {len(registrations)} "
                    f"DNS-SD service(s) for a printer restored from state, which "
                    f"decision Q-24 declines: {registrations[0].strip()}")
            on_the_network = advertised()
            if on_the_network is None:
                known.append("avahi-browse is not installed, so the network itself "
                             "was not asked; the server's log was")
            elif on_the_network:
                failures.append(
                    f"{len(on_the_network)} record(s) naming this server are on the "
                    f"network: {on_the_network[0]}")
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

    for note in known:
        print(f"KNOWN {note}")
    if failures:
        raise SystemExit("FAIL:\n  " + "\n  ".join(failures))
    print(f"PASS server: {len(PAGES)} page(s) served with the server still running, "
          f"a printer restored from state, and nothing advertised")


main()
