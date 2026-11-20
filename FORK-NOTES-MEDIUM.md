# Fork notes — MEDIUM features (word-synced lyrics, dynamic theme, local library)

Branch `rust-2.0`, uncommitted working-tree changes only. No GitHub push/fork.
Owner files: `src/lyrics.rs`, `src/ui/karaoke.rs`, `src/ui/lyric_prefs.rs`,
`src/theme.rs`, `src/preset.rs`, `src/locallib.rs` (new), `src/ui/pages.rs`
(Library page section only). No `Cargo.toml` changes, no new dependencies.

## 1. Word-synced lyrics — already parsed, now wired to a setting

Finding: enhanced-LRC word timing was **already** parsed into `Part`
(`lyrics.rs::parse_lrc` handles `<mm:ss.xx>` word stamps, including the
spaced-files case), and `ui/karaoke.rs` already rendered per-word
highlighting (`word_look` handles `Fill::Word` and `Fill::Sweep`; tokens get
per-word start/duration from `split_part`). What was missing was any way for
the listener to choose it: the theme preset's `Fill` (default `Sweep`) always
won, and there was no line-vs-word choice.

Changes:
- `src/ui/lyric_prefs.rs`: new `Highlight` enum (`Auto` / `Word` / `Line` /
  `Sweep`), stored as `lyric_highlight` (`auto` by default) in `Prefs`, with
  `Prefs::set_highlight()` and a process-wide current value
  (`highlight_current()`, same pattern as `theme::FONT_CHOICE`) so the
  karaoke renderer — which reads the preset style, not these prefs — can see
  it. `Highlight::fill(word_timed)` resolves: `Auto` → `Word` when the song
  has word timing, else `Line`; an explicit `Word` on line-only lyrics falls
  back to `Line`.
- `src/ui/karaoke.rs`: `Cfg::new` takes `word_timed` (from
  `lyrics.sync == Sync::Word` in `View::show`) and uses the resolved fill
  instead of the preset's `s.fill`. The theme's fill is still honoured when
  the listener picks `Sweep` explicitly.
- `src/lyrics.rs`: two new tests proving the data the renderer relies on
  (`enhanced_lrc_word_times_drive_word_sync`,
  `line_only_lrc_has_no_word_times`). Existing providers/fallbacks untouched.

Behaviour note: the default is now **word when word timing is available,
else line**, per spec — so on word-timed songs the default look changes from
the theme's per-character sweep to per-word lighting. Pick `Sweep` in
Settings › Lyrics to get the old look back.

## 2. Dynamic artwork-driven theming

Finding: the cover accent already flowed everywhere through
`Preset::accent(from_cover)` → `App::accent_color()` (progress bar, nav,
highlights, and the now-playing/page background tint in `shell.rs`), fed by
`art.rs::palette()` (most vivid colour, lifted for dark UI). What was missing
was a palette with dominant/muted/vibrant, a contrast guarantee, and an
on/off toggle.

Changes:
- `src/theme.rs`: `Palette { dominant, muted, vibrant, accent }`,
  `palette_from_bytes()` (quantizes the already-downloaded cover to 48×48,
  5-bit buckets; no network), `palette_from_rgb()` (testable core),
  `luminance()` / `contrast_ratio()` / `ensure_contrast()` (WCAG ratio;
  accents are lifted/darkened until they reach 3:1 against the background —
  accents are never body text, so 3:1 not 4.5:1), and the toggle itself:
  `dynamic_theme()` / `set_dynamic_theme()` (default ON),
  `load_dynamic_theme()` / `save_dynamic_theme()` (storage key
  `dynamic_theme`, `"0"` = off), `dynamic_accent()` (cover colour while on,
  `RED` while off). `Palette::fallback()` covers no-cover-loaded.
- `src/preset.rs`: `Preset::accent()` now routes cover-derived accents
  (`accent == "cover"`) through `theme::dynamic_accent()`; fixed-hex and
  `system` accents are untouched. With the toggle off, every cover-driven
  accent (including the background tint) falls back to the default pink.
- Tests in `theme.rs`: palette picks the cover colour, greyscale falls back
  to dominant, PNG-bytes round trip, contrast math, toggle gating.

## 3. Local music library (v1)

New `src/locallib.rs`, mirroring the `downloads.rs` global-state pattern
(no `App` field needed, so no `mod.rs` changes beyond the init hook):
- Folders: `folders()` / `add_folder()` / `remove_folder()` / `pick_folder()`
  (rfd picker on its own thread, like the download folder picker).
  Persisted with the scanned index in `local_library.json` in the data dir
  (`init()` / `save()`); a rescan re-reads tags each time (header-only, fast).
- Scan: `walk_audio()` (recursive, depth ≤ 16, symlink loops guarded),
  extensions FLAC / MP3 / M4A / AAC / OGG / OGA / OPUS / WAV.
- Metadata (`read_metadata()`): symphonia probe first (FLAC, M4A — the
  formats this build enables), then mp4ameta for M4A
  (title/artist/album/duration), then a minimal dependency-free ID3v2
  reader (TIT2/TPE1/TALB, Latin-1/UTF-16/UTF-8) plus ID3v1 fallback for
  MP3, then rodio's `Decoder::total_duration()` for the length of anything
  else (WAV, OGG). Filename fallback: `Artist - Title`, leading track
  number stripped.
- Playback: `LocalTrack::to_track()` (id `local:<fnv-1a of path>`,
  no thumbs) and `open(id)` returning a `Shared` buffer via
  `Format::File` — the same symphonia/rodio decoder path `stream::decoder`
  uses for files. Needs HOOK H3 (below) to be reached.
- `src/ui/pages.rs` (Library section only): a "Local files" chip beside the
  YTM tabs (kept in egui temp data, cleared when a YTM tab is picked). It
  works **without sign-in** — the login gate now sits below it. The section
  has Add folder / Rescan, the folder list with Remove, and the track list
  (Play / Shuffle / click-to-play, reusing the existing `list()` rows).
  The YTM tabs and their spacing are otherwise unchanged.
- Tests in `locallib.rs` (all real files, no mocks): stable ids, filename
  parsing, ID3v1, directory walk, a generated 2 s WAV scanning with
  filename tags + rodio duration, `open()` returning a `Shared` buffer,
  cache JSON round trip. FLAC/MP3/M4A tag paths were additionally verified
  against ffmpeg-generated files (see Verification).

## Verification (done 2026-11-20, after a VM restart that wiped /tmp and the apt-installed dev packages)

Environment wrangling (not code): the restart wiped `libgtk-3-dev`,
`libjavascriptcoregtk-4.1-dev` and `libwebkit2gtk-4.1-dev`, so the check
first failed on `javascriptcoregtk-4.1` pkg-config. Reinstalled all three
(the `libzstd1` version conflict needed the same
`--allow-downgrades libzstd1=1.5.5+dfsg2-2build1.1` fix the parent used
earlier). The baseline check had failed the same way before the restart,
so this was environmental, not the fork. `rustfmt` is not installed for
the toolchain, so no fmt check was possible.

- `cargo check` (own target dir `target-b`, with the temporary
  `mod locallib;` line in `main.rs`, removed again afterwards — `git diff
  src/main.rs` now shows only the other agent's changes): **exit 0**.
  63 warnings, all pre-existing (egui deprecations, unused items elsewhere)
  or "never used" on the new hook entry points (`locallib::init`/`open`,
  `theme::load/save_dynamic_theme`, `locallib::count`/`path_for`) — those
  warnings vanish once HOOKs H1–H5 are applied.
- `cargo test --bin bitchord -- locallib theme::tests lyric_prefs lyrics
  preset`: **29 passed, 0 failed** (199 other tests filtered out, not run).
- Scanner verified against real ffmpeg-generated files in `/tmp/loctest`
  (since wiped by the restart, but the run happened first): 3 s FLAC tagged
  Flac Song/Flac Artist/Flac Album, 5 s MP3 (ID3v2.3) tagged Mp3
  Song/Artist/Album, 7 s M4A tagged M4a Song/Artist/Album, 4 s OGG
  (duration only — tags fall back to filename, as documented), 2 s
  untagged WAV. All tags and durations read correctly through a temporary
  test that was removed after the run; the permanent test suite keeps the
  equivalent coverage with generated files.
- Bugs the tests caught and fixed during verification (all in owned files):
  `Format::File.mime` is `Option<String>` not `Option<&str>`; mp4ameta
  `title()`/`artist()`/`album()` return `Option<&str>`; symphonia's
  `Hint` method is `with_extension`, not `extension`; `eframe::Storage::
  set_string` needs an owned `String`; `preset::Fill` has no `Debug`, so
  the lyric-prefs test uses `matches!`; ID3v1's NUL padding now stripped
  in `clean()`; palette vibrant pick now breaks saturation ties by
  coverage (a 10% blue corner no longer beats 90% red).
- GUI not run (headless VM): the Library page, Settings rows and karaoke
  rendering are compile-checked only, not eyeballed. End-to-end local
  playback (click → H3 → decode) is untested live — it needs the GUI plus
  H1–H3 applied.

## HOOKS (outside owned files — parent to apply)

H1 — `src/main.rs`, register the module (alphabetical, between `login` and
`lyrics`):
```rust
mod locallib;
```

H2 — `src/ui/mod.rs`, `App::new`, right after the downloads init:
```rust
crate::locallib::init(backend.data_dir());
```
and near the other setting loads (next to the `theme::set_font_choice(...)` line):
```rust
crate::theme::load_dynamic_theme(&get);
```
and in `App::save` (eframe `save`), right after `self.lyric_prefs.save(storage);`:
```rust
crate::theme::save_dynamic_theme(storage);
```

H3 — `src/backend.rs`, `open_stream`, so local files actually play. The
existing code is:
```rust
let id = video_id.to_owned();
if let Some(shared) = tokio::task::spawn_blocking(move || crate::downloads::open(&id)).await? {
    return Ok(shared);
}
```
Add directly after it (before `self.resolve(...)`):
```rust
if let Some(shared) = tokio::task::spawn_blocking({
    let id = video_id.to_owned();
    move || crate::locallib::open(&id)
})
.await?
{
    return Ok(shared);
}
```
Without H3, clicking a local track falls through to the YTM resolver and
fails (the track id is `local:…`, not a video id). Downloads keep priority.

H4 — `src/ui/pages.rs`, Settings › Lyrics (`settings_lyrics`, NOT owned):
a highlight row after the "Text size" row, mirroring the "Translate into"
dropdown:
```rust
divider(ui);
setting_row(ui, "Highlight", Some("How the sung words light up. Automatic lights each word when the lyrics have word timing, and the whole line when they only have line timing."), |ui| {
    let current = app.lyric_prefs.highlight.label();
    let btn = dropdown_button(ui, current, 160.0, "Highlight");
    egui::Popup::menu(&btn).show(|ui| {
        ui.set_width(160.0);
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.add_space(4.0);
        for h in super::lyric_prefs::Highlight::ALL {
            if option_row(ui, h.label(), h == app.lyric_prefs.highlight).clicked() {
                app.lyric_prefs.set_highlight(h);
                ui.close();
            }
        }
        ui.add_space(4.0);
    });
});
```

H5 — `src/ui/pages.rs`, Settings › Appearance (`settings_appearance`):
a toggle after the "Animated background" row:
```rust
divider(ui);
setting_row(ui, "Dynamic theme from artwork", Some("Take the accent colour (highlights, progress bar, now-playing background tint) from the current cover art. Off, the default accent is used instead."), |ui| {
    let mut on = theme::dynamic_theme();
    toggle(ui, &mut on, "Dynamic theme from artwork");
    if on != theme::dynamic_theme() {
        theme::set_dynamic_theme(on);
    }
});
```

## Known limits / not done

- Local-library rescan is synchronous and re-reads tags every time (no
  size/mtime fast path yet); fine for a few hundred files, will block the UI
  briefly on a very large folder. A background scan is the obvious v1.1.
- OGG/Vorbis comments are not read (only duration via rodio); MP3 duration
  comes from rodio's header read, which can be `None` on odd files — those
  tracks show no length but still play.
- No cover art is read from local files (no embedded-art extraction); rows
  show the music-note fallback. `mp4ameta`/`symphonia` art is a v1.1 add.
- The dynamic-theme toggle and highlight setting have no UI until H4/H5
  are applied; until then both sit at their defaults (dynamic ON, highlight
  Automatic) and are fully functional.
- `Fill::Sweep` is still a valid explicit pick, but it is no longer any
  theme's effective default while highlight is Automatic.
