# Continuous integration — what it checks, and what it cannot

*Written 2026-09-10, when CI first existed.*

Before this, the list of things that must pass lived in prose — in
`CONTRIBUTING.md`, in `docs/SESSION-STATE.md`, in `docs/GOLDEN-VALIDATION.md`
— and every run was somebody typing eight commands from memory. Two documents
already spoke of "keeping the corpus in CI" and of CI "building both with and
without `golden-replay`" while no workflow existed at all.

That gap had already cost something measurable. Three clippy lints that did
not exist when the engine was written turn the documented `-D warnings` run
red on rustc 1.95 (`unnecessary_sort_by`, `explicit_counter_loop`,
`manual_div_ceil`, all in `spl2-core`), and nothing announced it; it was found
because somebody happened to type the command. Fixing the lints is one commit.
Not noticing for weeks is the problem CI is for.

## One entry point

`scripts/run-checks.sh` is the check list. CI runs that script and nothing
else, so the enforced list and the documented list are the same list, and a
contributor can reproduce a red run exactly.

```sh
./scripts/run-checks.sh            # every group, in order
./scripts/run-checks.sh --list     # the groups, and what each covers
./scripts/run-checks.sh --self-test    # prove the runner still fails fast
./scripts/run-checks.sh test probes
```

| Group | What it runs | Why it is a group of its own |
|---|---|---|
| `fmt` | `cargo fmt --all --check` | — |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` | — |
| `test` | `cargo test --workspace` — 163 tests, the golden corpus among them | The corpus is the only thing that catches a PJL reordering (`docs/GOLDEN-VALIDATION.md` §2) |
| `features` | `spl2-core` built with **and** without `golden-replay`, plus the `qpdl-decode` example | Decision Q-6 put the CUPS raster parser behind a non-default feature precisely so it stays out of the shipping path; only a no-feature build proves it did |
| `goldens` | `sha256sum -c goldens/SHA256SUMS` | A blessed corpus must not drift from its recorded checksums; the bless discipline depends on the checksums being real |
| `probes` | `p5-probe` (3 modes, and its output **diffed against the committed `docs/P5-MEASUREMENTS.json`**), `transport-probe` plain and both injections, `server-probe` (every web page, and a printer restored from state with DNS-SD registration and a wildcard listener), `g1-probe --all` (44 cases) and all three injections | These drive the real application over loopback; they are the only checks that exercise PAPPL itself |
| `security` | `security-probe.py` — reproduces the two libpappl 1.3.1 overflows | Its failure is news about the archive, not about this tree; see below |
| `deb` | `sh -n` over the maintainer scripts, then `scripts/build-deb.sh` | The hand-built package path never substitutes `${shlibs:Depends}`, so the packaging has to be built to be believed |

One thing the script adds that no single existing tool did: the P5 record is
now **compared**, not just regenerated. `scripts/p5-probe.py` measures what
PAPPL delivers and writes JSON; the claim in `docs/MARGINS.md` is that a rerun
reproduces `docs/P5-MEASUREMENTS.json` byte for byte, and the script never
checked that. The `probes` group diffs them.

## The jobs

All five run in a `debian:trixie` container, because decision Q-1/D-1 targets
the libpappl Debian trixie ships (1.3.1-2.1+b2) and nothing else.

| Job | Toolchain | Groups | Gates? | What a red run means |
|---|---|---|---|---|
| `build-and-test` | trixie's packaged rustc (1.85.0) | `--self-test`, then `fmt clippy test features goldens` | yes | A defect in the change under test — or, if `--self-test` is what failed, in the runner itself |
| `harnesses` | trixie's packaged rustc | `probes` | yes | Either the driver's geometry moved, or a harness stopped being able to detect that it moved |
| `package` | trixie's packaged rustc | `deb` | yes | The package no longer builds; the built `.deb` is uploaded as an artifact |
| `security-signal` | trixie's packaged rustc | `security` | **no** | Most likely **libpappl was fixed** — go and retire the matching row in `docs/SECURITY-REVIEW.md`. It does not gate, because that is news about the archive and must not block an unrelated pull request |
| `future-toolchain` | current stable, via rustup | `fmt clippy test features` | **no** | A newer compiler's new lint or a behaviour change. Advisory on purpose: this is the drift that went unnoticed until it was typed by hand, and it should warn before the next Debian rustc makes it blocking |

The workflow also runs weekly (`cron: 17 5 * * 1`). Two of the signals above
arrive without anyone pushing a commit: Debian patching libpappl, and a new
lint in a newer stable Rust.

### `--offline --locked`, deliberately

Every cargo invocation in the script is `--offline --locked`. This workspace
has no third-party crates — `Cargo.lock` holds exactly the five local packages
— `spl2-core` is required to stay dependency free, and the FFI crates are hand
written rather than generated. Offline keeps CI hermetic and turns "somebody
added a dependency" into a visible, deliberate edit of the script rather than
a silent network fetch.

### The declared minimum Rust is not tested

Three crates declare `rust-version = "1.77"`. Nothing has ever compiled
against it: the development host has rustup 1.95.0 and trixie packages 1.85.0.
That is written up as open question **Q-22** in `docs/DECISIONS.md` — with
resolutions and my expectation — rather than fixed here, because it decides
what the project promises downstream.

## What a green run does not mean

CI is the software half. Everything below is outside it, and none of it is
covered by any amount of green:

1. **Release gate G-1 — where toner lands on paper.** Still open, per model.
   `g1-probe.py --all` proves the measurement page survives the driver
   unchanged on all 44 media/resolution combinations; the sheet has not been
   printed. Runbook: `docs/G1-MEASUREMENT.md`.
2. **The hard margin is self-consistent, not measured.** Every case derives
   its margin from the same 12.5 pt constant, so byte agreement proves
   internal consistency and nothing about the paper. Q-13 (the vertical rule)
   and Q-21 (the 0.33 mm byte-column shift) are open for the same reason.
3. **USB.** No USB device exists on a runner, so `devices` lists nothing and
   the P7 remainder stays open: the duplicate desktop queue after reconnect or
   power cycle, the `uaccess` ACL on `04e8:330f`, and the still-unrecorded
   IEEE-1284 device ID. See `docs/HARDWARE-TESTING.md`.
4. **The maintainer scripts are parsed, never executed.** `sh -n` only.
   Running `postinst` installs a service, which is not something CI should do
   to itself; the five branches of its dangling-symlink condition were
   exercised by hand in a scratch directory instead.
5. **The `.deb` is built, never installed or upgraded.** The
   `Conflicts/Replaces/Provides` path that removes `samsung-ml2160-rust` —
   which is what stops the old root service — is untested by CI.
6. **Client interoperability beyond `Print-Job`.** Every probe uses
   `Print-Job` because `ipptool -t` fails every `Get-Printer-Attributes`
   against this printer over a libcups validation bug that is not this
   driver's (recorded in `docs/SESSION-STATE.md`).
7. **Print quality of any kind.** No dithering, no toner, no paper path.

## Evidence that the check list can fail

A harness that has never gone red is not evidence, so each claim above was
made to fail. Two kinds of proof:

`server-probe.py` has no `--inject` flag; the previous releases are the
injection. Note that the `harnesses` job runs as **root** in its container, so
it exercises the branch where PAPPL keeps the state file in
`/var/lib/<base name>.state` and ignores `XDG_CONFIG_HOME` — the probe expects
the path for the uid it is running under, and asserts the server logged
loading exactly that file. The first version of the probe assumed the scoped
`XDG_CONFIG_HOME` and turned this job red for that reason alone. `--application dist/...` drives the same checks against the
2.0.0~alpha-5 and 2.0.0~alpha-6 binaries, and both go red on the first page
with the server dead of SIGSEGV and on the DNS-SD registration the restarted
server still asks for. Two of its checks are conditional on the environment
and print `KNOWN` rather than failing when they cannot run: the "Add Printer"
page is not fetched where there is no system D-Bus socket, because PAPPL
aborts on a null DNS-SD client while listing devices, and `avahi-browse` is
only consulted where it is installed. Both are upstream defects this tree
cannot fix; they are S-6 and S-7 in `docs/DEBIAN-BUG-DRAFT.md`.

**Injections, which run on every push.** They are part of the `probes` and
`security` groups rather than a one-off experiment, so they keep proving
themselves: `transport-probe --inject truncate` and `--inject flip`,
`g1-probe --inject shift` (R-1 displacement), `--inject crop` (Q-13's vertical
rule) and `--inject scale` (R-4), `p5-probe --device-failure` (which requires
`job-state=aborted` from a `/dev/full` device), and `security-probe.py`
(which requires the server to die from a signal). Each of those cases passes
only when it *detects* the defect it planted.

**Mutation runs against the groups whose redness the injections do not
cover**, performed on 2026-09-10 in a scratch worktree:

| Mutation | Group | Result |
|---|---|---|
| One byte of `goldens/a4-600-marks.spl` overwritten with `\0` | `goldens` | RED — `a4-600-marks.spl: FAILED`, "1 computed checksum did NOT match", exit 1; green again once reverted |
| One measured page width in `docs/P5-MEASUREMENTS.json` moved by 1 pixel (4960 → 4961) | `probes` | RED — the diff step failed with exit 1 before any other probe ran. **First it passed**; see below |
| A feature-gated `raster` item referenced from non-gated code in `spl2-core` | `features` | RED — the no-feature build failed with `E0433: cannot find module or crate raster` (exit 101) while `cargo test --workspace` stayed green, which is exactly the Q-6 blind spot the group exists for |

### The mutation that found a defect in the runner itself

The second mutation passed on its first run, with the corrupted record sitting
in the tree, and that was a bug in `run-checks.sh` rather than a weak check.
The runner called each group as `run_$g || fail $g`. Putting a shell function
on the left of `||` disables `set -e` for the whole function body, so a
command that failed in the middle of a group was ignored and the group's
status became the status of its *last* command. `goldens` had gone red only
because `sha256sum` happened to be the last thing it ran.

Groups are now called plainly, `set -e` ends the run at the first failure, and
an exit trap reports which group was running. Two things came out of it:

* `--self-test` runs a hidden group whose middle command fails and whose last
  command would succeed, and requires the run to abort, name the group, and
  never reach the last line. The gating CI job runs it before anything else.
* The self-test had to be made to fail too, and the first version did not:
  it called the group function directly, bypassing the runner loop, so it
  passed even with the `|| fail` runner reintroduced. It now goes through the
  loop like any other group. Against the `|| fail` runner it reports
  `FAILED — a group with a failing command exited 0` and exits 1; against the
  current runner it passes. Both directions were run.

The lesson is the project's own rule, applied to the thing doing the checking:
a harness that has not been shown to go red is not a safety net, and that
includes the harness runner.

The five migration risks (R-1 to R-5) were mutated earlier, against the test
suite, and that table is in `docs/GOLDEN-VALIDATION.md` §2. It still holds:
those mutations are caught by the `test` group.

## Runtime, and what it costs

A full local run on the development host, from an empty `target/`: **2 m 52 s**
for all eight groups. In CI the jobs run in parallel and each also pays an
`apt-get install` and a `cargo` build of its own.

## What was rehearsed, and what was not

Every command in `scripts/run-checks.sh` was run on the development host,
which is Debian trixie with libpappl 1.3.1-2.1+b2 — the same distribution and
the same library version the container job uses — under both toolchains:
trixie's packaged rustc 1.85.0 and rustup's 1.95.0. All eight groups pass
under both.

`--self-test` was run against both a correct and a deliberately broken runner,
as described above.

**The workflow file itself has not been executed.** There is no container
runtime on the development host (no `docker`, no `podman`, no `act`), so the
YAML, the `apt-get` package lists and the `actions/checkout` step in a bare
`debian:trixie` image are unverified; the first real run will be the one on
GitHub. The YAML parses, and its job and trigger structure was checked with a
parser rather than by eye. If the first push goes red in a step that installs
packages rather than in a step that runs the script, that is why.
