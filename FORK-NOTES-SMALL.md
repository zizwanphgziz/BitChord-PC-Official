# Fork notes — SMALL features (sleep timer, playback speed, ListenBrainz, DAC output)

Branch `rust-2.0`, uncommitted working-tree changes only. No GitHub push/fork.
Owner files: `src/audio.rs`, `src/ui/playback.rs`, `src/ui/pages.rs` (own sections
only), `src/main.rs` (mod registrations + the `audio::spawn` call), plus new
`src/sleep_timer.rs` and `src/listenbrainz.rs`. No `Cargo.toml` changes, no new
dependencies. `src/lastfm.rs`, `src/hires.rs`, `src/backend.rs`, `src/locallib.rs`,
`src/theme.rs`, `src/lyrics.rs`, `src/ui/nowplaying.rs`, `src/ui/responses.rs`
(except the one-line Finished hook, H5 below) are untouched in behaviour.

## 0. What the investigation found (before writing code)

- **rodio 0.22.2 HAS built-in speed control.** `Player::set_speed(f32)` exists
  (`rodio-0.22.2/src/player.rs:204`); the player's 5 ms periodic callback
  re-factors the `Speed` wrapper around the running source, so it applies to the
  track playing now, not just the next one. `Speed` works by reporting a
  sample rate multiplied by the factor (`source/speed.rs`), so the pitch rises
  with the tempo: this is a tempo control, not time-stretching. Position stays
  in song time (`track_position` sits outside the `Speed` wrapper), and
  `Speed::try_seek` multiplies the seek position by the factor, so seeks stay
  correct while a speed is set.
- **rodio 0.22.2 has NO silence detection or trimming.** `source/skip.rs` is
  skip-a-duration, not silence; there is no silence/trim source in the crate.
  See §5 for why skip-silence was not implemented.
- **Output-device selection already existed** (`audio::output_devices`,
  `AudioCmd::SetDevice`, the Settings picker in `pages.rs`). What was missing
  was any control over the rate the mixer runs at.
- **cpal 0.17.3 (what rodio 0.22.2 uses) has no exclusive-mode API.** It opens
  the device by its enumerated name; on Linux the `default` device goes through
  PipeWire/PulseAudio, which shares and resamples. There is no way through
  rodio/cpal to ask for exclusive access or to force a raw `hw:` device — the
  picker can only offer what cpal enumerates. See §4.
- **Stats for nerds already exists upstream** and was not touched:
  `nerd_stats()` in `src/ui/nowplaying.rs:1114`, gated on
  `app.show_nerd_stats`, toggle at `src/ui/pages.rs:1879`, showing codec / bit
  depth / sample rate / bitrate / channels. Verified still present after these
  changes (the toggle and the function are byte-identical to upstream).

## 1. Sleep timer (`src/sleep_timer.rs`, new)

- Pure `SleepTimer` state + a process-wide lock holding it, mirroring
  `lastfm.rs`'s global-state pattern so both the UI and the audio thread can
  see it without either owning it. Nothing here touches egui.
- Presets 15 / 30 / 45 / 60 / 90 min (`PRESETS_MIN`) plus "After this track"
  plus Cancel. Minutes timers pause playback when the deadline passes
  (one-shot); "After this track" pauses when the current track ends naturally.
  The app is never quit.
- Last chosen preset is remembered in `sleep_timer_preset` in the data dir (a
  plain file, like the backend's `auto_upgrade` flag) and offered highlighted
  next time.
- Firing has two paths, either of which is enough, and the flag is one-shot so
  only one fires:
  - UI tick: `App::tick_sleep_timer` (`src/ui/playback.rs`), called from
    `logic` (hook H3). Sends `AudioCmd::Pause` and toasts.
  - Audio-thread backstop: the audio loop checks `take_due()` every iteration
    while a song plays and pauses itself (`src/audio.rs`, in `run`).
  Both check `playing` before consuming the flag: a timer that runs out while
  paused stays armed and fires as soon as playback resumes, instead of
  clearing silently.
- "After this track" plumbing: while one is armed the audio thread does NOT
  send the crossfade `Ending` event (else the next song would start before this
  one ends), and as the track finishes naturally it sets `stop_pending` before
  sending `Finished`. The UI consumes that flag in `App::next_after_end`
  (hook H5) instead of advancing the queue, and the guard at the top of
  `play_current_at` covers the repeat-one path, which calls `play_current`
  directly. A download that fails mid-song does NOT set the flag (only the
  natural-finish branch does), so a broken stream still recovers.
- UI: a Sleep timer block in Settings › Playback (`src/ui/pages.rs`,
  `sleep_timer_editor`), showing the countdown (`request_repaint_after(1 s)`
  while one runs) and a Cancel pill. A timer set with nothing playing waits for
  a song (the row says so). No quick control in the player ⋯ menu yet: that
  menu lives in `src/ui/nowplaying.rs`, which is outside the owned files — see
  hook H6 for the one-line addition if wanted.
- Tests in `sleep_timer.rs`: countdown maths, one-shot firing, after-track
  having no countdown, cancel keeping the last preset, remaining labels.

## 2. Playback speed 0.5–2× (`src/audio.rs`, `src/ui/playback.rs`, `src/ui/pages.rs`)

- `AudioStatus.speed` (f32 bits) + `AudioHandle::speed()` / `set_speed()` +
  `AudioCmd::SetSpeed(f32)`. The audio loop keeps a local `speed`, applies it
  to the first player and to every player a handoff or an upgrade swaps in
  (a fresh `Player` starts at 1.0), and to the fading tail under a crossfade so
  the blend stays in step.
- Speed applies to the currently playing track immediately (rodio's 5 ms
  callback), and stays put across songs. It is persisted in eframe storage
  under `playback_speed` (load in `App::new`, save in `App::save`, hooks H1/H2)
  and clamped to 0.5–2× (`clamp_speed`, NaN/inf → 1.0).
- UI: a Playback speed row in Settings › Playback with preset pills
  0.5 / 0.75 / 1 / 1.25 / 1.5 / 2.0×, and `App::set_speed` / `App::speed` in
  `src/ui/playback.rs` so the player ⋯ menu can use the same call — the menu
  itself is in `src/ui/nowplaying.rs` (not owned), see hook H6.
- The pitch rises with the tempo (rodio resamples by lying about the rate).
  The Settings row says so in plain words.
- Tests in `audio.rs`: clamping (including NaN/inf), `set_speed` storing the
  value and sending the command.

### Skip silence — NOT implemented, deliberately

- rodio 0.22 has no silence detector or trim wrapper, so one would have to be
  written around the decoder.
- Trailing silence needs a lookahead buffer (you only know silence is trailing
  once louder audio follows, or the stream ends). That buffer delays the
  samples the position clock counts (`player.get_pos()` counts emitted
  samples), so the progress bar and the crossfade `Ending` timing would lag by
  the buffer, and the fade-out tail would be cut.
- Leading silence needs a trim offset added to every seek (`Speed::try_seek`
  multiplies by the speed factor; the offset would have to be re-derived after
  every seek, since it is only known once the leading silence has been
  scanned). Seeking is core here (progress drag, SponsorBlock, jam sync).
- A wrapper would sit inside `Meter`'s chain, where tempo analysis and the
  crossfade ramps also live. The risk to gapless/crossfade behaviour outweighs
  a feature that mostly saves a second of quiet intro. No Settings toggle was
  added for a feature that does not exist. If it is ever wanted, the safe shape
  is a trim that only ever drops leading silence and reports the trim offset to
  the seek path — a bigger, audio-thread-owned change, not a wrapper.

## 3. ListenBrainz scrobbling (`src/listenbrainz.rs`, new)

- A mirror of `src/lastfm.rs`, which was read first and not changed: a
  `Scrobble` record (reuses `lastfm::Scrobble` — one record type, not two), a
  process-wide state lock, a one-at-a-time send mutex, an offline queue file
  `listenbrainz_queue.json` next to `lastfm_queue.json` (removed when empty,
  capped at 1000 entries with the oldest dropped and a log line), batch sends
  of up to 50.
- Token in the app vault under `listenbrainz` (the same mechanism Last.fm's
  session key uses); the enable toggle in a small `listenbrainz_enabled` file
  in the data dir. Default OFF.
- `POST https://api.listenbrainz.org/1/submit-listens`, header
  `Authorization: Token <user-token>`, body `{listen_type, payload}`: one
  listen goes as `"single"`, several as `"import"`; `playing_now` has no
  `listened_at`. `additional_info.duration` is included when known.
- Scrobble rule, same as Last.fm's: played half its length or 4 minutes,
  songs under 30 s don't count. Implemented in `App::sync_listenbrainz`
  (`src/ui/playback.rs`, hook H3), with the per-generation state
  (`note_started` / `note_scrobbled`) inside the module, mirroring
  `App::scrobble` for Last.fm. Failed sends are logged and kept for later;
  they never block playback (everything runs on the backend's network tasks).
- UI in Settings › Connections (`src/ui/pages.rs`): enable toggle, a masked
  user-token field (`masked_settings_field`, `egui::TextEdit::password(true)`),
  and a Test button that runs `GET .../1/validate-token` with the token
  (`App::listenbrainz_test` in `src/ui/playback.rs`); the answer is toasted by
  `sync_listenbrainz` and shown inline under the row until the next test.
- Tests in `listenbrainz.rs`: single → `"single"` with `listened_at`,
  several → `"import"`, `playing_now` has no `listened_at`, missing album /
  duration are omitted (not null), queue JSON round trip.

## 4. Bit-perfect USB DAC output — what is real, and what is not

Investigated in the vendored sources (`rodio-0.22.2/src/stream.rs`,
`cpal-0.17.3`):

- The app already picks its output device (`AudioCmd::SetDevice`, Settings
  picker). The mixer is built once at launch at the device's default rate
  (`Output::new`), and the sink is opened from the device's default config
  (`DeviceSinkBuilder::from_device` → `open_sink_or_fallback`).
- What rodio DOES expose: `DeviceSinkBuilder::with_sample_rate` (public) and
  `rodio::stream::supported_output_configs(device)` (public), and cpal's
  `Device::supported_output_configs()` ranges (`min_sample_rate` /
  `max_sample_rate`).
- Implemented: an **output sample rate** setting. `audio::spawn` takes a
  `sample_rate: Option<u32>` (hook H4 in `main.rs` reads it from eframe
  storage); `Output::new` mixes at the picked rate when the device supports
  it (checked against the cpal ranges) and `Output::open` asks the sink for
  the same rate via `with_sample_rate`, falling back through the device's
  supported configs if it refuses. `audio::supported_rates(device)` lists the
  device's real rates for the picker. The pick is kept in `AudioStatus`
  (`sample_rate_pref`, for persistence, and `sample_rate_active`, the rate this
  launch actually runs at); Settings offers a "Restart now" pill (the same
  pattern as the proxy/renderer rows) when the two differ.
- What this buys: a 96 kHz FLAC played with the mixer at 96 kHz goes
  decoder → mixer (no resample) → sink (no resample) → device. A per-track
  native rate is NOT possible without rebuilding the mixer for every track,
  which would stop playback and break crossfades (two players share one
  mixer); the mixer rate is therefore one rate per launch, picked by the
  listener.
- What is NOT achievable through rodio/cpal, and was not claimed: **true
  exclusive-mode bit-perfect**. cpal 0.17 has no exclusive-mode API; on Linux
  it opens the device by its enumerated name, and the `default` device goes
  through PipeWire/PulseAudio, which shares the device and resamples in its
  own mixer. Picking a `hw:` device in the existing picker (when cpal
  enumerates one) bypasses dmix, but cpal still opens it in shared mode as far
  as its API goes, and there is no way to verify bit-perfectness from inside
  the app. Getting there would mean dropping to raw ALSA (the `alsa` crate,
  NOT a current dependency — not added, per the task) and owning the device
  open, the format negotiation and the resampling policy outright. The Settings
  row says the rate avoids resampling; it does not say "bit-perfect".

## 5. Stats for nerds — verified, not implemented

Already upstream: `nerd_stats()` at `src/ui/nowplaying.rs:1114`, gated on
`app.show_nerd_stats`, toggle at `src/ui/pages.rs:1879`, showing codec / bit
depth / sample rate / bitrate / channels. Untouched by these changes.

## Hooks (outside the owned files — parent to apply)

H1 — `src/ui/mod.rs`, `App::new`, next to `crate::lastfm::init(...)` (already
applied in the verification run, reverted after — see Verification):
```rust
crate::listenbrainz::init(backend.data_dir());
crate::sleep_timer::init(backend.data_dir());
```
and after `app.request_liked(false);`:
```rust
if let Some(v) = get("playback_speed").and_then(|v| v.parse::<f32>().ok()) {
    app.audio.set_speed(v);
}
```

H2 — `src/ui/mod.rs`, `App::save`, after the `disable_direct_stream` line:
```rust
storage.set_string("playback_speed", self.audio.speed().to_string());
storage.set_string("output_sample_rate", self.audio.sample_rate_pref().map_or(String::new(), |r| r.to_string()));
```

H3 — `src/ui/mod.rs`, `logic`, after `self.sync_lastfm();`:
```rust
self.tick_sleep_timer();
self.sync_listenbrainz();
```

H4 — `src/main.rs`, where the audio thread is spawned (already applied and KEPT,
because `audio::spawn`'s signature changed and the tree must compile):
```rust
let sample_rate = cc.storage.and_then(|s| s.get_string("output_sample_rate")).and_then(|v| v.parse().ok());
let audio = audio::spawn(volume, log_volume, device, sample_rate, audio_tx, wake);
```

H5 — `src/ui/responses.rs`, the `Finished` arm: `self.next();` →
`self.next_after_end();` (the repeat-one path is already guarded inside
`play_current_at`, so only this one line is needed).

H6 — optional, `src/ui/nowplaying.rs`, `options_menu`, next to the
"Upgrade quality" action: a speed section using the `App::set_speed` /
`App::speed` helpers (in `src/ui/playback.rs`), e.g. a "Speed" caption with
one `menu_action` row per preset in `crate::audio::SPEED_PRESETS`, and a
"Sleep timer" row that opens Settings › Playback. Not written because
`nowplaying.rs` is outside the owned files; the Settings rows work without it.

## Verification

- `cargo check` with `CARGO_TARGET_DIR=.../target-c`: **exit 0** (first full
  check took ~15 min on this VM's 1.5 shared CPUs; incremental re-checks take
  seconds). 53 warnings with the hooks applied, 69 with them reverted — the
  extra 16 are all "never used" on the hook entry points (the `tick_sleep_timer`
  / `sync_listenbrainz` / `next_after_end` methods, the `listenbrainz` /
  `sleep_timer` module functions nothing calls yet), which vanish once the
  parent applies H1/H3/H5. The one `Instant` unused-import warning sits in
  upstream `src/motion.rs`.
- `cargo test --bin bitchord`: **215 passed, 0 failed, 24 ignored** (baseline
  204 + 11 new: 4 sleep-timer, 5 ListenBrainz, 2 speed). Targeted run first:
  `cargo test --bin bitchord -- sleep_timer listenbrainz audio::tests` →
  26 passed, 0 failed. The sleep-timer (4/4) and ListenBrainz payload (5/5)
  tests were also run standalone in /tmp scratch crates while the test build
  was blocked (see below); the numbers above are from the real tree.
- Environment wrangling (not code): the test build first failed on missing
  system dev packages (`gdk-pixbuf-2.0`, `gtk+-3.0`, `javascriptcoregtk-4.1`,
  `webkit2gtk-4.1` — only the runtime libs remained; the same wipe the MEDIUM
  notes hit). `cargo check` passes without them (cached build-script outputs),
  but the test build re-runs the `gdk-pixbuf-sys` build script and fails on
  pkg-config. Installing them needed two fixes: a platform `apt-get update`
  (pid 792, the OS-intent replay) held the apt lock for ~10 min, and the
  install then failed on the known `libzstd1` conflict, fixed the way the
  MEDIUM notes record (`--allow-downgrades libzstd1=1.5.5+dfsg2-2build1.1`).
  All four `-dev` packages are now installed and pkg-config resolves them.
- Headless VM: the GUI itself was not run. Settings rows, the player-menu hook
  and the DAC rate path are compile-checked only, not eyeballed; the sample-rate
  list on this VM is empty (no audio devices here), so `supported_rates`
  returning `[]` on a device-less machine is expected, not a failure.
- The hook lines H1/H2/H3/H5 were applied temporarily to verify the integrated
  tree, then reverted (except H4, which stays — see above). Until the parent
  applies them, the features compile but sit idle: the timer never ticks, the
  ListenBrainz sync never runs, and the speed/sample-rate picks are not
  persisted across launches.
