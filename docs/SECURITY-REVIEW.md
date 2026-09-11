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
| S-3 | our systemd unit | the service ran as root | n/a | yes — fixed in 2.0.0~alpha-4 (Q-18) |
| S-4 | libpappl control socket | mode 0777, any local user may drive the server | not in the shipped configuration | contained in 2.0.0~alpha-4 (Q-19) |
| S-5 | libpappl `client-webif.c` + `loc.c` | a null footer is dereferenced on every web page | yes, over HTTP — it killed the server | avoided in 2.0.0~alpha-7 (Q-25) |
| S-6 | libpappl `device-network.c` | a null DNS-SD client is asserted on while listing devices | yes, wherever D-Bus is unreachable | no — 2.0.0~alpha-7 stopped provoking it (Q-24) |
| S-7 | libpappl `device-network.c` | the Avahi lock is kept after a failed browse | yes, wherever DNS-SD browsing fails | no |

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

## S-3 — the service ran as root, which widened S-1 and S-2 (FIXED in 2.0.0~alpha-4)

Until 2.0.0~alpha-3, `packaging/systemd/ml216x-printer-app.service` ran the
daemon as `root`, for the reasons written in that unit: a root PAPPL server
keeps its state in `/var/lib`, its control socket in `/run`, and a `usb://`
device is a node under `/dev/bus/usb`. The cost was that a local client
crashing the service through S-1 or S-2 crashed a root process, and that S-1's
heap overflow ran with root's authority.

**Fixed by decision Q-18.** The package now installs a systemd *user* unit and
enables it per user; the server runs as the logged-in user, its state is in
that user's `$XDG_CONFIG_HOME`, its spool is a 0700 `RuntimeDirectory` under
`$XDG_RUNTIME_DIR`, and the USB node is reached through a `uaccess` ACL on the
one hardware-confirmed device rather than by being root. S-1 and S-2 are
unchanged as bugs and remain reachable by any local client that can open the
IPP port, but what they now reach is one unprivileged session's own process
rather than the machine. The two libpappl faults still need the upstream fix;
this narrows their consequence, and nothing more.

The retired system unit stays in the tree as the record of what it did. If
anyone reinstates it by hand, S-3 comes back with it.

## S-4 — the control socket is connectable by any local user (CONTAINED in 2.0.0~alpha-4)

Measured, not inferred. Run as uid 1000 with `TMPDIR` unset:

```
I [...] Listening for connections on '/tmp/ml216x-printer-app1000.sock'.
srwxrwxrwx 1 dev dev 0 Eyl  6 22:42 /tmp/ml216x-printer-app1000.sock
```

Mode 0777 on an `AF_UNIX` socket means any local user may connect, and the
subcommands reached through it — `add`, `modify`, `delete`, `default`,
`submit`, `shutdown` — are the server's whole control surface, with no
authentication of their own. libpappl chooses the path: `%s/%s%d.sock` under
`$TMPDIR` or `/tmp` with the caller's uid for a non-root server, `/run/%s.sock`
for a root one; both format strings are present in `libpappl.so.1`.

This predates the user-service move and was worse under it: the same 0777
socket in `/run` let any local account drive a **root** server.

**Contained, by decision Q-19.** The socket's mode is libpappl's and is
unchanged — it is still 0777 — but the directory holding it is no longer
`/tmp`. `crates/ml216x-printer-app/src/runtime.rs` points `TMPDIR` at
`$XDG_RUNTIME_DIR` when the caller has not set one, and only when that
directory has nothing set for group or other; the socket then sits inside a
per-user 0700 directory that no other account can traverse. Server and client
are the same binary and make the same decision, so no documented command
changed. Verified by running it: with `TMPDIR` unset the server binds
`/run/user/1000/ml216x-printer-app1000.sock`, `/tmp` gets nothing, and a
`status` from a shell with no `TMPDIR` still answers.

Two things this does not do, and they are the reason the row says *contained*
rather than *fixed*. It does not change the socket's permissions, so anything
that puts it back in a shared directory — an explicit `TMPDIR=/tmp`, a
`$XDG_RUNTIME_DIR` that is not private, a root server, which uses `/run` and
ignores `TMPDIR` entirely — is exposed exactly as before, and the first two
print a warning saying so. And it is a local workaround for a libpappl default
that would be better fixed upstream, which is candidate (d) in Q-19 and is not
this project's to schedule.

## S-5 — a null footer is dereferenced on every web page (AVOIDED in 2.0.0~alpha-7)

Found while measuring Q-24, by fetching the pages rather than by reading code.
Every page of the web interface killed the server:

```
papplLocGetString → cupsArrayFind → strcmp
papplClientHTMLFooter
_papplSystemWebAddPrinter
_papplClientProcessHTTP
```

`papplClientHTMLFooter` (`pappl/client-webif.c`) resolves the footer before it
checks whether there is one:

```c
const char *footer = papplClientGetLocString(client, papplSystemGetFooterHTML(...));
if (footer) { ... }
```

and `papplLocGetString` (`pappl/loc.c`) puts the key straight into the array
lookup — `search.key = (char *)key; cupsArrayFind(loc->pairs, &search)` —
whose comparison calls `strcmp`. An application that set no footer HTML has
`papplSystemGetFooterHTML` return `NULL`, so the lookup dereferences it. The
early return in `papplLocGetString` covers a null *loc*, not a null key.

The crash lands after the status line and most of the body have been written,
so the client sees a truncated page and the print server is gone.

Reproduced on `/`, `/addprinter`, `/config` and a printer's own pages against
`1.3.1-2.1+b2`, with `avahi-daemon` running and with it unreachable, and
against both binaries in `dist/` — so 2.0.0~alpha-5 and 2.0.0~alpha-6 both
shipped with it, while `README.md` recommended opening
`http://localhost:8631/`. Reach is local: the port is loopback-only (Q-18),
and the web interface has no authentication service configured, so any local
account could stop the print server at will, repeatedly.

**Avoided by decision Q-25**, which passes a footer string, so the lookup has
a key and returns it. The defect is libpappl's and is unchanged; an
application that passes no footer still crashes.
`scripts/server-probe.py` fetches every page and asserts the server is still
running, which is what keeps this from coming back.

## S-6 — a null DNS-SD client is asserted on while listing devices (NOT FIXED)

`pappl/device-network.c:459` passes the result of `_papplDNSSDInit(NULL)`
straight into `avahi_service_browser_new`:

```c
if ((pdl_ref = avahi_service_browser_new(_papplDNSSDInit(NULL), ...)) == NULL)
```

`_papplDNSSDInit` returns `NULL` when `avahi_client_new` fails, which happens
whenever the D-Bus system bus cannot be reached at all — a container without
`dbus`, a minimal server, or a process whose `DBUS_SYSTEM_BUS_ADDRESS` points
somewhere unusable. `avahi_service_browser_new` asserts on the client, so the
process dies:

```
Unable to initialize DNS-SD: Daemon not running
ml216x-printer-app: browser.c:581: avahi_service_browser_new: Assertion `client' failed.
```

The null is checked one line later, on the browser rather than on the client,
which is the whole defect. Note that a *stopped* `avahi-daemon` does not
trigger it: with the bus reachable, `AVAHI_CLIENT_NO_FAIL` returns a client in
the connecting state and the browse then fails cleanly. It is the unreachable
bus that is fatal.

Reachable two ways in a printer application: the `devices` sub-command, and
the web interface's "Add Printer" page, which lists devices
(`pappl/system-webif.c:512`) — so a client that can open the HTTP port can
kill the server on a machine with no system bus.

**This project stopped provoking it in 2.0.0~alpha-7** (Q-24 replaced the
mechanism that had made the bus unreachable on purpose), but the defect is
untouched and this project cannot fix it: the page belongs to PAPPL.
`scripts/server-probe.py` therefore skips that page where there is no system
bus, and says so rather than failing.

## S-7 — the Avahi lock is kept after a failed browse (NOT FIXED)

The same function takes the DNS-SD lock before browsing and returns without
releasing it when the browse fails:

```c
_papplDNSSDLock();
...
if ((pdl_ref = avahi_service_browser_new(...)) == NULL)
{
  _papplDeviceError(err_cb, err_data, "Unable to create service browser.");
  cupsArrayDelete(devices);
  return (ret);            // the lock is still held
}
_papplDNSSDUnlock();
```

`_papplDNSSDLock` is `avahi_threaded_poll_lock`, so the next DNS-SD operation
in that process waits forever. Measured against a server whose browse could
not work: the first view of "Add Printer" answered in 2.0 s, the second and
third never completed and were cut off at 20 s. In a printer application that
means one hung request and, with it, any later DNS-SD work in the process.

Reachable wherever browsing fails while the client exists — the ordinary case
of a machine with D-Bus but no running `avahi-daemon`. Not this project's to
fix; `scripts/server-probe.py` reports it as a `KNOWN` finding rather than a
failure, because a red check for an upstream defect that the environment
decides is not a signal anyone can act on.

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
4. **File the Debian bug and record the number here.** **Drafted 2026-09-10,
   extended 2026-09-12 with a second report for S-5, S-6 and S-7; not yet
   submitted.** The full text is `docs/DEBIAN-BUG-DRAFT.md` — against
   `src:pappl` 1.3.1-2.1, severity grave, tagged security/upstream, citing
   `4587888f50` and `44327aaac3` and the confirmed 1.3.1 lines above, with both
   reproductions and the exposure argument. What is left is the send itself,
   which needs a bug-tracker submission and stays with the maintainer. **When
   it is sent, put the bug number here and in `README.md`** — until then this
   item is open, and the draft says so on its first line.
5. ~~**Decide S-4.**~~ **Done.** Q-19 was decided by delegation and
   implemented in 2.0.0~alpha-4; see S-4 above for what it does and does not
   cover. What is left is the upstream half — a libpappl that creates its
   control socket 0600, or places it under `$XDG_RUNTIME_DIR` itself — which
   belongs in the same conversation as the S-1/S-2 bug report.

## Reproductions

The scripts live under `scripts/` and are self-contained (loopback, scoped
`XDG_CONFIG_HOME`, temporary output):

- `scripts/security-probe.py --case dither` reproduces S-1.
- `scripts/security-probe.py --case ready-media` reproduces S-2.
- `scripts/server-probe.py --application dist/<older binary>` reproduces S-5:
  the run fails on the first page with the server dead of `SIGSEGV`. Against
  this tree the same run passes, which is the regression test for Q-25.

Both print the server's exit signal and assert on it, so they fail if a future
libpappl fixes the bug — at which point the corresponding row above can be
retired rather than left as a stale warning.
