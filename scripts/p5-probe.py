#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Run real PWG Raster through PAPPL over loopback; no hardware is used.
Requires: cargo build -p ml216x-printer-app, cc, libcups2-dev, ipptool.
The server is always stopped; all raw input/output stays in a temporary directory.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
MEDIA = ["iso_a4_210x297mm", "na_letter_8.5x11in", "na_legal_8.5x14in",
         "na_executive_7.25x10.5in", "iso_a5_148x210mm", "iso_a6_105x148mm",
         "jis_b5_182x257mm", "na_number-10_4.125x9.5in", "iso_dl_110x220mm",
         "iso_c5_162x229mm", "om_folio_210x330mm"]
RESOLUTIONS = [(300, 300), (600, 600), (1200, 600), (1200, 1200)]


def run(args):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=30)
    if result.returncode:
        raise RuntimeError(f"{' '.join(map(str, args))}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--port", type=int, default=18631)
    parser.add_argument("--device-failure", action="store_true", help="use /dev/full and require job-state=aborted")
    args = parser.parse_args()
    uri = f"ipp://127.0.0.1:{args.port}/ipp/print/probe"
    results = []
    with tempfile.TemporaryDirectory(prefix="ml216x-p5-") as temp:
        tmp = Path(temp)
        generator = tmp / "input"
        sink = Path("/dev/full") if args.device_failure else tmp / "output.jsonl"
        run(["cc", "-Wall", "-Wextra", "-Werror", ROOT / "scripts/pwg-probe-input.c", "-lcups", "-o", generator])
        # PAPPL mainloop reads XDG_CONFIG_HOME for its state file. Keep the
        # environment override scoped to this subprocess, never the user's shell.
        environment = dict(os.environ, XDG_CONFIG_HOME=str(tmp), TMPDIR=str(tmp))
        with (tmp / "server.log").open("w+") as log:
            server = subprocess.Popen([str(ROOT / "target/debug/ml216x-printer-app"), "--probe",
                "--probe-output", str(sink), "--listen-port", str(args.port),
                "--spool-directory", str(tmp / "spool"), "server"], stdout=log, stderr=log, env=environment)
            try:
                for _ in range(100):
                    if server.poll() is not None:
                        raise RuntimeError("PAPPL server exited during startup")
                    try:
                        with socket.create_connection(("127.0.0.1", args.port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.05)
                else:
                    raise RuntimeError("PAPPL did not open its loopback listener")
                for medium in (MEDIA[:1] if args.device_failure else MEDIA):
                    for xdpi, ydpi in (RESOLUTIONS if medium in MEDIA[:2] and not args.device_failure else [(600, 600)]):
                        raster = tmp / "input.pwg"
                        generated = json.loads(run([generator, raster, medium, xdpi, ydpi]))
                        test = tmp / "print.test"
                        test.write_text(f'''{{
NAME "P5 {medium} {xdpi}x{ydpi}"
OPERATION Print-Job
GROUP operation-attributes-tag
ATTR charset attributes-charset utf-8
ATTR language attributes-natural-language en
ATTR uri printer-uri $uri
ATTR name requesting-user-name p5-probe
ATTR mimeMediaType document-format image/pwg-raster
ATTR boolean ipp-attribute-fidelity true
GROUP job-attributes-tag
ATTR integer copies 1
ATTR keyword media {medium}
ATTR resolution printer-resolution {xdpi}x{ydpi}dpi
FILE $filename
STATUS successful-ok
EXPECT job-state WITH-VALUE {8 if args.device_failure else 9}
}}
''')
                        offset = sink.stat().st_size if sink.exists() else 0
                        run(["ipptool", "-t", "-f", raster, uri, test])
                        if args.device_failure:
                            results.append({"device":"/dev/full", "job_state":"aborted"})
                            print("PASS device failure: job-state=aborted", flush=True)
                            continue
                        with sink.open() as f:
                            f.seek(offset)
                            events = [json.loads(line) for line in f]
                        page = next(e for e in events if e["event"] == "page-start")
                        lines = [e for e in events if e["event"] == "line"]
                        assert events[-1]["event"] == "job-end", events[-1]
                        assert page["dpi"] == [xdpi, ydpi], page
                        assert page["margin"] == 441 and page["header_margins"] == [0, 0], page
                        assert all(page[k] == generated[k] for k in ("width", "height", "bytes_per_line")), (page, generated)
                        assert [e["y"] for e in lines] == list(range(page["height"]))
                        mx, my = generated["inset"]
                        marks = [e for e in lines if e["ones"]]
                        assert [e["y"] for e in marks] == [0, my, page["height"] - 1 - my, page["height"] - 1], marks
                        assert all(e["ones"] == 2 for e in marks), marks
                        assert marks[0]["first_nonzero_byte"] == 0
                        assert marks[0]["last_nonzero_byte"] == (page["width"] - 1) // 8
                        assert marks[1]["first_nonzero_byte"] == mx // 8
                        assert marks[1]["last_nonzero_byte"] == (page["width"] - 1 - mx) // 8
                        results.append({"medium": medium, "input": generated, "callback": page, "line_count": len(lines), "marks": marks})
                        print(f"PASS {medium} {xdpi}x{ydpi}: {page['width']}x{page['height']}, full media", flush=True)
                args.output.write_text(json.dumps({"pappl":run(["pkg-config", "--modversion", "pappl"]).strip(), "cases":results}, indent=2) + "\n")
            except Exception:
                log.flush()
                log.seek(0)
                print(log.read()[-12000:])
                raise
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)


if __name__ == "__main__":
    main()
