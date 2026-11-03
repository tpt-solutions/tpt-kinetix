# Crate Publishing Tracker

Tracks which workspace crates are published to crates.io. Tick a box once the crate is live
on crates.io, and fill in the version and date.

- Shared version across all crates (currently `0.1.1`); see README "Publish order".
- Publish list is defined by `publish = ...` in `release-plz.toml` and each crate's `Cargo.toml`.
- Checked against crates.io 2026-11-04 (`https://crates.io/api/v1/crates/<name>`):
  `tpt-kinetix-core` 0.1.1 (and 0.1.0) and `tpt-kinetix-demux` 0.1.1 are live. The other 16
  publishable crates return 404. Re-check with the API above; `cargo search` only lists a few results.
- `cargo publish --workspace --dry-run` passed for all 18 crates at 0.1.1 on 2026-10-08. Not re-run
  since. It publishes in dependency order, so the per-wave tables below are informational.
  demux<->mux is a dev-dependency cycle, broken with path-only (version-less) dev-deps, which cargo
  strips on publish.
- Git tags `v0.1.0` and `v0.1.1` exist, but a tag alone does not mean the crates were published.

## Will be published (20 workspace members → 18 published)

Grouped by dependency wave: publish a wave only after every crate in the earlier waves is live.

### Wave 1 — no internal dependencies

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [x] | `tpt-kinetix-core` | — | 0.1.1 | 2026-10-07 |
| [ ] | `tpt-kinetix-kg` | — | | |

### Wave 2 — depend on `core`

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [x] | `tpt-kinetix-demux` | core | 0.1.1 | 2026-10-07 |
| [ ] | `tpt-kinetix-mux` | core | | |
| [ ] | `tpt-kinetix-av1` | core | | |
| [ ] | `tpt-kinetix-vp9` | core | | |
| [ ] | `tpt-kinetix-bitstream` | core | | |

### Wave 3 — depend on wave 2

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [ ] | `tpt-kinetix-lean` | core, bitstream | | |
| [ ] | `tpt-kinetix-lossless` | core, bitstream | | |
| [ ] | `tpt-kinetix-realtime` | core, bitstream | | |
| [ ] | `tpt-kinetix-screen` | core, bitstream | | |
| [ ] | `tpt-kinetix-vision` | core, bitstream | | |
| [ ] | `tpt-kinetix-face` | core, bitstream | | |
| [ ] | `tpt-kinetix-volumetric` | core, bitstream | | |
| [ ] | `tpt-kinetix-package` | core, demux, mux | | |
| [ ] | `tpt-kinetix-pipeline` | core, demux, vp9, av1 | | |

### Wave 4 — depends on wave 3

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [ ] | `tpt-kinetix-stream` | core, demux, package | | |

### Wave 5 — final

| Done | Crate | Depends on | Version | Published on |
|:---:|---|---|---|---|
| [ ] | `tpt-kinetix-cli` | core, demux, mux, package, vp9, av1, vision, bitstream, pipeline, stream | | |

## Will NOT be published

| Crate | Reason |
|---|---|
| `out-kinetix-h264` | Patent-encumbered (encode and decode). `publish = false`; `out-` prefix marks it as outside the published set. See [PATENTS.md](PATENTS.md). Enforced by `just check-publish-safe`. |
| `tpt-kinetix-test-utils` | Internal test helpers. `publish = false`, `release = false`. Depends on `out-kinetix-h264`, so it must never become publishable. |

## Name reservation

Reserve names before the first real publish (see README "crates.io name reservation"). Tick when
the name is claimed on crates.io (a real release also counts).

- [x] `tpt-kinetix-core`
- [ ] `tpt-kinetix-kg`
- [x] `tpt-kinetix-demux`
- [ ] `tpt-kinetix-mux`
- [ ] `tpt-kinetix-av1`
- [ ] `tpt-kinetix-vp9`
- [ ] `tpt-kinetix-bitstream`
- [ ] `tpt-kinetix-lean`
- [ ] `tpt-kinetix-lossless`
- [ ] `tpt-kinetix-realtime`
- [ ] `tpt-kinetix-screen`
- [ ] `tpt-kinetix-vision`
- [ ] `tpt-kinetix-face`
- [ ] `tpt-kinetix-volumetric`
- [ ] `tpt-kinetix-package`
- [ ] `tpt-kinetix-pipeline`
- [ ] `tpt-kinetix-stream`
- [ ] `tpt-kinetix-cli`

## Notes / known gaps

- README "Publish order" and "name reservation" now list all 18 publishable crates, so the
  original codec crates are covered. `release-plz.toml` marks them `publish = true` to match.
- `tpt-kinetix-stream` has `tpt-kinetix-test-utils` as a path-only `[dev-dependencies]` entry
  (no `version`), so it is stripped on publish. Still verify with
  `cargo publish --dry-run -p tpt-kinetix-stream`.
- Before each publish: `just check`, `just check-publish-safe`, `cargo publish --dry-run -p <crate>`.
