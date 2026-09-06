# Security review — 2.0 printer application

This is the P9 security review. It covers what the printer application exposes
by running on a network-capable framework, and it settles the dithering-path
question the Q-1 follow-up in `docs/DECISIONS.md` deferred to this step.

Every finding below was reproduced against the code and the libraries this
project actually builds against — `ml216x-printer-app` on Debian trixie,
`libpappl1t64` 1.3.1-2.1+b2, `libcups2t64` 2.4.10 — not inferred from the
changelog. The reproductions ran on loopback with `XDG_CONFIG_HOME` scoped to a
temporary directory, so nothing touched the developer's own state.

## Summary

| # | Where | Class | Reachable in our config | Fix is ours? |
|---|---|---|---|---|
| S-1 | libpappl `job-process.c` | out-of-bounds write while dithering 8-bit input | yes, over IPP | no |
| S-2 | libpappl `printer-ipp.c` | stack overflow on oversized `media-ready` | yes, over IPP | no |
| S-3 | our systemd unit | the service runs as root | n/a | yes, deferred |

S-1 and S-2 are the two unpatched upstream fixes the Q-1 follow-up flagged
(`4587888f50` and `44327aaac3`). Both are **confirmed present in 1.3.1** and
both **crash the running server**. Neither has a fix this project can make: the
faults are inside libpappl, before any callback this project registers. The
containment that makes them a local denial of service rather than worse is that
the server listens on the loopback address only.

## S-1 — dithering an 8-bit raster wider than the page overflows the output line

**The hoped-for answer was wrong.** The Q-1 follow-up asked whether declaring
only 1-bit `BLACK_1` output makes PAPPL's dithering path unreachable. It does
not. `force_raster_type = PAPPL_PWG_RASTER_TYPE_BLACK_1` forces the *output* to
1-bit, but the *input* can still be 8-bit: a client may send `image/pwg-raster`
with `cupsBitsPerPixel = 8`, or an `image/jpeg` / `image/png` file that PAPPL's
own filters convert to 8-bit and then dither down. Forcing `BLACK_1` is
precisely what selects the dithering path, not what avoids it.

`pappl/job-process.c` (`_papplJobProcessRaster`) allocates the output line from
the **job's** header and bounds the dither loop by the **document's** header:

```c
if ((line = malloc(options->header.cupsBytesPerLine)) == NULL)   // page width: 620 B for A4@600
...
for (x = 0, lineptr = line, ...; x < header.cupsWidth; x ++, pixptr ++)   // document width: unbounded
{
    if (*pixptr > dither[x & 15])
        byte |= bit;
    if (bit == 1) { *lineptr++ = byte; ... }   // writes past `line` once x/8 >= 620
}
```

(`job-process.c:696` and `:718`/`:739`; `job-filter.c:412` is the same pattern
for the image filters.) When the incoming raster is wider than the printer
page, `lineptr` walks off the end of `line`. This project's own page-header
validation cannot prevent it, because the overflow is in libpappl and runs
before `rwriteline_cb` is ever called.

**Reproduced.** An 8-bit grayscale PWG raster 40000 px wide (the A4 page is
4960 px at 600 dpi) submitted with `ipptool`:

```
input: {"width":40000,"height":64,"bytes_per_line":40000,"bpp":8}
    wide 8-bit grayscale                                                 [FAIL]
server exit code: -6            # SIGABRT: "corrupted size vs. prev_size"
```

A raster that fits the page (1000 px, and even 5000 px against the 4960 px
page) is dithered without incident and the server survives, which confirms the
width mismatch is the trigger and not the 8-bit path itself.

## S-2 — an oversized `media-ready` list overflows a stack buffer

`pappl/printer-ipp.c` (`_papplPrinterSetAttributes`) copies a client-supplied
`media-ready` list into `driver_data.media_ready`, which is
`pappl_media_col_t[PAPPL_MAX_SOURCE]` — 16 entries — inside a
`pappl_pr_driver_data_t driver_data;` declared **on the stack** (`:911`). The
preflight validates the count against `PAPPL_MAX_SOURCE` only to set the
response's *unsupported* status; the copy loop then runs regardless of that
result:

```c
count = ippGetCount(rattr);
for (i = 0; i < count; i ++)                        // count is attacker-controlled
    _papplMediaColImport(ippGetCollection(rattr, i), driver_data.media_ready + i);
```

(`printer-ipp.c:1045` for `media-col-ready`, `:1068` for `media-ready`.) Each
entry is 228 bytes, and the whole `driver_data` struct is 8728 bytes, so about
20 entries fit inside the struct before the copy walks into the rest of the
stack frame. Beyond that it is a classic stack smash.

**Reproduced.** `Set-Printer-Attributes` with 512 `media-ready` values:

```
    512 media-ready values                                               [FAIL]
server exit code: -11           # SIGSEGV
```

Counts up to 32 did not crash in testing, which is the dangerous part: a count
in the low twenties overwrites adjacent stack **without** an immediate crash,
so a carefully sized request is silent corruption rather than a clean abort.
`Set-Printer-Attributes` is accepted with no authentication service configured,
so any client that can reach the IPP port can send it.

## Exposure and containment

Both faults are reachable by any client that can open the IPP port. The port is
bound to `127.0.0.1` only (`papplSystemAddListeners(system, "127.0.0.1")` in
`crates/pappl/src/application.rs`), so in the shipped configuration the reach is
**local**: another process on the same host, not the network. That is what
keeps S-1 and S-2 a local denial of service rather than a remote one. Any change
that binds a non-loopback address inherits both as remote-reachable faults and
must not be made while the underlying libpappl is unpatched.

S-2's silent-corruption band means "denial of service" is the floor, not
necessarily the ceiling; a stack overflow with attacker-controlled contents is
not something to characterise more precisely from a black-box crash, so it is
treated as the more serious of the two.

## S-3 — the service runs as root, which widens S-1 and S-2

`packaging/systemd/ml216x-printer-app.service` runs the daemon as `root`, for
the reasons written in the unit: a root PAPPL server keeps its state in
`/var/lib`, its control socket in `/run`, and a `usb://` device is a node under
`/dev/bus/usb`. The cost is that a local client crashing the service through
S-1 or S-2 is crashing a root process. A dedicated system user plus a udev rule
granting it the printer device is the right shape and is deferred to hardware
bring-up (P12), because the udev rule needs the printer's real USB vendor and
product IDs, which have not been read off a device. Until then the unit already
carries `ProtectHome`, `PrivateTmp` and `NoNewPrivileges`, which limit what a
successful exploit of S-1 could reach, not whether it can crash the process.

## Actions

Tracking the Q-1 follow-up's four agreed actions:

1. **Confirm the unpatched lines before filing — done.** S-1 is
   `job-process.c:696`/`:718`/`:739`, fixed upstream by `4587888f50`; S-2 is
   `printer-ipp.c:1045`/`:1068`, fixed upstream by `44327aaac3`. Both are
   present in `pappl 1.3.1-2.1` and both were reproduced to a crash.
2. **Determine whether we are exposed to the dithering issue — done, and the
   answer is yes.** Declaring `BLACK_1` does not remove the exposure; see S-1.
   `BLACK_1` stays, because the printer speaks only 1-bit and the answer must
   not override what the hardware needs — it is not a security lever here.
3. **Loopback-only listener — already the case.** `application.rs` binds
   `127.0.0.1`. A non-loopback bind is a deliberate future opt-in and must wait
   on a patched libpappl.
4. **File the Debian bug and record the number here.** Still to do: file
   against `src:pappl` citing `4587888f50` and `44327aaac3` and the confirmed
   1.3.1 lines above, and add the bug number to this document and the README.
   This needs a bug-tracker submission and is left for the maintainer.

## Reproductions

The scripts live under `scripts/` and are self-contained (loopback, scoped
`XDG_CONFIG_HOME`, temporary output):

- `scripts/security-probe.py --case dither` reproduces S-1.
- `scripts/security-probe.py --case ready-media` reproduces S-2.

Both print the server's exit signal and assert on it, so they fail if a future
libpappl fixes the bug — at which point the corresponding row above can be
retired rather than left as a stale warning.
