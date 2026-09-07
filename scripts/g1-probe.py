#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""The software half of release gate G-1: prove the measurement page survives.

G-1 is a physical measurement (`docs/GOLDEN-VALIDATION.md` section 4), and
`docs/G1-MEASUREMENT.md` is the runbook for taking it. This script is what has
to pass before anyone spends paper: it prints `scripts/g1-page.c`'s ruler page
through the real printer application to a `file://` device, decodes the QPDL
back into a bitmap with the engine's own decompressor
(`crates/spl2-core/examples/qpdl-decode.rs`), and checks that every ruler tick,
every corner bracket and the calibration span landed on the pixel the driver's
geometry predicts.

What that leaves for the paper is exactly one unknown: where the engine's first
printable pixel physically sits. That is the number G-1 measures, and no byte
comparison can supply it.

The mapping being asserted, from `crates/ml216x-printer-app/src/driver.rs` and
`crates/spl2-core/src/engine.rs`:

    page column = raster column - hard_margin_bytes(12.5 pt, xdpi) * 8
    page row    = raster row    - hard_margin_lines(12.5 pt, ydpi)

The column form holds because PAPPL delivers a sheet-wide line, so
`band_placement`'s centring term is zero and `src_skip` is the whole hard
margin. If the media table ever made the band narrower or wider than the
incoming line that stops being true, and this harness fails rather than
quietly agreeing with a different rule.

`--inject` corrupts the decoded page so the checks must fail; a harness that
has never gone red is not evidence.

Requires: cargo, cc, libcups2-dev, ipptool.
XDG_CONFIG_HOME and TMPDIR are scoped to a temporary directory, so a run leaves
nothing in the user's own PAPPL state.
"""
import argparse
import http.client
import json
import os
import socket
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# One tick per millimetre is drawn; a tick centre must land within this many
# pixels of its prediction. One pixel is the rounding the placement itself
# carries; anything larger is a geometry change.
TOLERANCE_PX = 1


def run(args, env=None, check=True):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True,
                            timeout=300, env=env)
    if check and result.returncode:
        raise RuntimeError(f"{' '.join(map(str, args))}\n{result.stdout}\n{result.stderr}")
    return result


class Bitmap:
    """A decoded page: `pixels[y][x]` is 1 where there is toner."""

    def __init__(self, path):
        data = path.read_bytes()
        magic, dimensions, raster = data.split(b"\n", 2)
        assert magic == b"P4", magic
        self.width, self.height = (int(v) for v in dimensions.split())
        stride = (self.width + 7) // 8
        self.rows = [raster[y * stride:(y + 1) * stride] for y in range(self.height)]

    def get(self, x, y):
        if not (0 <= x < self.width and 0 <= y < self.height):
            return 0
        return (self.rows[y][x // 8] >> (7 - x % 8)) & 1

    def runs_down(self, x):
        """The [start, end] row ranges of ink in column `x`."""
        return self._runs([self.get(x, y) for y in range(self.height)])

    def runs_across(self, y):
        """The [start, end] column ranges of ink in row `y`."""
        return self._runs([self.get(x, y) for x in range(self.width)])

    @staticmethod
    def _runs(line):
        found, start = [], None
        for index, value in enumerate(line + [0]):
            if value and start is None:
                start = index
            elif not value and start is not None:
                found.append((start, index - 1))
                start = None
        return found

    def shift(self, by):
        """Displace the page horizontally: an R-1 margin error."""
        stride = (self.width + 7) // 8
        for y in range(self.height):
            bits = int.from_bytes(self.rows[y], "big") >> by
            self.rows[y] = bits.to_bytes(stride + 1, "big")[1:]

    def crop(self, lines):
        """Lose the top of the page: a Q-13 vertical-crop error."""
        stride = (self.width + 7) // 8
        for y in range(min(lines, self.height)):
            self.rows[y] = bytes(stride)

    def compress(self, every):
        """Drop every nth column: a scale or resolution error (R-4)."""
        stride = (self.width + 7) // 8
        for y in range(self.height):
            kept = [self.get(x, y) for x in range(self.width) if x % every]
            kept += [0] * (self.width - len(kept))
            packed = bytearray(stride)
            for x, value in enumerate(kept):
                if value:
                    packed[x // 8] |= 0x80 >> (x % 8)
            self.rows[y] = bytes(packed)


def centres(runs):
    return [(start + end) / 2 for start, end in runs]


def check(page, drawing, failures):
    """Every feature the generator declared, against the page the driver made."""
    drop_x, drop_y = drawing["drop_columns"], drawing["drop_lines"]

    if page.height != drawing["printable_lines"]:
        failures.append(f"page is {page.height} lines, the driver's crop predicts "
                        f"{drawing['printable_lines']}")
    # The mapping this harness asserts holds only while `band_placement`'s
    # centring term is zero, which is what a sheet-wide line produces. The band
    # is derived from the legacy PPD's integer points and the incoming raster
    # from the PWG size, so the two differ by up to a byte either way; that is
    # fine, two whole bytes of difference would not be. Same formula as
    # `crates/spl2-core/src/geometry.rs`.
    band_bytes = page.width // 8
    cups_bytes = drawing["raster"]["bytes_per_line"]
    centred = max(0, band_bytes - cups_bytes) // 2
    if centred:
        failures.append(
            f"the band is {band_bytes} B against a {cups_bytes} B line, so "
            f"`band_placement` centres the content by {centred} B; the raster-to-page "
            f"mapping this harness asserts (and the runbook's predictions) assume no "
            f"centring")
        return
    if band_bytes < cups_bytes:
        # The rightmost byte of every line is clipped. It carries no tick, but
        # say so rather than let it look like a missing feature.
        print(f"     note: the band is {band_bytes} B against a {cups_bytes} B line, "
              f"so the last {8 * (cups_bytes - band_bytes)} raster columns are clipped "
              f"on the right.")

    for ruler in drawing["rulers"]:
        vertical = ruler["axis"] == "y"
        # Along the measured axis the ticks march; across it the scan line
        # crosses them. Both are the generator's own coordinates, moved into
        # the page by the driver's transform and nothing else.
        if vertical:
            found = page.runs_down(ruler["scan"] - drop_x)
            window = [ruler["window"][0] - drop_y, ruler["window"][1] - drop_y]
            predicted = [(t, t["raster"] - drop_y) for t in ruler["ticks"]
                         if t["in_stream"] and 0 <= t["raster"] - drop_y < page.height]
        else:
            found = page.runs_across(ruler["scan"] - drop_y)
            window = [ruler["window"][0] - drop_x, ruler["window"][1] - drop_x]
            predicted = [(t, t["raster"] - drop_x) for t in ruler["ticks"]
                         if t["in_stream"] and 0 <= t["raster"] - drop_x < page.width]
        # A tick is `thickness` pixels wide from its position, so its run is
        # centred half a thickness past it.
        offset = (ruler["thickness"] - 1) / 2
        inside = [c for c in centres(found) if window[0] <= c <= window[1]]
        predicted.sort(key=lambda pair: pair[1])
        if len(inside) != len(predicted):
            dropped = sum(1 for t in ruler["ticks"] if not t["in_stream"])
            failures.append(
                f"{ruler['name']} ruler: {len(inside)} ticks reached the page, "
                f"{len(predicted)} were predicted, {dropped} more predicted to be "
                f"dropped inside the margin")
            continue
        for (tick, at), centre in zip(predicted, inside):
            if abs(centre - at - offset) > TOLERANCE_PX:
                failures.append(
                    f"{ruler['name']} ruler: the {tick['mm']} mm tick is centred at "
                    f"{centre}, predicted {at + offset}")
        spacing = [b - a for a, b in zip(inside, inside[1:])]
        nominal = drawing["resolution"][1 if vertical else 0] / 25.4
        worst = max((abs(s - nominal) for s in spacing), default=0)
        if worst > TOLERANCE_PX:
            failures.append(
                f"{ruler['name']} ruler: tick spacing is out by {worst:.2f} px; one "
                f"millimetre at {drawing['resolution']} dpi is {nominal:.2f} px")

    # The corner brackets: ink at the outer corner of each.
    for bracket in drawing["brackets"]:
        x, y = bracket["raster"][0] - drop_x, bracket["raster"][1] - drop_y
        if not page.get(x, y):
            failures.append(f"{bracket['corner']} bracket: no ink at its outer corner "
                            f"({x}, {y})")
    # The top-left bracket is the first pixel the driver emits at all, which is
    # the whole basis of the measurement: if it moves, the mapping has changed.
    top_left = next(b for b in drawing["brackets"] if b["corner"] == "top-left")
    if (top_left["raster"][0] - drop_x, top_left["raster"][1] - drop_y) != (0, 0):
        failures.append("the top-left bracket is no longer the page's first pixel; "
                        "the margin mapping this harness asserts has changed")

    # The calibration cross, which separates a scale error from an offset.
    calibration = drawing["calibration"]
    if calibration["span_hmm"]:
        for axis, name in ((0, "horizontal"), (1, "vertical")):
            bar = calibration[name]
            if axis == 0:
                runs = page.runs_across(bar["scan"] - drop_y)
                expected = (bar["extent"][0] - drop_x, bar["extent"][1] - drop_x)
            else:
                runs = page.runs_down(bar["scan"] - drop_x)
                expected = (bar["extent"][0] - drop_y, bar["extent"][1] - drop_y)
            spanning = [r for r in runs if r[1] - r[0] > (expected[1] - expected[0]) / 2]
            if len(spanning) != 1:
                failures.append(
                    f"{name} calibration span: {len(spanning)} bars found, expected 1")
                continue
            start, end = spanning[0]
            if abs(start - expected[0]) > TOLERANCE_PX or abs(end - expected[1]) > TOLERANCE_PX:
                failures.append(
                    f"{name} calibration span runs {start}..{end}, predicted "
                    f"{expected[0]}..{expected[1]} "
                    f"({calibration['span_hmm'] / 100:.0f} mm at "
                    f"{drawing['resolution'][axis]} dpi)")


class Server:
    """The printer application, serving one `file://` printer named `probe`.

    One server for the whole run: starting PAPPL 44 times to print 44 pages
    told us nothing the first start did not.
    """

    def __init__(self, application, port, tmp, environment):
        self.port = port
        self.tmp = tmp
        self.environment = environment
        self.output = tmp / "g1.spl"
        self.log = (tmp / "server.log").open("w+")
        self.process = subprocess.Popen([
            str(application), "--probe-output", str(self.output),
            "--listen-port", str(port), "--spool-directory", str(tmp / "spool"),
            "server"], stdout=self.log, stderr=self.log, env=environment)
        for _ in range(200):
            if self.process.poll() is not None:
                self.log.seek(0)
                raise RuntimeError(f"PAPPL server exited during startup\n{self.log.read()}")
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=.1):
                    return
            except OSError:
                time.sleep(.05)
        raise RuntimeError("PAPPL did not open its loopback listener")

    def uri(self):
        return f"ipp://127.0.0.1:{self.port}/ipp/print/probe"

    def attributes(self):
        """What the application publishes: the case list comes from here rather
        than from a second copy of `media_table`, so a medium this harness
        never exercised cannot hide behind an out-of-date list.

        Asked with a hand-built IPP request rather than through `ipptool`.
        `ipptool -t` fails every Get-Printer-Attributes against this printer,
        reporting `document-format-supported`'s `application/octet-stream` as
        having "bad characters (RFC 8011 section 5.1.10)". That report is
        wrong: the server answers `successful-ok`, the value on the wire is 24
        clean bytes, and libcups' own `ippValidateAttributes` accepts the whole
        response. Print-Job, which the rest of this file and the other probes
        use, is unaffected. See `docs/G1-MEASUREMENT.md`.
        """
        request = bytearray(b"\x02\x00\x00\x0b\x00\x00\x00\x01\x01")

        def add(tag, name, value):
            request.append(tag)
            request.extend(len(name).to_bytes(2, "big") + name)
            request.extend(len(value).to_bytes(2, "big") + value)

        add(0x47, b"attributes-charset", b"utf-8")
        add(0x48, b"attributes-natural-language", b"en")
        add(0x45, b"printer-uri", self.uri().encode())
        request.append(0x03)

        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        connection.request("POST", "/ipp/print/probe", bytes(request),
                           {"Content-Type": "application/ipp"})
        response = connection.getresponse().read()
        status = int.from_bytes(response[2:4], "big")
        if status:
            raise RuntimeError(f"Get-Printer-Attributes returned status {status:#06x}")

        # Enough of the IPP encoding to read two attributes: tag, name, value,
        # and a zero-length name meaning "another value of the last attribute".
        media, resolutions, name = [], [], None
        at = 8
        while at < len(response):
            tag = response[at]
            at += 1
            if tag < 0x10:
                if tag == 0x03:
                    break
                continue
            name_length = int.from_bytes(response[at:at + 2], "big")
            at += 2
            if name_length:
                name = response[at:at + name_length].decode()
                at += name_length
            value_length = int.from_bytes(response[at:at + 2], "big")
            at += 2
            value = response[at:at + value_length]
            at += value_length
            if name == "media-supported":
                media.append(value.decode())
            elif name == "printer-resolution-supported" and value_length == 9:
                # Cross feed, feed, then the units byte; 3 is dots per inch.
                x = int.from_bytes(value[0:4], "big")
                y = int.from_bytes(value[4:8], "big")
                if value[8] != 3:
                    raise RuntimeError(f"resolution {x}x{y} is not in dots per inch")
                resolutions.append(f"{x}x{y}")
        if not media or not resolutions:
            raise RuntimeError("the printer published no media or no resolutions")
        return media, resolutions

    def print_page(self, raster, media, xdpi, ydpi):
        """Submit one page and return the QPDL the device received."""
        self.output.write_bytes(b"")
        test = self.tmp / "print.test"
        test.write_text(f'''{{
NAME "G-1 measurement page {media} {xdpi}x{ydpi}"
OPERATION Print-Job
GROUP operation-attributes-tag
ATTR charset attributes-charset utf-8
ATTR naturalLanguage attributes-natural-language en
ATTR uri printer-uri $uri
ATTR name requesting-user-name g1-probe
ATTR mimeMediaType document-format image/pwg-raster
GROUP job-attributes-tag
ATTR boolean ipp-attribute-fidelity true
ATTR keyword media {media}
ATTR resolution printer-resolution {xdpi}x{ydpi}dpi
FILE {raster}
STATUS successful-ok
}}
''')
        run(["ipptool", "-t", self.uri(), str(test)], env=self.environment)
        for _ in range(200):
            if self.output.exists() and self.output.stat().st_size:
                # The stream is complete once it carries its closing UEL.
                stream = self.output.read_bytes()
                if stream.endswith(b"\t\x1b%-12345X"):
                    return stream
            time.sleep(.1)
        self.log.seek(0)
        raise RuntimeError(f"no complete stream reached the device\n{self.log.read()}")

    def stop(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--media", default="iso_a4_210x297mm")
    parser.add_argument("--resolution", default="600x600")
    parser.add_argument("--all", action="store_true",
                        help="every medium and resolution the application publishes")
    parser.add_argument("--port", type=int, default=8633)
    parser.add_argument("--keep", type=Path,
                        help="also write the page and its decode here, to print by hand")
    parser.add_argument("--inject", choices=["shift", "crop", "scale"],
                        help="corrupt the decoded page, so the harness must fail")
    arguments = parser.parse_args()

    application = ROOT / "target/debug/ml216x-printer-app"
    if not application.exists():
        raise SystemExit("build first: cargo build -p ml216x-printer-app")
    run(["cargo", "build", "-p", "spl2-core", "--features", "golden-replay",
         "--example", "qpdl-decode"])
    decoder = ROOT / "target/debug/examples/qpdl-decode"

    failures, passed = [], 0
    with tempfile.TemporaryDirectory(prefix="ml216x-g1-") as temporary:
        tmp = Path(temporary)
        environment = dict(os.environ, XDG_CONFIG_HOME=str(tmp), TMPDIR=str(tmp))
        generator = tmp / "g1-page"
        run(["cc", "-Wall", "-Wextra", "-Werror", ROOT / "scripts/g1-page.c",
             "-lcups", "-lm", "-o", generator])
        server = Server(application, arguments.port, tmp, environment)
        try:
            if arguments.all:
                media, resolutions = server.attributes()
                cases = [(m, r) for m in media for r in resolutions]
            else:
                cases = [(arguments.media, arguments.resolution)]

            for medium, resolution in cases:
                xdpi, ydpi = (int(v) for v in resolution.split("x"))
                raster = tmp / "g1.pwg"
                drawing = json.loads(
                    run([generator, raster, medium, xdpi, ydpi]).stdout)
                stream = server.print_page(raster, medium, xdpi, ydpi)
                decoded = json.loads(run([decoder, server.output, tmp / "pages"]).stdout)
                if len(decoded["pages"]) != 1:
                    failures.append(f"{medium} {resolution}: the job produced "
                                    f"{len(decoded['pages'])} pages")
                    continue
                page = Bitmap(tmp / "pages" / decoded["pages"][0]["pbm"])

                if arguments.inject == "shift":
                    page.shift(3)
                elif arguments.inject == "crop":
                    page.crop(60)
                elif arguments.inject == "scale":
                    page.compress(50)

                case_failures = []
                check(page, drawing, case_failures)
                if case_failures:
                    failures += [f"{medium} {resolution}: {f}" for f in case_failures]
                    continue
                passed += 1
                ticks = sum(1 for r in drawing["rulers"]
                            for t in r["ticks"] if t["in_stream"])
                print(f"PASS {medium} {resolution}: {len(stream)} bytes, {ticks} ticks "
                      f"and 4 brackets on the pixel the geometry predicts; the "
                      f"{drawing['calibration']['span_hmm'] // 100} mm span is exact.",
                      flush=True)

                if arguments.keep:
                    arguments.keep.mkdir(parents=True, exist_ok=True)
                    stem = f"g1-{medium}-{xdpi}x{ydpi}"
                    (arguments.keep / f"{stem}.pwg").write_bytes(raster.read_bytes())
                    (arguments.keep / f"{stem}.spl").write_bytes(stream)
                    (arguments.keep / f"{stem}.json").write_text(
                        json.dumps(drawing, indent=2))
        finally:
            server.stop()

    if arguments.inject:
        if failures:
            print(f"PASS --inject {arguments.inject}: the harness reported "
                  f"{len(failures)} failure(s), first: {failures[0]}")
            return
        raise SystemExit(f"FAIL: --inject {arguments.inject} was not detected")
    if failures:
        raise SystemExit(f"FAIL ({passed} case(s) passed):\n  " + "\n  ".join(failures))
    print(f"PASS G-1 page: {passed} case(s). Predicted on paper: the top-left "
          f"bracket sits 4.41 mm from each edge, and the sheet content 0.33 mm at "
          f"600 dpi left of its nominal position, because the horizontal margin "
          f"rounds up to a whole byte column. Measuring that is gate G-1; see "
          f"docs/G1-MEASUREMENT.md.")


main()
