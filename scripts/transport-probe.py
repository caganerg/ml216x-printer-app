#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Drive the printer application over a real socket device; no hardware is used.

Two properties are checked, both of which the goldens cannot reach because they
stop at the filter's stdout:

1. **The transport delivers what the driver wrote.** The same job is printed to
   a `file://` destination and to a `socket://` destination served by a loopback
   TCP sink here, and the two streams must be byte identical.
2. **A job that names no resolution runs at the one the printer declares.**
   PAPPL picks by print-quality from the position of the entry in the driver's
   resolution list and never consults the declared default, so a reordering of
   that list silently prints every ordinary job at the wrong scale (Q-14). Each
   quality is submitted with a document rendered at the resolution it should
   select, and the QPDL page header has to agree.

`--inject` exists so the harness can be shown to fail: it corrupts the socket
stream on the way in, and a run with it must report a mismatch. A harness that
has never gone red is not evidence.

Requires: cargo build -p ml216x-printer-app, cc, libcups2-dev, ipptool.
The server is always stopped, XDG_CONFIG_HOME is scoped to a temporary
directory so the user's own PAPPL state is untouched, and every byte stays in
that directory.
"""
import argparse
import json
import os
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
UEL = b"\x1b%-12345X"
MEDIUM = "iso_a4_210x297mm"
# What PAPPL selects for each print-quality, given the resolution list the
# application declares. See `pappl::application::quality_resolutions`.
QUALITY = {"draft": (3, (300, 300)), "normal": (4, (600, 600)), "high": (5, (1200, 1200))}


def run(args, env=None, check=True):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True,
                            timeout=120, env=env)
    if check and result.returncode:
        raise RuntimeError(f"{' '.join(map(str, args))}\n{result.stdout}\n{result.stderr}")
    return result


def page_resolution(stream):
    """The (x, y) dpi in the first QPDL page header of an SPL2 job."""
    assert stream.startswith(UEL), stream[:32]
    assert stream.endswith(b"\t" + UEL), stream[-32:]
    body = stream.index(b"@PJL ENTER LANGUAGE = QPDL")
    header = stream[stream.index(b"\n", body) + 1:][:17]
    assert len(header) == 17 and header[0] == 0, header
    return (header[16] * 100, header[1] * 100)


class Sink:
    """A loopback TCP device: one connection per job, everything kept."""

    def __init__(self, inject):
        self.inject = inject
        self.received = bytearray()
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.socket.bind(("127.0.0.1", 0))
        self.socket.listen(4)
        self.port = self.socket.getsockname()[1]
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        while True:
            try:
                connection, _ = self.socket.accept()
            except OSError:
                return
            with connection:
                connection.settimeout(60)
                while True:
                    try:
                        chunk = connection.recv(65536)
                    except OSError:
                        break
                    if not chunk:
                        break
                    self.received.extend(chunk)

    def take(self):
        """The job's bytes, with the fault injection applied."""
        data = bytes(self.received)
        self.received.clear()
        if self.inject == "truncate":
            return data[:-64]
        if self.inject == "flip" and data:
            middle = bytearray(data)
            middle[len(middle) // 2] ^= 0x01
            return bytes(middle)
        return data

    def close(self):
        self.socket.close()


def submit(app, port, printer, raster, quality, env, tmp):
    test = tmp / "print.test"
    test.write_text(f'''{{
NAME "transport {printer} {quality}"
OPERATION Print-Job
GROUP operation-attributes-tag
ATTR charset attributes-charset utf-8
ATTR naturalLanguage attributes-natural-language en
ATTR uri printer-uri $uri
ATTR name requesting-user-name tester
ATTR mimeMediaType document-format image/pwg-raster
GROUP job-attributes-tag
ATTR enum print-quality {QUALITY[quality][0]}
FILE {raster}
STATUS successful-ok
}}
''')
    run(["ipptool", "-t", f"ipp://127.0.0.1:{port}/ipp/print/{printer}", str(test)], env=env)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8632)
    parser.add_argument("--inject", choices=["truncate", "flip"],
                        help="corrupt the socket stream, so the harness must fail")
    args = parser.parse_args()

    application = ROOT / "target/debug/ml216x-printer-app"
    if not application.exists():
        raise SystemExit("build first: cargo build -p ml216x-printer-app")

    failures = []
    with tempfile.TemporaryDirectory(prefix="ml216x-transport-") as temp:
        tmp = Path(temp)
        environment = dict(os.environ, XDG_CONFIG_HOME=str(tmp), TMPDIR=str(tmp))
        generator = tmp / "input"
        run(["cc", "-Wall", "-Wextra", "-Werror", ROOT / "scripts/pwg-probe-input.c",
             "-lcups", "-o", generator])
        sink = Sink(args.inject)
        output = tmp / "file.spl"
        log = (tmp / "server.log").open("w+")
        server = subprocess.Popen([
            str(application), "--probe-output", str(output),
            "--listen-port", str(args.port), "--spool-directory", str(tmp / "spool"),
            "server"], stdout=log, stderr=log, env=environment)
        try:
            for _ in range(200):
                if server.poll() is not None:
                    raise RuntimeError("PAPPL server exited during startup")
                try:
                    with socket.create_connection(("127.0.0.1", args.port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.05)
            else:
                raise RuntimeError("PAPPL did not open its loopback listener")
            run([application, "-u", f"ipp://127.0.0.1:{args.port}/", "add", "-d", "sock",
                 "-m", "samsung_ml216x", "-v", f"socket://127.0.0.1:{sink.port}"],
                env=environment)

            for quality, (_, resolution) in QUALITY.items():
                raster = tmp / f"input-{quality}.pwg"
                json.loads(run([generator, raster, MEDIUM, *resolution]).stdout)

                output.write_bytes(b"")
                submit(application, args.port, "probe", raster, quality, environment, tmp)
                time.sleep(1)
                by_file = output.read_bytes()

                submit(application, args.port, "sock", raster, quality, environment, tmp)
                time.sleep(1)
                by_socket = sink.take()

                for name, stream in (("file", by_file), ("socket", by_socket)):
                    if not stream:
                        failures.append(f"{quality}: nothing reached the {name} device")
                        continue
                    try:
                        found = page_resolution(stream)
                    except AssertionError as error:
                        failures.append(f"{quality}: unparsable {name} stream: {error}")
                        continue
                    if found != resolution:
                        failures.append(
                            f"{quality}: {name} ran at {found[0]}x{found[1]} dpi, "
                            f"expected {resolution[0]}x{resolution[1]}")
                if by_file and by_socket and by_file != by_socket:
                    failures.append(
                        f"{quality}: the socket received {len(by_socket)} bytes, the file "
                        f"holds {len(by_file)}; the two streams differ")
                if not failures:
                    print(f"PASS {quality}: {len(by_file)} bytes, "
                          f"{resolution[0]}x{resolution[1]} dpi, file == socket")
        finally:
            server.terminate()
            try:
                server.wait(timeout=15)
            except subprocess.TimeoutExpired:
                server.kill()
            sink.close()

    if args.inject:
        # The injection has to be what fails, and it has to fail.
        if failures:
            print(f"PASS --inject {args.inject}: the harness reported "
                  f"{len(failures)} mismatch(es), first: {failures[0]}")
            return
        raise SystemExit(f"FAIL: --inject {args.inject} was not detected")
    if failures:
        raise SystemExit("FAIL:\n  " + "\n  ".join(failures))
    print("PASS transport: every quality delivered identical bytes over both devices")


main()
