# Crate Publishing Tracker

Tracks which workspace crates are published to crates.io. Tick a box once the crate is live
on crates.io, and fill in the version and date.

- Shared version across all crates (currently `0.1.2`); see README "Publish order".
- Publish list is defined by `publish = ...` in `release-plz.toml` and each crate's `Cargo.toml`.
- Checked against crates.io 2026-11-04 (`https://crates.io/api/v1/crates/<name>`): all 18 publishable
  crates are live at 0.1.2. Re-check with the API above; `cargo search` only lists a few results.
- `cargo publish --workspace --dry-run` passed for all 18 crates at 0.1.2 on 2026-11-04. The real publish
  hit the crates.io new-crate rate limit (about 1 per 10 minutes after the first 5), so it took hours.
  `cargo publish` errors on an already-published crate, so resume with `-p <crate>` one at a time.
  demux<->mux is a dev-dependency cycle, broken with path-only (version-less) dev-deps, which cargo
  strips on publish.
- Git tags `v0.1.0`, `v0.1.1` and `v0.1.2` exist; `v0.1.2` matches the published crates.

## Will be published (20 workspace members → 18 published)

Grouped by dependency wave: publish a wave only after every crate in the earlier waves is live.

### Wave 1 — no internal dependencies

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [x] | `tpt-kinetix-core` | — | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-kg` | — | 0.1.2 | 2026-11-04 |

### Wave 2 — depend on `core`

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [x] | `tpt-kinetix-demux` | core | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-mux` | core | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-av1` | core | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-vp9` | core | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-bitstream` | core | 0.1.2 | 2026-11-04 |

### Wave 3 — depend on wave 2

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [x] | `tpt-kinetix-lean` | core, bitstream | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-lossless` | core, bitstream | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-realtime` | core, bitstream | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-screen` | core, bitstream | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-vision` | core, bitstream | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-face` | core, bitstream | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-volumetric` | core, bitstream | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-package` | core, demux, mux | 0.1.2 | 2026-11-04 |
| [x] | `tpt-kinetix-pipeline` | core, demux, vp9, av1 | 0.1.2 | 2026-11-04 |

### Wave 4 — depends on wave 3

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [x] | `tpt-kinetix-stream` | core, demux, package | 0.1.2 | 2026-11-04 |

### Wave 5 — final

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [x] | `tpt-kinetix-cli` | core, demux, mux, package, vp9, av1, vision, bitstream, pipeline, stream | 0.1.2 | 2026-11-04 |

## Will NOT be published

| Crate | Reason |
|---|---|
| `out-kinetix-h264` | Patent-encumbered (encode and decode). `publish = false`; `out-` prefix marks it as outside the published set. See [PATENTS.md](PATENTS.md). Enforced by `just check-publish-safe`. |
| `tpt-kinetix-test-utils` | Internal test helpers. `publish = false`, `release = false`. Depends on `out-kinetix-h264`, so it must never become publishable. |

## Name reservation

Reserve names before the first real publish (see README "crates.io name reservation"). Tick when
the name is claimed on crates.io (a real release also counts).

- [x] `tpt-kinetix-core`
- [x] `tpt-kinetix-kg`
- [x] `tpt-kinetix-demux`
- [x] `tpt-kinetix-mux`
- [x] `tpt-kinetix-av1`
- [x] `tpt-kinetix-vp9`
- [x] `tpt-kinetix-bitstream`
- [x] `tpt-kinetix-lean`
- [x] `tpt-kinetix-lossless`
- [x] `tpt-kinetix-realtime`
- [x] `tpt-kinetix-screen`
- [x] `tpt-kinetix-vision`
- [x] `tpt-kinetix-face`
- [x] `tpt-kinetix-volumetric`
- [x] `tpt-kinetix-package`
- [x] `tpt-kinetix-pipeline`
- [x] `tpt-kinetix-stream`
- [x] `tpt-kinetix-cli`

## Notes / known gaps

- README "Publish order" and "name reservation" now list all 18 publishable crates, so the
  original codec crates are covered. `release-plz.toml` marks them `publish = true` to match.
- `tpt-kinetix-stream` has `tpt-kinetix-test-utils` as a path-only `[dev-dependencies]` entry
  (no `version`), so it is stripped on publish. Still verify with
  `cargo publish --dry-run -p tpt-kinetix-stream`.
- Before each publish: `just check`, `just check-publish-safe`, `cargo publish --dry-run -p <crate>`.
