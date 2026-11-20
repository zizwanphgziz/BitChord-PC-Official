//! Local music library (v1): folders the listener picks on the Library page,
//! scanned for audio files, with title/artist/album/duration read from the
//! files themselves.
//!
//! Nothing here touches the YTM library: these tracks live on disk and play
//! from their files, through the same `Shared` buffer a download plays from
//! (`downloads::open`) — see the `open` hook in `backend.rs`.
//!
//! The folder list and the scanned index are cached as JSON in the data dir
//! (`local_library.json`), so a rescan only re-reads files whose size or
//! modification time changed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::model::{Link, Track};
use crate::stream::{Format, Shared};

/// Audio files the library picks up.
pub const AUDIO_EXTS: &[&str] = &["flac", "mp3", "m4a", "aac", "ogg", "oga", "opus", "wav"];

/// A track in the local library: its file, its tags, and its id (see `id_for`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalTrack {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Seconds; `None` when nothing could say how long the file is.
    pub duration_secs: Option<u32>,
}

impl LocalTrack {
    /// The track as the rest of the app sees it: playable with `Action::Play`
    /// like any other song, with no art (the rows show a music note instead).
    pub fn to_track(&self) -> Track {
        Track {
            id: self.id.clone(),
            title: self.title.clone(),
            artists: if self.artist.is_empty() { vec![] } else { vec![Link { name: self.artist.clone(), id: None }] },
            album: if self.album.is_empty() { None } else { Some(Link { name: self.album.clone(), id: None }) },
            duration: self.duration_secs,
            thumbs: vec![],
            track_nr: None,
            plays: None,
            music_video: false,
        }
    }
}

/// A stable id for a file: `local:` + FNV-1a of its path, so the same file
/// keeps its id across restarts (and the cache can find it again).
pub fn id_for(path: &Path) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.as_os_str().to_string_lossy().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("local:{h:016x}")
}

/// The extension of an audio file, lowercased (`None` for anything else).
pub fn audio_ext(path: &Path) -> Option<String> {
    let e = path.extension()?.to_str()?.to_ascii_lowercase();
    AUDIO_EXTS.contains(&e.as_str()).then_some(e)
}

/// How a local file decodes: rodio's symphonia path, the same one
/// `stream::decoder` uses for `Format::File`.
pub fn format_for(ext: &str) -> Format {
    let mime = |m: &str| Some(m.to_owned());
    match ext {
        "flac" => Format::File { hint: "flac", mime: mime("audio/flac") },
        "mp3" => Format::File { hint: "mp3", mime: mime("audio/mpeg") },
        "m4a" => Format::File { hint: "m4a", mime: mime("audio/mp4") },
        "aac" => Format::File { hint: "aac", mime: mime("audio/aac") },
        "ogg" | "oga" | "opus" => Format::File { hint: "ogg", mime: mime("audio/ogg") },
        _ => Format::File { hint: "wav", mime: mime("audio/wav") },
    }
}

// ------------------------------------------------------------------- state

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    folders: Vec<PathBuf>,
    tracks: Vec<LocalTrack>,
}

struct State {
    folders: Vec<PathBuf>,
    tracks: Vec<LocalTrack>,
    file: PathBuf,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| {
    RwLock::new(State { folders: Vec::new(), tracks: Vec::new(), file: PathBuf::new() })
});

/// Load the cached folder list and index; `data_dir` is the app's data dir.
pub fn init(data_dir: &Path) {
    let file = data_dir.join("local_library.json");
    let cache: Cache = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut st = STATE.write().unwrap();
    st.folders = cache.folders.into_iter().filter(|f| f.is_dir()).collect();
    st.tracks = cache.tracks.into_iter().filter(|t| t.path.is_file()).collect();
    st.file = file;
}

fn save_locked(st: &State) {
    if st.file.as_os_str().is_empty() {
        return;
    }
    let cache = Cache { folders: st.folders.clone(), tracks: st.tracks.clone() };
    if let Ok(b) = serde_json::to_vec_pretty(&cache) {
        let _ = std::fs::write(&st.file, b);
    }
}

/// The folders being watched.
pub fn folders() -> Vec<PathBuf> {
    STATE.read().unwrap().folders.clone()
}

/// The scanned tracks, as app tracks, sorted by artist, album, title.
pub fn tracks() -> Vec<Track> {
    local_tracks().iter().map(LocalTrack::to_track).collect()
}

pub fn local_tracks() -> Vec<LocalTrack> {
    STATE.read().unwrap().tracks.clone()
}

/// Watch a folder (and scan it). False when it was already watched or isn't a dir.
pub fn add_folder(path: PathBuf) -> bool {
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    {
        let mut st = STATE.write().unwrap();
        if !path.is_dir() || st.folders.contains(&path) {
            return false;
        }
        st.folders.push(path);
        save_locked(&st);
    }
    rescan();
    true
}

/// Stop watching a folder, and drop its tracks. False when it wasn't watched.
pub fn remove_folder(path: &Path) -> bool {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut st = STATE.write().unwrap();
    let before = st.folders.len();
    st.folders.retain(|f| *f != path);
    if st.folders.len() == before {
        return false;
    }
    st.tracks.retain(|t| !t.path.starts_with(&path));
    save_locked(&st);
    true
}

/// Re-scan every watched folder. Tag reads are header-only, so a rescan
/// re-reads them each time; the cache still saves the folder list and the
/// last index, so the library shows at once on launch.
pub fn rescan() {
    let folders = STATE.read().unwrap().folders.clone();
    let mut fresh = Vec::new();
    for f in &folders {
        for path in walk_audio(f) {
            fresh.push(read_file(&path));
        }
    }
    fresh.sort_by_key(|t| (t.artist.to_lowercase(), t.album.to_lowercase(), t.title.to_lowercase()));
    let mut st = STATE.write().unwrap();
    st.tracks = fresh;
    save_locked(&st);
}

/// Every audio file under `root`, deepest first, sorted by path. Symlinked
/// directories are followed once each (a loop of links can't spin forever).
pub fn walk_audio(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 16 {
            continue;
        }
        let canon = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !seen.insert(canon) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let ft = match e.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() || (ft.is_symlink() && p.is_dir()) {
                stack.push((p, depth + 1));
            } else if ft.is_file() && audio_ext(&p).is_some() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Read one file's tags (and duration), falling back to its filename.
pub fn read_file(path: &Path) -> LocalTrack {
    let meta = read_metadata(path);
    let (title, artist) = match (meta.title, meta.artist) {
        (Some(t), Some(a)) => (t, a),
        (Some(t), None) => (t, String::new()),
        (None, Some(a)) => (filename_title(path), a),
        (None, None) => filename_split(path),
    };
    LocalTrack { id: id_for(path), path: path.to_path_buf(), title, artist, album: meta.album.unwrap_or_default(), duration_secs: meta.duration_secs }
}

// ------------------------------------------------------------------- playback

/// A local track's audio, as a complete stream buffer for playback — the
/// mirror of `downloads::open`, for the `backend.rs` stream hook.
pub fn open(id: &str) -> Option<Arc<Shared>> {
    let t = STATE.read().unwrap().tracks.iter().find(|t| t.id == id)?.clone();
    let data = std::fs::read(&t.path).ok()?;
    let ext = audio_ext(&t.path).unwrap_or_else(|| "wav".to_owned());
    log::info!("{id}: playing the local file {}", t.path.display());
    Some(Shared::from_bytes_with_format(data, t.duration_secs.unwrap_or(0) as u64 * 1000, None, format_for(&ext), None))
}

/// Ask for a folder to watch (the system picker, on its own thread).
pub fn pick_folder(ctx: &egui::Context) {
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        if let Some(dir) = rfd::FileDialog::new().set_title("Choose a music folder").pick_folder() {
            add_folder(dir);
            ctx.request_repaint();
        }
    });
}

// ------------------------------------------------------------------- metadata

/// What could be read from a file's tags.
#[derive(Default)]
pub struct Meta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
}

fn clean(s: String) -> Option<String> {
    // Fixed-width tag fields (ID3v1) pad with NULs; strip those too.
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_owned();
    (!s.is_empty()).then_some(s)
}

/// Tags and duration for one file, best effort: symphonia's probe first
/// (FLAC, M4A), then mp4ameta (M4A), then a minimal ID3 reader (MP3), then
/// rodio's decoder for the duration alone (WAV, OGG and the rest).
pub fn read_metadata(path: &Path) -> Meta {
    let mut meta = Meta::default();
    let ext = audio_ext(path).unwrap_or_default();
    if let Some(m) = symphonia_tags(path, &ext) {
        merge(&mut meta, m);
    }
    if meta.title.is_none() && ext == "m4a" && let Ok(tag) = mp4ameta::Tag::read_from_path(path) {
        if let Some(t) = clean(tag.title().unwrap_or_default().to_owned()) {
            meta.title = Some(t);
        }
        if let Some(a) = clean(tag.artist().unwrap_or_default().to_owned()) {
            meta.artist = Some(a);
        }
        if let Some(a) = clean(tag.album().unwrap_or_default().to_owned()) {
            meta.album = Some(a);
        }
        let secs = tag.duration().as_secs();
        if secs > 0 {
            meta.duration_secs = Some(secs as u32);
        }
    }
    if meta.title.is_none() && ext == "mp3" && let Some(m) = id3_tags(path) {
        merge(&mut meta, m);
    }
    if meta.duration_secs.is_none() {
        meta.duration_secs = rodio_duration(path);
    }
    meta
}

fn merge(into: &mut Meta, from: Meta) {
    if into.title.is_none() {
        into.title = from.title;
    }
    if into.artist.is_none() {
        into.artist = from.artist;
    }
    if into.album.is_none() {
        into.album = from.album;
    }
    if into.duration_secs.is_none() {
        into.duration_secs = from.duration_secs;
    }
}

/// symphonia's probe: the tags in the container, and the track length from
/// its frame count. (The probe only knows the formats this build enables —
/// FLAC and MP4 — so other files fall through to the readers below.)
fn symphonia_tags(path: &Path, ext: &str) -> Option<Meta> {
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTagKey};
    use symphonia::core::probe::Hint;
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(ext);
    let probed = symphonia::default::get_probe().format(&hint, mss, &Default::default(), &MetadataOptions::default()).ok()?;
    let mut format = probed.format;
    let mut meta = Meta::default();
    if let Some(rev) = format.metadata().current() {
        for tag in rev.tags() {
            let v = clean(tag.value.to_string());
            match tag.std_key {
                Some(StandardTagKey::TrackTitle) => meta.title = v,
                Some(StandardTagKey::Artist) => meta.artist = v,
                Some(StandardTagKey::Album) => meta.album = v,
                _ => {}
            }
        }
    }
    if let Some(track) = format.default_track() {
        let p = &track.codec_params;
        if let (Some(tb), Some(n)) = (p.time_base, p.n_frames) {
            let secs = n as f64 * tb.numer as f64 / tb.denom as f64;
            if secs > 0.0 {
                meta.duration_secs = Some(secs.round() as u32);
            }
        }
    }
    Some(meta)
}

/// How long rodio says a file is, from its headers alone (nothing decoded).
fn rodio_duration(path: &Path) -> Option<u32> {
    use rodio::Source;
    let file = std::fs::File::open(path).ok()?;
    let decoder = rodio::Decoder::new(file).ok()?;
    let secs = decoder.total_duration()?.as_secs();
    (secs > 0).then_some(secs as u32)
}

// ------------------------------------------------------------------- ID3 (MP3)

/// A minimal ID3v2 reader (TIT2/TPE1/TALB) plus an ID3v1 fallback: enough
/// for the tags ffmpeg and the usual taggers write, without a new dependency.
fn id3_tags(path: &Path) -> Option<Meta> {
    let bytes = std::fs::read(path).ok()?;
    let mut meta = id3v2(&bytes).unwrap_or_default();
    if meta.title.is_none() {
        merge(&mut meta, id3v1(&bytes));
    }
    (!meta.title.is_none() || !meta.artist.is_none() || !meta.album.is_none()).then_some(meta)
}

fn id3v2(b: &[u8]) -> Option<Meta> {
    if b.len() < 10 || &b[..3] != b"ID3" {
        return None;
    }
    // Synchsafe size: the tag's length after its 10-byte header.
    let size = b[6..10].iter().fold(0usize, |a, x| (a << 7) | (*x as usize & 0x7f));
    let end = (10 + size).min(b.len());
    let mut meta = Meta::default();
    let mut i = 10;
    while i + 10 <= end {
        let id = &b[i..i + 4];
        if id == [0; 4] {
            break;
        }
        let len = if b[3] == 4 {
            // v2.4: synchsafe frame sizes.
            b[i + 4..i + 8].iter().fold(0usize, |a, x| (a << 7) | (*x as usize & 0x7f))
        } else {
            u32::from_be_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize
        };
        let start = i + 10;
        if start + len > end || len == 0 {
            break;
        }
        let value = decode_text(&b[start..start + len]);
        match id {
            b"TIT2" => meta.title = clean(value),
            b"TPE1" => meta.artist = clean(value),
            b"TALB" => meta.album = clean(value),
            _ => {}
        }
        i = start + len;
    }
    Some(meta)
}

/// ID3v2 text: encoding byte 0 = Latin-1, 1 = UTF-16 with BOM, 2 = UTF-16BE,
/// 3 = UTF-8.
fn decode_text(b: &[u8]) -> String {
    if b.is_empty() {
        return String::new();
    }
    match b[0] {
        0 => b[1..].iter().map(|c| *c as char).collect::<String>().trim_matches('\0').to_owned(),
        3 => String::from_utf8_lossy(&b[1..]).trim_matches('\0').to_owned(),
        1 | 2 => {
            let be = b[0] == 2 || b.get(1..3) == Some(&[0xfe, 0xff]);
            let start = if b[0] == 1 && b.len() >= 3 { 3 } else { 1 };
            let units: Vec<u16> = b[start..]
                .as_chunks::<2>().0.iter()
                .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
                .collect();
            String::from_utf16_lossy(&units).trim_matches('\0').to_owned()
        }
        _ => String::from_utf8_lossy(&b[1..]).trim_matches('\0').to_owned(),
    }
}

/// The 128 trailing bytes of an MP3: "TAG" + 30-byte title/artist/album.
fn id3v1(b: &[u8]) -> Meta {
    let mut meta = Meta::default();
    if b.len() < 128 || &b[b.len() - 128..b.len() - 125] != b"TAG" {
        return meta;
    }
    let tail = &b[b.len() - 128..];
    let field = |at: usize| clean(tail[at..at + 30].iter().map(|c| *c as char).collect());
    meta.title = field(3);
    meta.artist = field(33);
    meta.album = field(63);
    meta
}

// ------------------------------------------------------------------- filenames

/// A file's name as a title: its stem, without a leading track number.
pub fn filename_title(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    strip_track_number(stem)
}

fn strip_track_number(stem: &str) -> String {
    let digits: String = stem.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = stem[digits.len()..].trim_start_matches(['.', '-', ' ', '_']);
        if !rest.is_empty() {
            return rest.to_owned();
        }
    }
    stem.to_owned()
}

/// "Artist - Title.ext" → ("Title", "Artist"); anything else → (stem, "").
fn filename_split(path: &Path) -> (String, String) {
    let stem = filename_title(path);
    match stem.split_once(" - ") {
        Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() => (t.trim().to_owned(), a.trim().to_owned()),
        _ => (stem, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bitchord-locallib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A minimal 16-bit mono WAV: 2 s at 8 kHz, so rodio can read its length.
    fn write_wav(path: &Path) {
        let samples: u32 = 8000 * 2;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&8000u32.to_le_bytes());
        v.extend_from_slice(&16000u32.to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(samples * 2).to_le_bytes());
        v.extend(std::iter::repeat_n(0u8, (samples * 2) as usize));
        std::fs::write(path, v).unwrap();
    }

    #[test]
    fn ids_are_stable_for_a_path() {
        let p = Path::new("/music/Artist - Song.flac");
        assert_eq!(id_for(p), id_for(p));
        assert!(id_for(p).starts_with("local:"));
        assert_ne!(id_for(p), id_for(Path::new("/music/other.flac")));
    }

    #[test]
    fn filename_split_reads_artist_dash_title() {
        assert_eq!(filename_split(Path::new("02 - The Artist - The Song.mp3")), ("The Song".into(), "The Artist".into()));
        assert_eq!(filename_split(Path::new("Just A Song.flac")), ("Just A Song".into(), String::new()));
        assert_eq!(filename_title(Path::new("07. Intro.wav")), "Intro");
    }

    #[test]
    fn id3v1_tags_read() {
        let mut tag = vec![0u8; 128];
        tag[..3].copy_from_slice(b"TAG");
        let put = |b: &mut Vec<u8>, at: usize, s: &str| {
            b[at..at + s.len()].copy_from_slice(s.as_bytes());
        };
        put(&mut tag, 3, "V1 Title");
        put(&mut tag, 33, "V1 Artist");
        put(&mut tag, 63, "V1 Album");
        let mut b = vec![0u8; 200];
        b.extend_from_slice(&tag);
        let m = id3v1(&b);
        assert_eq!(m.title.as_deref(), Some("V1 Title"));
        assert_eq!(m.artist.as_deref(), Some("V1 Artist"));
        assert_eq!(m.album.as_deref(), Some("V1 Album"));
    }

    #[test]
    fn scan_finds_audio_ignores_the_rest() {
        let d = tmpdir("scan");
        write_wav(&d.join("A - First Song.wav"));
        std::fs::write(d.join("notes.txt"), b"not music").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        write_wav(&d.join("sub").join("Second Song.wav"));
        let found = walk_audio(&d);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|p| p.extension().unwrap() == "wav"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wav_scans_with_filename_and_duration() {
        let d = tmpdir("wav");
        let p = d.join("The Artist - Two Seconds.wav");
        write_wav(&p);
        let t = read_file(&p);
        assert_eq!(t.title, "Two Seconds");
        assert_eq!(t.artist, "The Artist");
        assert_eq!(t.duration_secs, Some(2), "rodio reads the WAV length");
        assert_eq!(t.to_track().duration, Some(2));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn open_returns_a_shared_buffer_for_a_scanned_file() {
        let d = tmpdir("open");
        let p = d.join("Song.wav");
        write_wav(&p);
        let t = read_file(&p);
        {
            let mut st = STATE.write().unwrap();
            st.tracks = vec![t.clone()];
            st.file = PathBuf::new();
        }
        let shared = open(&t.id).expect("the scanned file opens");
        assert_eq!(shared.total, std::fs::metadata(&p).unwrap().len());
        assert_eq!(shared.duration_ms, 2000);
        {
            STATE.write().unwrap().tracks.clear();
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn folders_round_trip_through_the_cache_file() {
        let d = tmpdir("cache");
        let data = tmpdir("cache-data");
        let file = data.join("local_library.json");
        {
            let mut st = STATE.write().unwrap();
            st.folders = vec![d.clone()];
            st.tracks = vec![];
            st.file = file.clone();
            save_locked(&st);
        }
        // Fresh state, then init from the file on disk.
        {
            let mut st = STATE.write().unwrap();
            st.folders.clear();
            st.file = PathBuf::new();
        }
        init(&data);
        assert_eq!(folders(), vec![d.clone()]);
        {
            let mut st = STATE.write().unwrap();
            st.folders.clear();
            st.tracks.clear();
            st.file = PathBuf::new();
        }
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&data);
    }
}
