# Debian bug report — draft, not yet submitted

Action 4 of `docs/SECURITY-REVIEW.md` owes Debian a report against `src:pappl`
for S-1 and S-2. This is the text, written out so submitting it is a copy and a
send. **Nothing here has been filed.** When it is, put the bug number in
`docs/SECURITY-REVIEW.md` and in `README.md`, and change this file's first line.

## How to send it

From a Debian machine with `reportbug` installed:

```sh
reportbug --severity=grave --tag=security --tag=upstream src:pappl
```

Or by mail, to `submit@bugs.debian.org`, with the pseudo-headers below as the
first lines of the body. Either way the report reaches the Debian Printing
Team. Because these are memory-corruption bugs, consider also mailing
`team@security.debian.org` — that is their call to make, not ours, but the
`security` tag alone does not always reach them.

---

## The report

```
Package: src:pappl
Version: 1.3.1-2.1
Severity: grave
Tags: security upstream fixed-upstream

Dear Maintainer,

libpappl 1.3.1 as shipped in trixie carries two memory-safety defects that
are fixed in upstream PAPPL but not in the packaged version. Both are
reachable by any client that can open a printer application's IPP port, and
both were reproduced to a crash against 1.3.1-2.1+b2 on trixie (amd64) while
developing a printer application against this library.

I am reporting them together because they share a cause — an
attacker-controlled count used without being bounded — and because the fix in
both cases is an upstream commit that already exists.

(1) Out-of-bounds write while dithering an 8-bit raster wider than the page
    pappl/job-process.c, _papplJobProcessRaster
    Fixed upstream by commit 4587888f50
    Confirmed unpatched in 1.3.1 at job-process.c:696, :718 and :739;
    pappl/job-filter.c:412 has the same pattern for the image filters.

The output line is allocated from the *job's* header:

    if ((line = malloc(options->header.cupsBytesPerLine)) == NULL)

while the dither loop is bounded by the *document's* header:

    for (x = 0, lineptr = line, ...; x < header.cupsWidth; x ++, pixptr ++)

When the incoming raster is wider than the printer's page, lineptr walks past
the end of line. A driver cannot prevent this: the overflow happens inside
libpappl, before the driver's rwriteline callback is reached.

Note that declaring 1-bit output does not avoid the path. Forcing
PAPPL_PWG_RASTER_TYPE_BLACK_1 constrains the output, not the input, and is
precisely what selects the dithering path for an 8-bit document.

Reproduced: an 8-bit grayscale PWG raster 40000 px wide, submitted with
ipptool to a printer application whose page is 4960 px at 600 dpi (A4). The
server dies with SIGABRT and glibc reports "corrupted size vs. prev_size". A
raster that fits the page, including one slightly narrower than the page at
5000 px, is dithered without incident, which places the trigger at the width
mismatch rather than at the 8-bit path.

(2) Stack buffer overflow from an oversized media-ready list
    pappl/printer-ipp.c, _papplPrinterSetAttributes
    Fixed upstream by commit 44327aaac3
    Confirmed unpatched in 1.3.1 at printer-ipp.c:1045 (media-col-ready)
    and :1068 (media-ready).

A client-supplied list is copied into driver_data.media_ready, which is
pappl_media_col_t[PAPPL_MAX_SOURCE] — 16 entries — inside a
pappl_pr_driver_data_t declared on the stack at printer-ipp.c:911. The
preflight compares the count against PAPPL_MAX_SOURCE only to decide the
response's unsupported-attributes status; the copy loop then runs regardless:

    count = ippGetCount(rattr);
    for (i = 0; i < count; i ++)
        _papplMediaColImport(ippGetCollection(rattr, i),
                             driver_data.media_ready + i);

Each entry is 228 bytes and the enclosing struct is 8728 bytes, so roughly
twenty entries fit inside the struct before the copy reaches the rest of the
stack frame.

Reproduced: Set-Printer-Attributes carrying 512 media-ready values kills the
server with SIGSEGV. Counts up to 32 did not crash in testing, which is the
part that worries me most: a count in the low twenties overwrites adjacent
stack contents without an immediate crash, so a carefully sized request is
silent corruption rather than a clean abort. Set-Printer-Attributes is
accepted with no authentication service configured.

Exposure

In my own application the listener is bound to 127.0.0.1, which makes both
faults local rather than remote. That is a property of my configuration and
not of the library: papplSystemAddListeners takes whatever address the
application passes, and a printer application that listens on a network
interface — which is what the framework is for — inherits both as
remotely reachable faults. I would not want the loopback case to set the
severity.

For (2) I am treating denial of service as the floor rather than the ceiling:
a stack overflow with attacker-controlled contents is not something I am
willing to characterise more precisely from a black-box crash.

Suggested fix

Cherry-pick 4587888f50 and 44327aaac3 into the trixie package, or update to an
upstream release that contains both. I have not prepared a patch against the
Debian packaging; if that would help, say so and I will.

Reproducers

Both crashes are reproduced by a self-contained script in my project, which
runs a printer application on the loopback address in a temporary directory
and asserts on the server's exit signal, so the checks fail if a future
libpappl stops crashing:

  https://github.com/caganerg/ml216x-printer-app
  scripts/security-probe.py --case dither        (1)
  scripts/security-probe.py --case ready-media   (2)

The reasoning behind both, with the line numbers as confirmed in 1.3.1, is in
docs/SECURITY-REVIEW.md in the same repository.

System information: Debian trixie, amd64; libpappl1t64 1.3.1-2.1+b2;
libcups2t64 2.4.10.

Thank you for maintaining this package.
```

---

## Two more upstream conversations this does not cover

Kept here so they are not lost, and deliberately **not** folded into the report
above: a bug report that asks for three unrelated things gets one of them.

1. **The control socket is created 0777** (S-4 in `docs/SECURITY-REVIEW.md`).
   A non-root PAPPL server puts its control socket at `$TMPDIR/<name><uid>.sock`
   and creates it world-writable, and every client subcommand — `add`,
   `modify`, `delete`, `default`, `submit`, `shutdown` — is reachable through
   it with no authentication of its own. In `/tmp` that is any local account.
   This project contains it by pointing `TMPDIR` at `$XDG_RUNTIME_DIR`
   (decision Q-19), but the mode is libpappl's to fix: 0600, or placing the
   socket under `$XDG_RUNTIME_DIR` itself. This belongs upstream, and is a
   security report rather than a wishlist item.

2. **There is no way to decline DNS-SD** (decision Q-23). A printer
   application that binds the loopback address still advertises every printer
   over DNS-SD, and nothing in the API declines: there is no `PAPPL_SOPTIONS_`
   flag, `printer-dns-sd-name` is not in `printer-settable-attributes`, the web
   interface has no field, and `papplPrinterSetDNSSDName` — the one call that
   would do it — deadlocks when made from the `PAPPL_EVENT_PRINTER_CREATED`
   callback, because PAPPL raises that event while holding the printer's own
   lock. That deadlock is arguably a bug on its own. The consequence for users
   is a duplicate CUPS queue pointing at a port nothing can reach. This one is
   a wishlist/feature request against upstream PAPPL, not against Debian.
