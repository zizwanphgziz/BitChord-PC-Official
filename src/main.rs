#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod appicon;
mod apple;
mod artist_signature;
mod fullscreen;
mod proxy;
mod renderer;
mod art;
mod audio;
mod backend;
mod bidi;
mod deeplink;
mod discord;
mod dash;
mod downloads;
mod hires;
mod images;
mod jam;
mod innertube;
mod jsc;
mod lastfm;
mod listenbrainz;
mod login;
mod locallib;
mod lyrics;
mod media;
mod model;
mod motion;
mod opus;
mod preset;
mod shader;
mod sleep_timer;
mod sponsorblock;
mod stream;
mod theme;
mod transfer;
mod translate;
mod tray;
mod turnstile;
mod ui;
mod update;
mod webmusic;
mod ytdlp;
mod ytm;
mod vault;
mod ytmapi;

use std::sync::{Arc, OnceLock};

use crossbeam_channel::unbounded;

/// Window size on first launch, and the smallest it may be (points).
pub const DEFAULT_SIZE: [f32; 2] = [1200.0, 780.0];
pub const MIN_SIZE: [f32; 2] = [760.0, 480.0];

fn main() -> eframe::Result {
    // Child-process modes for the webview windows (see login.rs / turnstile.rs).
    let mut args = std::env::args_os().skip(1);
    if let (Some(flag), Some(dir)) = (args.next(), args.next()) {
        // The parent hands its proxy setting down through the environment.
        proxy::init(std::path::Path::new(""));
        if flag == login::FLAG {
            login::child_main(login::Site::Google, dir.into());
            return Ok(());
        }
        if flag == login::SPOTIFY_FLAG {
            login::child_main(login::Site::Spotify, dir.into());
            return Ok(());
        }
        if flag == turnstile::FLAG {
            turnstile::child_main(dir.into());
            return Ok(());
        }
    }
    // A party link (`bitchord://party/CODE` or the https invite page): hand it to the running
    // instance if there is one, else join once this one is up. Before `init_logging`,
    // which would truncate the running instance's log file.
    if let Some(link) = deeplink::from_args() {
        if deeplink::forward_to_running(&link.url()) {
            return Ok(());
        }
        deeplink::push(link);
    } else if deeplink::forward_show() {
        return Ok(());
    }
    init_logging();
    update::clean_portable();
    stream::init_dev_hooks();
    proxy::init(&data_dir());

    // Dev hook: YTM_BACKGROUND opens the window off-screen, unfocused and off the
    // taskbar, so automated test runs don't pop up over whatever the user is doing.
    let background = std::env::var_os("YTM_BACKGROUND").is_some();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("BitChord")
        .with_app_id("bitchord")
        .with_inner_size(DEFAULT_SIZE)
        .with_min_inner_size(MIN_SIZE)
        // Windows: the app draws its own title bar (see `ui::titlebar`).
        .with_decorations(!ui::custom_titlebar())
        .with_icon(appicon::get(appicon::load(&data_dir())).icon_data().expect("bundled icons are valid PNGs"));
    if background {
        viewport = viewport.with_position([-4000.0, 0.0]).with_inner_size([1920.0, 1057.0]).with_active(false).with_taskbar(false);
    }
    let renderer = renderer::init(&data_dir());
    // First run: the hi-res addon list starts with MadFLAC (see `seed_madflac`).
    // This runs before eframe loads its settings, which this process only ever
    // gets a shared reference to.
    seed_madflac(&data_dir());
    let options = eframe::NativeOptions {
        viewport,
        renderer: match renderer {
            renderer::Renderer::OpenGl => eframe::Renderer::Glow,
            renderer::Renderer::Wgpu => eframe::Renderer::Wgpu,
        },
        persist_window: !background,
        // Settings live with the rest of the data: beside the portable exe, or a
        // YTM_DATA_DIR profile (e.g. a clean signed-out one for screenshots).
        persistence_path: Some(data_dir().join("app.ron")),
        // No MSAA/depth/stencil: egui doesn't need them and they cost VRAM.
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        // Dev hook: YTM_FPS paces frames itself (see `dev_pace`), so vsync is off.
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: std::env::var_os("YTM_FPS").is_none(), ..Default::default() },
        wgpu_options: renderer::wgpu_options(),
        ..Default::default()
    };

    let run = eframe::run_native(
        "BitChord",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            theme::install_style(&ctx);
            theme::install_fonts(&ctx, "inter");

            // Background threads wake the UI through this; set once the context exists.
            static CTX: OnceLock<egui::Context> = OnceLock::new();
            let _ = CTX.set(ctx.clone());
            let wake = || {
                if let Some(c) = CTX.get() {
                    c.request_repaint();
                }
            };

            // Party links from later launches arrive over a pipe (first instance only).
            deeplink::listen(wake);
            // Keep `bitchord:` links pointing at this copy; dev builds leave it alone
            // (even with YTM_UPDATE_FORCE).
            if update::installed() && !cfg!(debug_assertions) {
                deeplink::register_protocol();
            }

            let volume = cc
                .storage
                .and_then(|s| s.get_string("volume"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.8f32);
            // The slider is logarithmic unless turned off. A volume saved before that
            // existed was linear: move it to where it sounds the same.
            let log_setting = cc.storage.and_then(|s| s.get_string("log_volume"));
            let log_volume = log_setting.as_deref().is_none_or(|v| v == "1");
            let volume = if log_setting.is_none() { volume.cbrt() } else { volume };
            // Dev hook: YTM_MUTE starts silent and never persists the volume.
            let volume = if std::env::var_os("YTM_MUTE").is_some() { 0.0 } else { volume };

            let device = cc.storage.and_then(|s| s.get_string("output_device")).filter(|d| !d.is_empty());
            // The mixer rate, when the listener picked one (Settings › Playback ›
            // Output sample rate): the mixer is built once with it at launch.
            let sample_rate = cc.storage.and_then(|s| s.get_string("output_sample_rate")).and_then(|v| v.parse().ok());

            let (audio_tx, audio_rx) = unbounded();
            let audio = audio::spawn(volume, log_volume, device, sample_rate, audio_tx, wake);

            let data_dir = data_dir();
            let (resp_tx, resp_rx) = unbounded();
            let backend = backend::Backend::new(data_dir, resp_tx, audio.clone(), Arc::new(wake))?;

            egui_extras::install_image_loaders(&ctx);
            ctx.add_bytes_loader(Arc::new(images::HttpImages::new(backend.clone())));

            let (media_tx, media_rx) = unbounded();
            if let Some(h) = hwnd(cc) {
                appicon::set_hwnd(h);
                fullscreen::set_hwnd(h);
                ui::set_titlebar_hwnd(h);
                // eframe sets only the small icon; the taskbar button wants the big one.
                appicon::set_window_icon(appicon::get(appicon::load(backend.data_dir())));
            }
            let media = media::Media::new(hwnd(cc), media_tx, wake);

            // Which GL driver this is, for reports of drawing problems (Windows on ARM runs
            // GL through Mesa's D3D12 layer).
            if let Some(gl) = &cc.gl {
                use eframe::glow::HasContext;
                // SAFETY: plain string queries on the current context.
                let (renderer, version) = unsafe { (gl.get_parameter_string(eframe::glow::RENDERER), gl.get_parameter_string(eframe::glow::VERSION)) };
                log::info!("gl: {renderer} ({version})");
            }
            if let Some(rs) = &cc.wgpu_render_state {
                let info = rs.adapter.get_info();
                log::info!("wgpu: {} ({:?}, driver {} {})", info.name, info.backend, info.driver, info.driver_info);
            }
            // The animated background needs the GPU context; without it the UI falls back
            // to a flat gradient.
            let shader = std::env::var_os("YTM_NO_SHADER").is_none();
            let bg = match (&cc.gl, &cc.wgpu_render_state) {
                (Some(gl), _) if shader => shader::Background::new(gl).map_err(|e| log::warn!("background shader unavailable: {e:#}")).ok(),
                (_, Some(rs)) if shader => Some(shader::Background::new_wgpu(rs)),
                _ => None,
            };

            Ok(Box::new(ui::App::new(
                backend,
                audio,
                ui::Channels { resp_rx, audio_rx, media_rx },
                media,
                bg,
                cc.storage,
            )))
        }),
    );
    // A wgpu window that won't open would fail the same way at every launch: go back
    // to OpenGL for the next one.
    if run.is_err() && renderer == renderer::Renderer::Wgpu {
        log::error!("renderer: {} failed to start; switching back to OpenGL", renderer::api_name());
        let _ = renderer::save(&data_dir(), renderer::Renderer::OpenGl);
    }
    run
}

#[cfg(windows)]
fn hwnd(cc: &eframe::CreationContext) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut std::ffi::c_void),
        _ => None,
    }
}

#[cfg(not(windows))]
fn hwnd(_: &eframe::CreationContext) -> Option<*mut std::ffi::c_void> {
    None
}

/// Log to stderr in debug builds; release builds have no console, so log to a file
/// next to the settings (overwritten each launch).
fn init_logging() {
    let mut builder = env_logger::Builder::from_env(
        // A test build can be made more talkative: YTM_DEFAULT_LOG=warn,bitchord=debug cargo build.
        env_logger::Env::default().default_filter_or(option_env!("YTM_DEFAULT_LOG").unwrap_or("warn,bitchord=info")),
    );
    // Dev hook: YTM_LOG=<path> logs there (test runs beside a running copy).
    if let Some(path) = std::env::var_os("YTM_LOG") {
        if let Ok(file) = std::fs::File::create(path) {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
    } else if !cfg!(debug_assertions) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        // The last run's log too: reopening the app after a problem would otherwise wipe it.
        let _ = std::fs::rename(dir.join("ytm-rs.log"), dir.join("ytm-rs.prev.log"));
        if let Ok(file) = std::fs::File::create(dir.join("ytm-rs.log")) {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
    }
    builder.init();
}

/// Seed the hi-res addon list with MadFLAC on first run: when the listener has no
/// hi-res sources configured, the list starts with the MadFLAC manifest, enabled.
/// Deleting that line removes it for good: `madflac_seeded` records that the choice
/// was made, so later starts never re-add it — including when the list is empty
/// because the listener cleared it.
///
/// The seed goes into the settings file (`app.ron`) directly, because eframe hands
/// this process only a shared reference to its storage, and `set_string` needs a
/// mutable one. The file is a pretty-printed RON map of strings; the edit is
/// line-based and aborts on any shape it doesn't recognise, leaving the file alone
/// rather than risking the listener's settings. The pre-seed file is kept once as
/// `app.ron.bak-madflac-seed`.
fn seed_madflac(data_dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(data_dir);
    let path = data_dir.join("app.ron");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let next = if text.trim().is_empty() {
        Some(format!("{{\n    \"hires_source\": \"{}\",\n    \"madflac_seeded\": \"1\",\n}}\n", crate::hires::MADFLAC_MANIFEST_URL))
    } else {
        seed_madflac_in_ron(&text)
    };
    if let Some(next) = next {
        if !text.trim().is_empty() {
            let backup = data_dir.join("app.ron.bak-madflac-seed");
            if !backup.exists() {
                let _ = std::fs::copy(&path, &backup);
            }
        }
        if let Err(e) = std::fs::write(&path, next) {
            log::warn!("couldn't seed the MadFLAC hi-res source: {e:#}");
        }
    }
}

/// Seed MadFLAC into an eframe settings file's text, returning the new text. `None`
/// when the file already records the decision (`madflac_seeded`), or when it isn't
/// the pretty-printed map shape expected.
fn seed_madflac_in_ron(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    // Pretty RON puts the opening brace on its own line; anything else is not a
    // file this editor understands.
    if lines.first().is_none_or(|l| l.trim() != "{") {
        return None;
    }
    if lines.iter().any(|l| l.trim_start().starts_with("\"madflac_seeded\":")) {
        return None;
    }
    let url = crate::hires::MADFLAC_MANIFEST_URL;
    let mut found = false;
    for line in lines.iter_mut() {
        if line.trim_start().starts_with("\"hires_source\":") {
            found = true;
            match ron_string_value(line) {
                Some((start, end, value)) if value.trim().is_empty() => {
                    // The key is there but empty: fill it in.
                    let mut replaced = String::with_capacity(line.len() + url.len());
                    replaced.push_str(&line[..start]);
                    replaced.push('"');
                    replaced.push_str(url);
                    replaced.push('"');
                    replaced.push_str(&line[end..]);
                    *line = replaced;
                }
                Some(_) => {} // The listener already has sources: keep them.
                None => return None,
            }
            break;
        }
    }
    // The flag goes right after the opening brace, so it is set even when the
    // listener already has sources: a list emptied later must not re-seed.
    lines.insert(1, "    \"madflac_seeded\": \"1\",".to_owned());
    if !found {
        lines.insert(1, format!("    \"hires_source\": \"{url}\","));
    }
    Some(lines.join("\n") + "\n")
}

/// The quoted string value of a pretty-RON `"key": "value",` line: the byte span of
/// the value's quotes and the value unescaped. `None` when the line isn't one.
fn ron_string_value(line: &str) -> Option<(usize, usize, String)> {
    let colon = line.find(':')?;
    let rest = &line[colon + 1..];
    let rest = rest.trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let start = line.len() - rest.len();
    let mut value = String::new();
    let mut chars = line[start + 1..].char_indices();
    while let Some((off, ch)) = chars.next() {
        match ch {
            '\\' => {
                let (_, next) = chars.next()?;
                value.push(match next {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '0' => '\0',
                    other => other,
                });
            }
            '"' => return Some((start, start + 1 + off + 1, value)),
            _ => value.push(ch),
        }
    }
    None
}

/// Settings, caches and the sign-in: `data` beside the portable build's exe, else the
/// user's app data. Dev hook: YTM_DATA_DIR runs on a separate profile.
fn data_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("YTM_DATA_DIR") {
        return d.into();
    }
    if let Some(dir) = update::portable_dir() {
        return dir.join("data");
    }
    directories::ProjectDirs::from("", "", "ytm-rs").expect("no home directory").data_dir().to_path_buf()
}


#[cfg(test)]
mod seed_tests {
    use super::seed_madflac_in_ron;
    use crate::hires::MADFLAC_MANIFEST_URL;

    #[test]
    fn an_empty_map_gets_the_seed_and_the_flag() {
        let out = seed_madflac_in_ron("{\n}\n").expect("seeded");
        assert!(out.contains(&format!("\"hires_source\": \"{MADFLAC_MANIFEST_URL}\"")), "{out}");
        assert!(out.contains("\"madflac_seeded\": \"1\""), "{out}");
    }

    #[test]
    fn other_settings_survive_and_existing_sources_are_kept() {
        let text = "{\n    \"volume\": \"0.8\",\n    \"hires_source\": \"https://other.example/addon\",\n}\n";
        let out = seed_madflac_in_ron(text).expect("seeded");
        assert!(out.contains("\"volume\": \"0.8\""), "{out}");
        assert!(out.contains("\"hires_source\": \"https://other.example/addon\""), "{out}");
        assert!(!out.contains(MADFLAC_MANIFEST_URL), "{out}");
        assert!(out.contains("\"madflac_seeded\": \"1\""), "{out}");
    }

    #[test]
    fn an_empty_source_is_filled_in() {
        let text = "{\n    \"hires_source\": \"\",\n}\n";
        let out = seed_madflac_in_ron(text).expect("seeded");
        assert!(out.contains(&format!("\"hires_source\": \"{MADFLAC_MANIFEST_URL}\"")), "{out}");
        assert!(out.contains("\"madflac_seeded\": \"1\""), "{out}");
    }

    #[test]
    fn a_seeded_file_is_left_alone() {
        let text = format!("{{\n    \"hires_source\": \"{MADFLAC_MANIFEST_URL}\",\n    \"madflac_seeded\": \"1\",\n}}\n");
        assert!(seed_madflac_in_ron(&text).is_none());
        let cleared = "{\n    \"hires_source\": \"\",\n    \"madflac_seeded\": \"1\",\n}\n";
        assert!(seed_madflac_in_ron(cleared).is_none(), "a cleared list is not re-seeded");
    }

    #[test]
    fn an_unexpected_shape_is_left_alone() {
        assert!(seed_madflac_in_ron("").is_none());
        assert!(seed_madflac_in_ron("not ron at all").is_none());
        assert!(seed_madflac_in_ron("{\"hires_source\": \"\"}").is_none(), "compact RON is not edited");
    }
}
