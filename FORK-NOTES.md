# Fork notes — coordinator (integration, hooks, cleanup, verification)

Branch `rust-2.0`, uncommitted working-tree changes only. No GitHub push/fork.
Upstream: jackychan4891/BitChord-PC-Official, HEAD 595840cc ("Merge pull request
#35 from FumoRin/linux-font-fallback"). Version bumped 2.0.1 → **2.1.0**
(`Cargo.toml` + `installer/bitchord.iss`; `Cargo.lock` follows). App name and
data-dir paths deliberately NOT renamed — that is a branding decision for Mad.
`.github/workflows/tests.yml` now covers `rust-2.0` as well as `main`.

Wave notes (per-agent detail): `FORK-NOTES-AUTOUPGRADE.md`,
`FORK-NOTES-MEDIUM.md`, `FORK-NOTES-SMALL.md`.

## Hooks applied by the coordinator (all verified in the integrated tree)

- H1/H2/H3 (small wave): `src/ui/mod.rs` — `listenbrainz::init` +
  `sleep_timer::init` in `App::new`; `playback_speed` loaded in `App::new` and
  persisted in `App::save` together with `output_sample_rate`;
  `tick_sleep_timer()` + `sync_listenbrainz()` in `logic` after `sync_lastfm()`.
- H4 (small wave, kept by Agent C): `src/main.rs` passes the stored
  `output_sample_rate` into `audio::spawn` (signature changed, tree must compile).
- H5 (small wave): `src/ui/responses.rs` `Finished` arm — `self.next()` →
  `self.next_after_end()` (repeat-one path already guarded in `play_current_at`).
- H6 (coordinator, extra): `src/ui/nowplaying.rs` `options_menu` — a SPEED
  section (presets from `crate::audio::SPEED_PRESETS`, current one marked with
  `icon::CHECK`) plus a "Sleep timer…" row that opens Settings
  (`app.go(Page::Settings)`; Settings has no section jump, the timer lives
  under Settings › Playback). Needed `Page` added to the `use super::{...}`
  import. One fix while applying: `for &v in SPEED_PRESETS` is a type error
  (by-value array iteration yields `f32`) — written as `for v in ...`.
- Earlier hooks (waves 1–2): `mod locallib;`, `locallib::init` + dynamic-theme
  load/save in `src/ui/mod.rs`, `locallib::open` in `backend.rs open_stream`,
  auto-upgrade toggle + hires Test pill + lyrics Highlight dropdown + dynamic
  theme toggle in `src/ui/pages.rs`.

## Cleanup pass (coordinator) — fork adds ZERO new warnings

`cargo check` on the integrated tree: **44 warnings, exit 0**. Pristine
upstream baseline on the same toolchain: **45 warnings**. The fork is one
under baseline because H6 uses `icon::SETTINGS` (previously unused upstream).

Deleted as genuinely unused fork-added code (all verified unused by the
compiler, none reachable from live code):
- `theme.rs`: `Palette`, `Palette::fallback`, `palette_from_bytes`,
  `palette_from_rgb` + their 3 tests. These duplicated the live quantizer in
  `src/art.rs` (`art::palette`, used by the animated background + accent) and
  were only ever called by their own tests. See "Dynamic theme" below.
- `locallib.rs`: `count()` and `path_for()` (3-line helpers, nothing called
  them; trivially re-addable if a Library action ever needs them).
- Fixed 2 fork-added `egui::Id::new` deprecations (Agent B's `local_tab` /
  `set_local_tab`): `Id::new` → `Id::unique`, which the vendored egui source
  shows is the identical implementation, just renamed — zero behaviour change.
- Fixed 9 clippy findings on fork-added lines (all mechanical, verified by
  re-running clippy + the full test suite after): `is_multiple_of` ×2
  (`hires.rs` `fmt_khz`, `pages.rs` `fmt_rate_khz`), `sort_by_key`
  (`locallib.rs` rescan sort), 2 collapsible-ifs → let-chains (edition 2024)
  in `locallib.rs` metadata reading, `chunks_exact(2)` → `as_chunks::<2>()`
  (same drop-the-odd-byte behaviour), `repeat().take()` → `repeat_n`, `map_or`
  → `is_none_or` (`main.rs` RON check), and the `seed_tests` module in
  `main.rs` moved to end of file (it sat above `fn data_dir`, tripping
  `items_after_test_module`).

Deliberately NOT touched: the remaining 44 warnings are all pre-existing
upstream (16 egui deprecations, unused icon constants, `SEARCH_LIMIT`,
`preset::get`/`cover`, `mvod`, `wav`, etc.). Fixing those is a separate
upstream-cleanup job; the fork diff stays focused on features. Consequence:
`cargo clippy --all-targets -- -D warnings` (what upstream CI runs) still
fails on those pre-existing warnings — same as upstream main on this
toolchain. Verified by running clippy here (see Verification).

Added: one test in `src/art.rs` for the LIVE accent path
(`accent_follows_the_cover_colour`: red cover → red accent; grey cover →
lifted grey), replacing the coverage lost when the dead Palette tests were
deleted.

## Dynamic theme — verified live wiring (not just the toggle)

The first read suggested the accent never changes (App.accent starts as
`theme::RED`). That was wrong: `Resp::Art` (`src/ui/responses.rs`) sets
`self.accent = art.accent` for every track — `art::process` already extracts
the most vivid cover colour upstream. Agent B's change routes the default
preset's `"cover"` accent through `theme::dynamic_accent`: toggle ON (default)
= cover colour, contrast-lifted to WCAG 3:1; toggle OFF = the default red.
The feature is genuinely live; only the duplicate Palette quantizer was dead.

## What is NOT done / known limits

- **Skip silence — not implemented** (Agent C, deliberate): rodio 0.22 has no
  silence detector; a lookahead wrapper would lag the position clock the
  progress bar and crossfade timing depend on, and would need a trim offset
  re-derived on every seek. No toggle was added for a feature that does not
  exist. Documented in FORK-NOTES-SMALL.md §2.
- **True exclusive-mode bit-perfect DAC — not achievable via rodio/cpal**
  (Agent C, with evidence): cpal 0.17.3 has no exclusive-mode API. What
  shipped instead: an output sample-rate setting (mixer + sink at the picked
  rate, e.g. 96 kHz, so hi-res tracks reach the device unresampled; one rate
  per launch, since per-track rates would break crossfades). The Settings row
  says the rate avoids resampling; it never says "bit-perfect". The `alsa`
  crate would be needed for the real thing — NOT added, needs Mad's approval
  as a bigger job.
- **Stats for nerds — already existed upstream**, verified present, not
  re-implemented (`nerd_stats()` in `src/ui/nowplaying.rs:1114`, toggle in
  Settings).
- **Spotify live — out of scope** (per the task). **Request signing —
  impossible** (PC client cannot do official BitChord device verification;
  author says so). **Per-network ceilings — N/A on desktop.**

## Verification (final integrated tree, target-a)

- `cargo check --bin bitchord`: **exit 0, 44 warnings** (upstream baseline 45).
- `cargo test --bin bitchord`: **213 passed, 0 failed, 24 ignored**
  (204 baseline + 11 new from Agent C − 3 deleted Palette tests + 1 new
  art.rs test = 213). `TEST_EXIT:0`.
- `cargo clippy --all-targets -- -D warnings`: **fails with 75 errors, all on
  pre-existing upstream lines — 0 on fork-added lines** (verified by
  attributing every error location against the fork diff). The fork started at
  84 errors; the 9 on fork lines were fixed (see Cleanup). Upstream CI runs
  this command, so it presumably fails the same way on upstream main with this
  toolchain — fixing the remaining 75 upstream findings (collapsible ifs,
  too-many-arguments, `is_multiple_of`, egui deprecations, …) is a separate
  upstream-cleanup job, not a fork feature.
  (Clippy was not installed on this VM; installed via `rustup component add
  clippy` for this run.)
- **GUI never run** (headless VM): everything above is compile + unit tests.
  Mad must test on his PC: auto-upgrade swap timing, MadFLAC pre-config
  seeding on first launch, sleep timer firing, speed pills, ListenBrainz
  scrobbling against a real token, DAC rate picker on a real device, local
  library playback end-to-end.
- MadFLAC compatibility: the client sends the same range requests as before;
  the server-side 1-byte probe fix (MadFLAC 1.2.2) is untouched. No change to
  streaming/range behaviour in this fork.

## Building the Windows .exe

Upstream release workflow (`.github/workflows/release.yml`, unchanged):
push tag `v2.1.0` → Windows job checks `Cargo.toml` version ==
`installer/bitchord.iss` `AppVersion` == tag, runs
`cargo build --release --locked` (x64) and `--target aarch64-pc-windows-msvc`
(ARM64), stages `WebView2Loader.dll` next to each exe, builds the installer
with Inno Setup 6 (`ISCC.exe installer\bitchord.iss`, plus `/DArm64`), zips
the portable builds, and attaches everything to the GitHub release as a draft
until all files are attached. Linux + macOS jobs run the same way.
Local Windows build (same commands, on a Windows PC with Rust + Inno Setup):
`cargo build --release --locked`, then the ISCC step above.
Runtime note: the user's PC needs the WebView2 runtime (the DLL is staged by
the workflow; the runtime itself is a normal Edge WebView2 install, not a
build-time dependency).
