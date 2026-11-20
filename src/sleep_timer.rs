//! Sleep timer: pause playback after a while, or when the current track ends.
//!
//! The state lives in a process-wide lock, so both the UI (which shows the
//! countdown and takes the listener's pick) and the audio thread (which does the
//! pausing while the song plays) can see it without either owning it. Nothing here
//! touches egui: the UI checks the timer on its tick (`App::tick_sleep_timer` in
//! `ui::playback`), and the audio thread checks it in its own loop as a backstop
//! (the UI repaints regularly while music plays, but the audio loop is the one
//! that is always awake while a song is on).
//!
//! Two shapes:
//! - Minutes: pause when the deadline passes. One-shot: firing clears it.
//! - AfterTrack: pause when the current track ends naturally. The audio thread
//!   suppresses the crossfade `Ending` event while one is armed (else the next
//!   song would start before the track ends), and sets `stop_pending` as the track
//!   finishes; the UI consumes that flag instead of advancing the queue
//!   (`App::next_after_end`, and the guard at the top of `play_current_at` for
//!   the repeat-one path).

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// The preset minutes offered in Settings.
pub const PRESETS_MIN: [u32; 5] = [15, 30, 45, 60, 90];

/// What the timer is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// Pause at `ends_at`.
    Minutes { ends_at: Instant },
    /// Pause when the current track ends naturally.
    AfterTrack,
}

/// Pure state: what is armed and when it fires. Kept apart from the global so the
/// maths is unit-testable without the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepTimer {
    mode: Mode,
    /// The last preset the listener picked, for the Settings row to offer again.
    last_preset: u32,
}

impl Default for SleepTimer {
    fn default() -> Self {
        Self { mode: Mode::Off, last_preset: 30 }
    }
}

impl SleepTimer {
    /// Arm a minutes timer from `now`.
    pub fn set_minutes(&mut self, minutes: u32, now: Instant) {
        self.mode = Mode::Minutes { ends_at: now + Duration::from_secs(minutes as u64 * 60) };
        self.last_preset = minutes;
    }

    pub fn set_after_track(&mut self) {
        self.mode = Mode::AfterTrack;
    }

    pub fn cancel(&mut self) {
        self.mode = Mode::Off;
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn after_track_armed(&self) -> bool {
        self.mode == Mode::AfterTrack
    }

    pub fn last_preset(&self) -> u32 {
        self.last_preset
    }

    /// What is left on a minutes timer, if one is armed.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match self.mode {
            Mode::Minutes { ends_at } => Some(ends_at.saturating_duration_since(now)),
            _ => None,
        }
    }

    /// The minutes timer has run out. Consumes it: firing is one-shot.
    pub fn take_due(&mut self, now: Instant) -> bool {
        match self.mode {
            Mode::Minutes { ends_at } if now >= ends_at => {
                self.mode = Mode::Off;
                true
            }
            _ => false,
        }
    }

    /// "12:34 left" / "less than a minute left", for the Settings row.
    pub fn remaining_label(&self, now: Instant) -> Option<String> {
        let left = self.remaining(now)?;
        let secs = left.as_secs();
        Some(if secs < 60 {
            "less than a minute left".to_owned()
        } else {
            format!("{}:{:02} left", secs / 60, secs % 60)
        })
    }
}

struct State {
    timer: SleepTimer,
    data_dir: PathBuf,
    /// Set by the audio thread as the armed track finishes; the UI consumes it
    /// instead of advancing the queue.
    stop_pending: bool,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State { timer: SleepTimer::default(), data_dir: PathBuf::new(), stop_pending: false })
});

/// The last chosen preset, kept in `sleep_timer_preset` in the data dir (a plain
/// file like the backend's `auto_upgrade` flag, so no eframe storage plumbing is
/// needed for one number).
const PRESET_FILE: &str = "sleep_timer_preset";

/// Load the last chosen preset. Called once from `App::new`, next to `lastfm::init`.
pub fn init(data_dir: &Path) {
    let preset = std::fs::read_to_string(data_dir.join(PRESET_FILE))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|m| PRESETS_MIN.contains(m))
        .unwrap_or(30);
    let mut st = STATE.lock().unwrap();
    st.data_dir = data_dir.to_owned();
    st.timer.last_preset = preset;
}

fn save_preset(st: &State) {
    let _ = std::fs::write(st.data_dir.join(PRESET_FILE), st.timer.last_preset.to_string());
}

/// Arm a minutes timer from now, remembering the preset.
pub fn set_minutes(minutes: u32) {
    let mut st = STATE.lock().unwrap();
    st.timer.set_minutes(minutes, Instant::now());
    st.stop_pending = false;
    save_preset(&st);
}

/// Arm "stop after this track".
pub fn set_after_track() {
    let mut st = STATE.lock().unwrap();
    st.timer.set_after_track();
    st.stop_pending = false;
}

/// Cancel whatever is armed. (A timer that fired already cleared itself.)
pub fn cancel() {
    STATE.lock().unwrap().timer.cancel();
}

/// What is armed right now.
pub fn mode() -> Mode {
    STATE.lock().unwrap().timer.mode()
}

pub fn after_track_armed() -> bool {
    STATE.lock().unwrap().timer.after_track_armed()
}

pub fn last_preset() -> u32 {
    STATE.lock().unwrap().timer.last_preset()
}

pub fn remaining_label() -> Option<String> {
    STATE.lock().unwrap().timer.remaining_label(Instant::now())
}

/// The minutes timer has run out. Consumes it: firing is one-shot.
pub fn take_due() -> bool {
    STATE.lock().unwrap().timer.take_due(Instant::now())
}

/// The audio thread calls this as the armed track finishes naturally; the UI
/// consumes the flag instead of advancing the queue.
pub fn arm_stop_pending() {
    let mut st = STATE.lock().unwrap();
    if st.timer.after_track_armed() {
        st.timer.cancel();
        st.stop_pending = true;
    }
}

/// Take the stop-after-track flag, if the track just ended with one armed.
pub fn take_stop_pending() -> bool {
    std::mem::replace(&mut STATE.lock().unwrap().stop_pending, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> Instant {
        Instant::now() + Duration::from_secs(secs)
    }

    #[test]
    fn minutes_timer_counts_down_and_fires_once() {
        let mut t = SleepTimer::default();
        let start = Instant::now();
        t.set_minutes(30, start);
        assert_eq!(t.remaining(start), Some(Duration::from_secs(30 * 60)));
        assert_eq!(t.remaining(start + Duration::from_secs(60)), Some(Duration::from_secs(29 * 60)));
        assert!(!t.take_due(start + Duration::from_secs(29 * 60)));
        assert!(t.take_due(start + Duration::from_secs(30 * 60)));
        assert_eq!(t.mode(), Mode::Off);
        assert!(!t.take_due(start + Duration::from_secs(31 * 60)), "firing is one-shot");
        assert_eq!(t.remaining(start), None);
    }

    #[test]
    fn after_track_has_no_countdown_and_survives_time_passing() {
        let mut t = SleepTimer::default();
        t.set_after_track();
        assert!(t.after_track_armed());
        assert_eq!(t.remaining(at(3600)), None);
        assert!(!t.take_due(at(3600)));
        assert!(t.after_track_armed());
    }

    #[test]
    fn cancel_disarms_and_the_last_preset_is_remembered() {
        let mut t = SleepTimer::default();
        t.set_minutes(90, Instant::now());
        assert_eq!(t.last_preset(), 90);
        t.cancel();
        assert_eq!(t.mode(), Mode::Off);
        // The preset survives cancelling: the row offers it again.
        assert_eq!(t.last_preset(), 90);
        t.set_minutes(15, Instant::now());
        assert_eq!(t.last_preset(), 15);
    }

    #[test]
    fn remaining_label_reads_like_a_clock() {
        let mut t = SleepTimer::default();
        let start = Instant::now();
        t.set_minutes(45, start);
        assert_eq!(t.remaining_label(start).as_deref(), Some("45:00 left"));
        assert_eq!(t.remaining_label(start + Duration::from_secs(61)).as_deref(), Some("43:59 left"));
        t.set_minutes(1, start);
        assert_eq!(t.remaining_label(start + Duration::from_secs(30)).as_deref(), Some("less than a minute left"));
        t.cancel();
        assert_eq!(t.remaining_label(start), None);
    }
}
