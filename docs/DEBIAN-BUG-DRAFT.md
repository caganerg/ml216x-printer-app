# Debian bug reports — drafts, not yet submitted

Action 4 of `docs/SECURITY-REVIEW.md` owes Debian a report against `src:pappl`
for S-1 and S-2. This is the text, written out so submitting it is a copy and a
send. **Nothing here has been filed.** When it is, put the bug number in
`docs/SECURITY-REVIEW.md` and in `README.md`, and change this file's first line.

**Update 2026-09-12 (decision Q-27).** Upstream PAPPL 1.4.12 has since been
built and tested against this tree, which sharpens both reports rather than
retiring them. Of the six libpappl defects, **1.4.12 fixes four — S-1, S-2,
S-6 and S-7 — and leaves S-4 and S-5 exactly as 1.3.1 has them.** Trixie, forky
(testing) and sid all still carry 1.3.1-2.1, so everything below stands as a
report against Debian; what changes is that "fixed upstream" is now a verified
claim about a release that was run here, not a reading of a changelog. Where a
report says a defect is fixed upstream, 1.4.12 is the release to name.

There are now **two** reports here. The first is the memory-safety pair,
unchanged. The second, added 2026-09-12, is the three crashes and hangs
reachable through a printer application's web interface (S-5, S-6 and S-7).
They are kept apart deliberately, for the reason stated at the bottom of this
file: a report that asks for several unrelated things gets one of them
answered. The second report is a different severity, a different class and a
different set of upstream lines, and it is the one this project hit in normal
use rather than while hunting.

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
are fixed in upstream PAPPL 1.4.12 but not in the packaged version. Both are
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

My own application now binds every interface, which is what
papplSystemAddListeners(system, NULL) does and what a printer application is
for, so both faults are remotely reachable in it. An application that binds
127.0.0.1 instead has them as local faults only — that is a property of the
configuration, not of the library, and I would not want the loopback case to
set the severity.

For (2) I am treating denial of service as the floor rather than the ceiling:
a stack overflow with attacker-controlled contents is not something I am
willing to characterise more precisely from a black-box crash.

Suggested fix

Cherry-pick 4587888f50 and 44327aaac3 into the trixie package, or update to an
upstream release that contains both. Both are two-line bounds clamps, and both
are in upstream 1.4.12, which I have built and tested against my own
application: the two reproducers below kill a 1.3.1-2.1+b2 server and cannot
make a 1.4.12 one fall over. I have not prepared a patch against the Debian
packaging; if that would help, say so and I will.

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

## The second report — the web interface (S-5, S-6, S-7)

Same submission route as above, with a different severity:

```sh
reportbug --severity=important --tag=upstream src:pappl
```

```
Package: src:pappl
Version: 1.3.1-2.1
Severity: important
Tags: upstream

Dear Maintainer,

Three defects in libpappl 1.3.1 make a printer application's own web
interface unusable or fatal, depending on the machine. Two of them, (2) and
(3) below, are fixed in upstream 1.4.12; (1) is not, and is still present
there. All three were found
while developing a printer application against the trixie package, and all
three were reproduced against 1.3.1-2.1+b2 on trixie (amd64). None of them
needs an unusual configuration: the first needs only a printer application
that does not set footer HTML, and the other two need only a machine without
a reachable D-Bus system bus or without a running avahi-daemon.

(1) Null footer HTML is dereferenced on every web page
    pappl/client-webif.c, papplClientHTMLFooter
    pappl/loc.c, papplLocGetString

papplClientHTMLFooter resolves the footer before testing it:

    const char *footer = papplClientGetLocString(client,
                             papplSystemGetFooterHTML(papplClientGetSystem(client)));
    if (footer) { ... }

papplSystemGetFooterHTML returns NULL for an application that set none —
papplMainloop only calls papplSystemSetFooterHTML when its footer_html
argument is non-NULL — and papplLocGetString passes the key straight into the
array lookup:

    search.key = (char *)key;
    match      = cupsArrayFind(loc->pairs, &search);

whose comparison calls strcmp on it. The early return in papplLocGetString
guards a null "loc", not a null key.

Reproduced: a printer application built with footer_html = NULL segfaults on
"/", "/addprinter", "/config" and each printer's own pages. The crash happens
after the status line and most of the body are written, so the browser shows a
truncated page and the server is gone. Backtrace:

    papplLocGetString → cupsArrayFind → strcmp
    papplClientHTMLFooter
    _papplSystemWebAddPrinter
    _papplClientProcessHTTP

Suggested fix: return the key when it is NULL in papplLocGetString, or skip
the lookup in papplClientHTMLFooter when there is no footer.

(2) A null DNS-SD client is passed to Avahi while listing devices
    pappl/device-network.c:459, pappl_dnssd_find

    if ((pdl_ref = avahi_service_browser_new(_papplDNSSDInit(NULL), ...)) == NULL)

_papplDNSSDInit returns NULL when avahi_client_new fails, which is what
happens when the D-Bus system bus cannot be reached at all — a container
without dbus, or a minimal installation. avahi_service_browser_new asserts on
its client argument, so the process dies:

    Unable to initialize DNS-SD: Daemon not running
    app: browser.c:581: avahi_service_browser_new: Assertion `client' failed.

The null is checked one line later, on the browser rather than on the client.
Note that a stopped avahi-daemon alone does not trigger this: with the bus
reachable, AVAHI_CLIENT_NO_FAIL yields a client in the connecting state and
the browse fails cleanly. It is the unreachable bus that aborts.

Reachable from the "devices" sub-command and from the web interface's "Add
Printer" page, which calls papplDeviceList(PAPPL_DEVTYPE_ALL, ...) at
pappl/system-webif.c:512 — so on such a machine any client that can open the
HTTP port can kill the server.

Suggested fix: cherry-pick it from upstream 1.4.12, which already does this —
pappl_dnssd_find there takes the client into a variable and returns cleanly
when it is NULL, releasing the lock on the way out. Otherwise: test the result
of _papplDNSSDInit before browsing, as dnssd.c:467 and :937 already do for
registration.

(3) The DNS-SD lock is not released when a browse fails
    pappl/device-network.c:459-464, pappl_dnssd_find

The same function takes the lock and then returns without releasing it:

    _papplDNSSDLock();
    ...
    if ((pdl_ref = avahi_service_browser_new(...)) == NULL)
    {
      _papplDeviceError(err_cb, err_data, "Unable to create service browser.");
      cupsArrayDelete(devices);
      return (ret);            /* still holding the lock */
    }
    _papplDNSSDUnlock();

_papplDNSSDLock is avahi_threaded_poll_lock, so every later DNS-SD operation
in the process blocks forever. Reproduced on a machine where browsing could
not succeed: the first request for "/addprinter" completed in 2.0 s, the
second and third never completed and were cut off at 20 s. This is the
ordinary case of a machine with D-Bus but no running avahi-daemon.

Suggested fix: unlock on that path. Upstream 1.4.12 does, in the same place as
(2) — both early returns there call _papplDNSSDUnlock() before returning.

Exposure

My own application binds every interface, as papplSystemAddListeners(system,
NULL) does, so anyone who can reach the HTTP port can stop the print server
repeatedly by fetching a page; an application bound to 127.0.0.1 has the same
faults as local ones. Either way (1) needs no argument, no form submission and
no authentication, just a GET.

Of these three, (2) and (3) are fixed in upstream 1.4.12, which I have built
and tested; (1) is present there unchanged, so it is the one that needs a fix
rather than a backport.

Reproducer

A self-contained script in my project fetches every page of a printer
application's web interface and asserts that the server is still running
afterwards. Against a build that sets no footer it fails on the first page
with the server dead of SIGSEGV; against one that sets a footer it passes:

  https://github.com/caganerg/ml216x-printer-app
  scripts/server-probe.py

The reasoning for all three, with the confirmed 1.3.1 lines, is in
docs/SECURITY-REVIEW.md in the same repository, as S-5, S-6 and S-7.

System information: Debian trixie, amd64; libpappl1t64 1.3.1-2.1+b2;
libcups2t64 2.4.10; libavahi-client3 0.8-16.

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

2. **Declining DNS-SD costs an application its state handling** (decisions
   Q-23 and Q-24). A printer application that binds the loopback address still
   advertises every printer over DNS-SD, and the only way found to decline is
   indirect: clear each printer's DNS-SD name after the state file is read and
   before `papplSystemRun` registers anything. Since the mainloop loads the
   state file itself, with no hook between the load and the run, an
   application that wants this has to take the whole state file over —
   including recomputing the path the mainloop would have used, or its users
   lose their printers. There is no `PAPPL_SOPTIONS_` flag,
   `printer-dns-sd-name` is not in `printer-settable-attributes`, and the web
   interface has no field. `papplPrinterSetDNSSDName` works, but not from the
   `PAPPL_EVENT_PRINTER_CREATED` callback: `papplSystemAddEvent` raises the
   event holding the printer's lock for reading and the setter takes it for
   writing, so the creating thread deadlocks. That deadlock is arguably a bug
   on its own, and a printer added through the web interface's own form cannot
   be declined at all, because PAPPL registers it there explicitly. The
   consequence for users is a duplicate CUPS queue pointing at a port nothing
   can reach. This is a wishlist/feature request against upstream PAPPL — a
   system option, or an event raised without the lock — not against Debian.
