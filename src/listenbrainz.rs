//! ListenBrainz scrobbling: the same shape as Last.fm (`lastfm.rs`), against
//! ListenBrainz's own API instead.
//!
//! - A scrobble once a song has played half its length or 4 minutes (songs under
//!   30 s don't count): the same rule Last.fm uses, applied in
//!   `App::sync_listenbrainz` (`ui::playback`).
//! - The listener's user token (from listenbrainz.org's profile page) is kept in
//!   the app vault, the same mechanism Last.fm's session key uses; the toggle
//!   itself is a small `listenbrainz_enabled` file in the data dir, like the
//!   backend's `auto_upgrade` flag.
//! - Scrobbles that can't be sent (offline, ListenBrainz down) wait in
//!   `listenbrainz_queue.json` beside Last.fm's queue and go out with the next
//!   one. Failed sends never block playback: everything here runs on the
//!   backend's network tasks.
//!
//! API: `POST https://api.listenbrainz.org/1/submit-listens` with
//! `Authorization: Token <user-token>` and a body of
//! `{listen_type: "single" | "import" | "playing_now",
//!   payload: [{listened_at, track_metadata: {artist_name, track_name,
//!   release_name, additional_info: {duration, ...}}}]}`
//! (`listened_at` is omitted for `playing_now`). Token check:
//! `GET https://api.listenbrainz.org/1/validate-token` with the same header.

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::lastfm::Scrobble;

const ROOT: &str = "https://api.listenbrainz.org/1/";
const VAULT: &str = "listenbrainz";
const ENABLED_FILE: &str = "listenbrainz_enabled";
/// ListenBrainz takes up to 1000 listens per request; keep batches small.
const BATCH: usize = 50;
/// A token that keeps failing (revoked, wrong) mustn't grow the queue forever.
const MAX_QUEUE: usize = 1000;

struct State {
    enabled: bool,
    token: Option<String>,
    data_dir: PathBuf,
    queue: Vec<Scrobble>,
    /// The play generation the "now playing" call went out for, and whether it
    /// was scrobbled: the same per-generation state Last.fm keeps in `App`.
    scrobbled_gen: Option<(u64, bool)>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State { enabled: false, token: None, data_dir: PathBuf::new(), queue: Vec::new(), scrobbled_gen: None }));
/// One send at a time, so a queued scrobble never goes out twice.
static SENDING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);
/// The "Test connection" button's run and its answer, for the Settings row.
static TEST_BUSY: LazyLock<std::sync::atomic::AtomicBool> = LazyLock::new(Default::default);
static TEST_RESULT: LazyLock<Mutex<Option<Result<String, String>>>> = LazyLock::new(Default::default);
/// The last test's answer, kept for the Settings row to show (unlike the one
/// above, the toast does not consume this copy).
static LAST_TEST: LazyLock<Mutex<Option<Result<String, String>>>> = LazyLock::new(Default::default);

/// Load the saved toggle, token and any scrobbles still waiting to be sent.
pub fn init(data_dir: &Path) {
    let enabled = std::fs::read_to_string(data_dir.join(ENABLED_FILE)).is_ok_and(|s| s.trim() == "1");
    let token = crate::vault::get(VAULT, data_dir).and_then(|b| String::from_utf8(b).ok()).filter(|t| !t.trim().is_empty());
    let queue = std::fs::read(data_dir.join("listenbrainz_queue.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *STATE.lock().unwrap() = State { enabled, token, data_dir: data_dir.to_owned(), queue, scrobbled_gen: None };
}

/// A new generation started playing: true once per generation (the caller sends
/// "now playing" then). False for the same generation again.
pub fn note_started(generation: u64) -> bool {
    let mut st = STATE.lock().unwrap();
    if st.scrobbled_gen.is_some_and(|(g, _)| g == generation) {
        return false;
    }
    st.scrobbled_gen = Some((generation, false));
    true
}

/// The scrobble rule is met for this generation: true once (the caller sends the
/// scrobble then). False before the generation started, or after it scrobbled.
pub fn note_scrobbled(generation: u64) -> bool {
    let mut st = STATE.lock().unwrap();
    if st.scrobbled_gen != Some((generation, false)) {
        return false;
    }
    st.scrobbled_gen = Some((generation, true));
    true
}

/// Start a "Test connection" run: false when one is already running.
pub fn test_start() -> bool {
    !TEST_BUSY.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// Finish a "Test connection" run, leaving its answer for the Settings row.
pub fn test_finish(r: Result<String, String>) {
    *LAST_TEST.lock().unwrap() = Some(r.clone());
    *TEST_RESULT.lock().unwrap() = Some(r);
    TEST_BUSY.store(false, std::sync::atomic::Ordering::Relaxed);
}

pub fn test_busy() -> bool {
    TEST_BUSY.load(std::sync::atomic::Ordering::Relaxed)
}

/// Take the last test's answer, if one arrived since the last take.
pub fn take_test_result() -> Option<Result<String, String>> {
    TEST_RESULT.lock().unwrap().take()
}

/// The last test's answer, without taking it, for the Settings row to show.
pub fn peek_test_result() -> Option<Result<String, String>> {
    LAST_TEST.lock().unwrap().clone()
}

pub fn enabled() -> bool {
    STATE.lock().unwrap().enabled
}

pub fn set_enabled(on: bool) {
    let mut st = STATE.lock().unwrap();
    st.enabled = on;
    let _ = std::fs::write(st.data_dir.join(ENABLED_FILE), if on { "1" } else { "0" });
}

/// The saved token, if one was ever typed in.
pub fn token() -> Option<String> {
    STATE.lock().unwrap().token.clone()
}

/// Save a (possibly empty, which clears it) user token to the vault.
pub fn set_token(token: &str) {
    let token = token.trim();
    let mut st = STATE.lock().unwrap();
    if token.is_empty() {
        st.token = None;
        crate::vault::delete(VAULT, &st.data_dir);
    } else if let Err(e) = crate::vault::set(VAULT, &st.data_dir, token.as_bytes()) {
        log::warn!("listenbrainz: couldn't save the token: {e:#}");
    } else {
        st.token = Some(token.to_owned());
    }
}

/// Check the token against ListenBrainz. Ok(the account name) when it is valid.
pub async fn validate_token(http: &reqwest::Client, token: &str) -> anyhow::Result<String> {
    let v: Value = http
        .get(format!("{ROOT}validate-token"))
        .header("Authorization", format!("Token {token}"))
        .timeout(Duration::from_secs(20))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if v["valid"].as_bool().unwrap_or(false) {
        Ok(v["user_name"].as_str().unwrap_or_default().to_owned())
    } else {
        anyhow::bail!("{}", v["message"].as_str().unwrap_or("That token isn't valid"))
    }
}

/// Tell ListenBrainz what's playing now (no `listened_at`: it is playing, not played).
pub async fn now_playing(http: &reqwest::Client, s: &Scrobble) {
    let Some(token) = token() else { return };
    let body = json!({"listen_type": "playing_now", "payload": [listen_payload(s, false)]});
    match submit(http, &token, &body).await {
        Ok(_) => log::debug!("listenbrainz: now playing {} - {}", s.artist, s.track),
        Err(e) => log::warn!("listenbrainz: now playing failed: {e:#}"),
    }
}

/// Scrobble `s`, along with any earlier ones still waiting.
pub async fn scrobble(http: &reqwest::Client, s: Scrobble) {
    let _sending = SENDING.lock().await;
    let Some(token) = token() else { return };
    {
        let mut st = STATE.lock().unwrap();
        st.queue.push(s);
        if st.queue.len() > MAX_QUEUE {
            let drop = st.queue.len() - MAX_QUEUE;
            st.queue.drain(..drop);
            log::warn!("listenbrainz: queue capped, dropped {drop} oldest scrobbles");
        }
        save_queue(&st);
    }
    loop {
        let batch: Vec<Scrobble> = STATE.lock().unwrap().queue.iter().take(BATCH).cloned().collect();
        if batch.is_empty() {
            break;
        }
        let body = submit_body(&batch);
        match submit(http, &token, &body).await {
            Ok(_) => {
                log::info!("listenbrainz: scrobbled {}", batch.len());
                let mut st = STATE.lock().unwrap();
                st.queue.drain(..batch.len());
                save_queue(&st);
            }
            Err(e) => {
                log::warn!("listenbrainz: scrobble failed, keeping {} for later: {e:#}", batch.len());
                save_queue(&STATE.lock().unwrap());
                break;
            }
        }
    }
}

fn save_queue(st: &State) {
    let path = st.data_dir.join("listenbrainz_queue.json");
    if st.queue.is_empty() {
        let _ = std::fs::remove_file(path);
    } else if let Ok(json) = serde_json::to_vec(&st.queue) {
        let _ = std::fs::write(path, json);
    }
}

/// One listen's JSON. `with_time` is false for `playing_now` (ListenBrainz rejects
/// a `listened_at` there).
fn listen_payload(s: &Scrobble, with_time: bool) -> Value {
    let mut meta = json!({"artist_name": s.artist, "track_name": s.track});
    if let Some(album) = &s.album {
        meta["release_name"] = json!(album);
    }
    if let Some(d) = s.duration {
        meta["additional_info"] = json!({"duration": d});
    }
    let mut listen = json!({"track_metadata": meta});
    if with_time {
        listen["listened_at"] = json!(s.timestamp);
    }
    listen
}

/// The submit body for a batch: one listen goes as `single`, several as `import`.
fn submit_body(batch: &[Scrobble]) -> Value {
    let payload: Vec<Value> = batch.iter().map(|s| listen_payload(s, true)).collect();
    json!({"listen_type": if batch.len() == 1 { "single" } else { "import" }, "payload": payload})
}

async fn submit(http: &reqwest::Client, token: &str, body: &Value) -> anyhow::Result<()> {
    let res = http.post(format!("{ROOT}submit-listens")).header("Authorization", format!("Token {token}")).json(body).timeout(Duration::from_secs(20)).send().await?;
    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        anyhow::bail!("ListenBrainz answered {status}: {}", text.chars().take(200).collect::<String>());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrobble(artist: &str, track: &str) -> Scrobble {
        Scrobble { artist: artist.to_owned(), track: track.to_owned(), album: Some("Album".into()), duration: Some(200), timestamp: 1_700_000_000 }
    }

    #[test]
    fn one_listen_goes_as_single_with_its_time() {
        let body = submit_body(&[scrobble("Artist", "Song")]);
        assert_eq!(body["listen_type"], "single");
        let listen = &body["payload"][0];
        assert_eq!(listen["listened_at"], 1_700_000_000);
        assert_eq!(listen["track_metadata"]["artist_name"], "Artist");
        assert_eq!(listen["track_metadata"]["track_name"], "Song");
        assert_eq!(listen["track_metadata"]["release_name"], "Album");
        assert_eq!(listen["track_metadata"]["additional_info"]["duration"], 200);
    }

    #[test]
    fn several_listens_go_as_import() {
        let body = submit_body(&[scrobble("A", "One"), scrobble("B", "Two")]);
        assert_eq!(body["listen_type"], "import");
        assert_eq!(body["payload"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn playing_now_has_no_listened_at() {
        let listen = listen_payload(&scrobble("Artist", "Song"), false);
        assert!(listen.get("listened_at").is_none());
        assert_eq!(listen["track_metadata"]["track_name"], "Song");
    }

    #[test]
    fn missing_album_and_duration_are_omitted_not_null() {
        let s = Scrobble { artist: "A".into(), track: "T".into(), album: None, duration: None, timestamp: 42 };
        let listen = listen_payload(&s, true);
        assert!(listen["track_metadata"].get("release_name").is_none());
        assert!(listen["track_metadata"].get("additional_info").is_none());
        assert_eq!(listen["listened_at"], 42);
    }

    #[test]
    fn queue_survives_a_json_round_trip() {
        let q = vec![scrobble("A", "One"), scrobble("B", "Two")];
        let json = serde_json::to_vec(&q).unwrap();
        let back: Vec<Scrobble> = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, q);
    }
}
