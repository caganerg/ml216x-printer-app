# P11 — the 1.x filter, deleted

**Taken on 2026-09-10 by the maintainer's decision.** This file was written
first as the list Q-5 required, then followed. It is kept as the record of what
went, what stayed, and what the deletion cost.

## The gate, and the decision to open it early

This list originally said: **P11 may not be taken until release gate G-1 has
been measured on paper** (`docs/G1-MEASUREMENT.md`), because the 1.x filter is
the reference the 2.0 output is judged against, and because Q-7 records that
the 1.x static musl binary is the only build that runs where the `.deb` cannot.

The maintainer chose to take it before that measurement. What that costs, so
the choice is legible later: comparing a suspect 2.0 page against the same
document printed through 1.x now needs `git checkout v1.x-final` and a build
from the tag, rather than a crate in this tree. The comparison remains
possible; it is one step further away. Nothing that the corpus proves was lost
— see the preparation below, which is why this was a deletion of front-end code
and not of evidence.

## Preparation, done first

The removal used to be entangled with the evidence. `src/golden.rs` — the
harness that freezes 32 SPL2 streams byte for byte — lived in the same package
as the filter and drove it through `crate::process_with_margin`, so a literal
`git rm` of the 1.x code would have deleted the corpus's only reader along with
it.

That is no longer true:

* The page loop moved to `crates/spl2-core/src/replay.rs`, behind the
  `golden-replay` feature (Q-6), unchanged. `src/main.rs` calls it through two
  wrappers and supplies the stderr sink.
* The harness moved to `crates/spl2-core/tests/golden.rs` and drives
  `spl2_core::replay` directly. All 32 goldens matched byte for byte across the
  move, `goldens/SHA256SUMS` did not change, and the filter's own 61 tests still
  pass.

So P11 is now a deletion of front-end code, not of evidence.

## What went

| Path | What it is |
|---|---|
| `src/main.rs` | argv parsing, stdin/stdout wiring, the stderr `Log` impl, and the filter's own test module |
| `Cargo.toml` root `[package]`, `[[bin]]` and `[dependencies]` | the `rastertospl-rust` package itself; the file becomes a virtual workspace manifest |
| `docs/LEGACY-FILTER.md` | how to build and run the reference |

`src/main.rs` was 1793 lines and 61 of them were `#[test]`. Almost none of
those tests were about being a filter, so they went to
`crates/spl2-core/tests/filter.rs` rather than to the bin: page-header
validation, the job budget, band geometry, duplex, and the PPD-versus-limits
cross-checks. All 61 pass there. What was actually deleted is argv parsing,
the stdin/stdout wiring and the stderr `Log` implementation.

## What stayed

| Path | Why it stays |
|---|---|
| `crates/spl2-core/src/replay.rs` | the loop the corpus is replayed through |
| `crates/spl2-core/tests/golden.rs`, `goldens/` | the byte-for-byte evidence; permanent |
| `ppd/samsung-ml2160.ppd` | permanent project data (Q-5), and the source of the hard-margin table |
| the `v1.x-final` tag | the recovery anchor; the 1.x line is reachable from it forever |

## What the deletion did

1. **Moved the tests worth keeping**, per test rather than in bulk: the whole
   module went to `crates/spl2-core/tests/filter.rs`, where it drives
   `replay::process` with a discarding log instead of the filter's stderr one.
2. **Turned the root manifest into a virtual workspace** and dropped
   `default-members`, which existed only to stop `cargo build` selecting the
   filter. `Cargo.lock` lost the `rastertospl-rust` entry.
3. **Repointed every reference**: `README.md`, `docs/NON-GOALS.md` and
   `docs/MARGINS.md` cited `src/main.rs` by name and by line number;
   `packaging/debian/copyright` named it in the SpliX derivation stanza, which
   now names `crates/spl2-core/src/replay.rs` instead — the transcribed
   material moved with the loop and the stanza must keep naming every file that
   carries it.
4. **Re-ran the full check list.** The goldens are untouched: no `.spl` file
   and no line of `goldens/SHA256SUMS` changed, which is the evidence that what
   was deleted was front-end code.

The PPD keeps its `*cupsFilter` lines naming `rastertospl-rust`. That is not an
oversight: the PPD describes the 1.x queue, is kept as project data rather than
installed, and rewriting it would falsify a record.

## What it does not do

It does not remove the SpliX derivation, the GPL-2.0-only licence, or the
`ppd/` directory, and it does not touch the goldens. The 1.x *line* is not
being abandoned in git: `v1.x-final` (tag object `7c0cf2c`, commit `33d4ff2`)
is annotated, pushed, and the only ref that names it.
