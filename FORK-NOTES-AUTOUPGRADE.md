# Fork notes — auto-upgrade quality, MadFLAC pre-config, badge numbers, addon health-check

Implemented in the working tree (uncommitted, branch `rust-2.0`). No new
dependencies, no Cargo.toml changes, no request signing, no change to the
FLAC range/download path (the 1-byte `bytes=0-0` probe still works as before).

## 1. Auto-upgrade quality — now retries while the song plays

**Why it was unreliable.** `try_upgrade_hires` (`src/backend.rs`) asked the
addons exactly once, inside a single 20 s `HIRES_WAIT` timeout, and gave up.
A cold addon spends that budget before its stream call even starts
(manifest 8 s, then search 8 s, then stream 18 s, each with its own
timeout), so the first ask timed out with the copy still on its way — and
nothing ever asked again for that song.

**What changed (`src/backend.rs`).**
- `try_upgrade_hires` now asks up to 3 times (`UPGRADE_ATTEMPTS`) with
  escalating per-attempt waits (20 s / 30 s / 45 s, `UPGRADE_WAITS`) and
  pauses between attempts (5 s / 10 s, `UPGRADE_BACKOFF`). The manifest and
  search are cached per addon (`hires::Client`), so later attempts are
  mostly the stream call itself — a cold lookup usually lands on attempt 2.
- It stops asking when the song is nearly over (`UPGRADE_MIN_REMAINING`
  20 s, via `upgrade_worthwhile`), when the song is skipped past
  (generation changed), or when the listener reverts it to YouTube.
- The "Upgrading quality" state already existed in the player UI
  (`src/ui/nowplaying.rs`, `Resp::UpgradingQuality`); the guard in
  `try_upgrade_hires` now spans the whole retry run, so the badge stays up
  while retries are pending and clears when the swap lands (via
  `Resp::AudioInfo`) or definitively fails (guard drop).
- The swap itself is unchanged: it reuses `AudioCmd::Upgrade`
  (`src/audio.rs`), which keeps the current position and the play/pause
  state and crossfades quickly. No change to `src/audio.rs` was needed.
- New setting: automatic upgrades can be turned off (default ON).
  `Backend::auto_upgrade()` / `set_auto_upgrade(bool)`; the choice is kept
  in `data_dir/auto_upgrade` (`"1"`/`"0"`, missing = on). The automatic
  spawn in `start_playback` is gated on it; the manual "Upgrade quality"
  menu action (`try_upgrade_active`) ignores the setting, and the manual
  upgrade/revert behaviour is otherwise untouched.

**UI hook still needed (not in owned files):** a toggle in
Settings › Playback — see "Hooks" below.

## 2. MadFLAC pre-config (`src/main.rs`, `src/hires.rs`)

- `hires::MADFLAC_MANIFEST_URL` =
  `https://madflac.madvpn.dpdns.org:8443/manifest.json`.
- `seed_madflac()` runs in `main()` before eframe loads its settings. On
  first run, if no hi-res sources are configured, the list starts with the
  MadFLAC manifest. The `madflac_seeded` flag is written at the same time,
  so the URL is never re-added — including when the list is empty because
  the listener cleared it. The seeded line stays fully removable/editable
  in Settings › Playback like any other source.
- Why the seed edits `app.ron` directly: eframe 0.37 hands `main` only a
  shared (`&dyn Storage`) reference to its storage, and `set_string`
  needs `&mut`. The seed is a line-based edit of the pretty-printed RON
  map in `data_dir/app.ron`, with strict shape checks — any file that
  isn't the expected shape is left alone rather than risked. The pre-seed
  file is kept once as `app.ron.bak-madflac-seed`. A missing or empty
  `app.ron` gets a minimal valid file with just the two keys.
- Pure helper `seed_madflac_in_ron` is unit-tested (empty map, existing
  sources kept, empty source filled in, seeded/cleared file left alone,
  unexpected shape left alone).

## 3. Badge numbers (`src/hires.rs`, `src/ui/nowplaying.rs`)

- New `Info::badge_line()`: badge plus sample rate and bit depth when
  known, e.g. `Lossless · 44.1 kHz · 16-bit`,
  `Hi-Res Lossless · 96 kHz · 24-bit`; unknown numbers are omitted, so a
  bare badge is still the truth. The kHz formatting is shared with
  `details_line()` via `fmt_khz`.
- The now-playing badge (`audio_badge` and `audio_badge_width`) shows
  `badge_line()` instead of the bare badge. The Dolby Atmos lockup stands
  alone as before, and the badge's details popup already listed the
  numbers — it is unchanged.

## 4. Addon health-check (`src/hires.rs`, `src/ui/responses.rs`)

- `hires::check()` (the existing settings-row test, run in the background
  via `Req::CheckHiResSource` — the UI thread is never blocked) now
  reports a health status: the addon's version from its manifest
  (`version` field, when published) and the round-trip time of the whole
  test, e.g. `MadFLAC · v1.2.2 · Tidal · plays Hi-Res Lossless · 3.4 s`.
  The manifest's `version` is cached with the rest of the manifest.
- New `App::hires_retest(link)` (`src/ui/responses.rs`): forgets the last
  verdict and asks afresh; the new verdict lands back in `hires_checks`,
  so the settings row shows the new status.
- The per-addon Test button itself lives in the settings UI — see "Hooks".

## Hooks (needed outside the owned files)

1. **Settings › Playback — auto-upgrade toggle** (`src/ui/pages.rs`,
   after the "Disable direct source stream" row):
   ```rust
   setting_row(ui, "Auto-upgrade quality", Some("When a song starts on YouTube audio, keep looking for its hi-res copy in the background and switch to it when it arrives. Off: hi-res is only used when you ask for it."), |ui| {
       let mut on = app.backend.auto_upgrade();
       if toggle(ui, &mut on, "Auto-upgrade quality").changed() {
           app.backend.set_auto_upgrade(on);
       }
   });
   ```
2. **Settings › Playback — Test button per addon**
   (`src/ui/pages.rs`, `hires_sources_editor`, in the row layout next to
   the × remove button):
   ```rust
   if pill(ui, "Test", None, false).clicked() {
       app.hires_retest(&link);
   }
   ```
   (`link` is already the trimmed line in that loop; the verdict line
   below the row shows the new status, including version and RTT, once
   the background check answers. `hires_retest` is defined in
   `src/ui/responses.rs`.)

## Verification

- `cargo check` with `CARGO_TARGET_DIR=.../target-a` (see report for output).
- `cargo test` for the touched modules (`hires::tests`, `seed_tests`,
  `backend::explore_tests`).
- Headless VM: the GUI itself was not run.
