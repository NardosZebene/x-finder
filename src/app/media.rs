//! Quick Look plays audio and video (Media Foundation's MFPlay).
//!
//! Video is drawn by Windows into a small native child window placed exactly
//! over the Quick Look area (Slint reports that rectangle); play/pause, the
//! seek bar, time and mute are Slint controls below it. Audio needs no
//! window. Formats are whatever Windows can play (MP4/H.264, MOV, WMV, MP3,
//! AAC, WAV, FLAC…; MKV/HEVC need the Store extensions).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use finder_rs::core::preview::{self, Kind};
use slint::ComponentHandle;

use super::{App, with_app};
use crate::Bridge;

/// Debug trace (FINDER_MEDIA_LOG=file).
pub(super) fn trace(msg: &str) {
    if let Some(p) = std::env::var_os("FINDER_MEDIA_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{:?} {msg}", std::time::SystemTime::now());
        }
    }
}

/// Video loaded but no picture after this long: no decoder for it.
const NO_PICTURE_AFTER: Duration = Duration::from_millis(1500);

/// No duration after this long: Windows can't play the file.
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Media {
    path: PathBuf,
    started: Instant,
    duration: i64,
    /// A video file (vs. audio).
    video: bool,
    /// Video whose picture Windows can't decode (e.g. HEVC without the
    /// Store extension): plays as audio over the thumbnail.
    no_picture: bool,
    #[cfg(windows)]
    inner: imp::Player,
    _timer: slint::Timer,
}

/// Formats Quick Look plays: (is video).
fn playable(path: &Path) -> Option<bool> {
    match preview::classify_path(path) {
        Kind::Video => Some(true),
        Kind::Audio => Some(false),
        _ => None,
    }
}

/// "1:05" / "1:02:03" from 100 ns units.
fn clock(t: i64) -> String {
    let s = (t.max(0) / 10_000_000) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

impl App {
    /// Starts/stops playback to match Quick Look and its current file.
    pub(super) fn update_media(&mut self) {
        let want = if self.quicklook {
            let model = self.model();
            model
                .cursor()
                .filter(|_| model.selected_count() <= 1)
                .and_then(|c| model.with_entry(c, |e| (!e.is_dir).then(|| self.tab().loc.path_of(e))).flatten())
                .filter(|p| playable(p).is_some())
        } else {
            None
        };
        if want.as_deref() == self.media.as_ref().map(|m| m.path.as_path()) {
            return;
        }
        self.stop_media();
        if let Some(path) = want {
            self.start_media(path);
        }
    }

    pub(super) fn stop_media(&mut self) {
        self.media = None; // Drop shuts the player down and removes the window.
        if let Some(ui) = self.ui() {
            let b = ui.global::<Bridge>();
            b.set_media(0);
            b.set_media_playing(false);
            b.set_media_pos(0.0);
            b.set_media_time("".into());
            b.set_media_note("".into());
        }
    }

    #[cfg(windows)]
    fn start_media(&mut self, path: PathBuf) {
        let Some(video) = playable(&path) else { return };
        let Some(ui) = self.ui() else { return };
        let parent = imp::main_hwnd(&ui);
        trace("start");
        let inner = match imp::Player::new(&path, video, parent, self.media_muted) {
            Ok(p) => p,
            Err(e) => {
                ui.global::<Bridge>().set_media_note(format!("Can't play this file ({e}).").into());
                return;
            }
        };
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(200), || {
            with_app(|a| a.media_tick());
        });
        let b = ui.global::<Bridge>();
        b.set_media(if video { 1 } else { 2 });
        b.set_media_playing(true);
        b.set_media_muted(self.media_muted);
        b.set_media_note("".into());
        b.set_media_time("Loading…".into());
        self.media = Some(Media {
            path,
            started: Instant::now(),
            duration: 0,
            video,
            no_picture: false,
            inner,
            _timer: timer,
        });
        // Place the video window where Quick Look last reported its area.
        if video {
            let r = self.media_rect;
            self.place_media(r.0, r.1, r.2, r.3);
        }
    }

    #[cfg(not(windows))]
    fn start_media(&mut self, _path: PathBuf) {}

    fn media_tick(&mut self) {
        let Some(ui) = self.ui() else { return };
        let b = ui.global::<Bridge>();
        let Some(m) = self.media.as_mut() else { return };
        #[cfg(windows)]
        {
            let (pos, dur, playing) = m.inner.status();
            trace(&format!("tick {pos} {dur} {playing}"));
            if dur > 0 {
                m.duration = dur;
                // Loaded: now the video window can be shown and sized.
                let picture = m.inner.mark_ready();
                // Sound but still no picture a moment later: no decoder.
                if m.video && !picture && !m.no_picture && m.started.elapsed() > NO_PICTURE_AFTER {
                    m.no_picture = true;
                    b.set_media(2);
                    b.set_media_note(
                        "Sound only: Windows can't show this video's picture (HEVC needs the free \"HEVC Video Extensions\" from the Microsoft Store)."
                            .into(),
                    );
                }
            }
            let (duration, started) = (m.duration, m.started);
            if duration == 0 {
                if started.elapsed() > LOAD_TIMEOUT {
                    // Leave the thumbnail; explain instead of a dead player.
                    self.stop_media();
                    if let Some(ui) = self.ui() {
                        ui.global::<Bridge>()
                            .set_media_note("Windows can't play this format. Press Open to use another app.".into());
                    }
                }
                return;
            }
            b.set_media_playing(playing);
            b.set_media_pos((pos as f64 / duration as f64).clamp(0.0, 1.0) as f32);
            b.set_media_time(format!("{} / {}", clock(pos), clock(duration)).into());
        }
    }

    pub(super) fn media_toggle(&mut self) {
        let Some(m) = self.media.as_ref() else { return };
        #[cfg(windows)]
        m.inner.toggle(m.duration);
        self.media_tick();
    }

    pub(super) fn media_seek(&mut self, frac: f32) {
        let Some(m) = self.media.as_ref() else { return };
        #[cfg(windows)]
        if m.duration > 0 {
            m.inner.seek((m.duration as f64 * frac.clamp(0.0, 1.0) as f64) as i64);
        }
        self.media_tick();
    }

    /// Jumps `secs` seconds forward (or back when negative).
    pub(super) fn media_skip(&mut self, secs: i32) {
        let Some(m) = self.media.as_ref() else { return };
        #[cfg(windows)]
        if m.duration > 0 {
            let (pos, _, _) = m.inner.status();
            let t = (pos + secs as i64 * 10_000_000).clamp(0, (m.duration - 1).max(0));
            m.inner.seek(t);
        }
        self.media_tick();
    }

    pub(super) fn media_mute(&mut self) {
        self.media_muted = !self.media_muted;
        if let Some(ui) = self.ui() {
            ui.global::<Bridge>().set_media_muted(self.media_muted);
        }
        #[cfg(windows)]
        if let Some(m) = self.media.as_ref() {
            m.inner.set_mute(self.media_muted);
        }
    }

    /// Quick Look's video area moved or resized (logical pixels).
    pub(super) fn place_media(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.media_rect = (x, y, w, h);
        let Some(ui) = self.ui() else { return };
        let scale = ui.window().scale_factor();
        #[cfg(windows)]
        if let Some(m) = self.media.as_ref() {
            m.inner.place((x * scale) as i32, (y * scale) as i32, (w * scale) as i32, (h * scale) as i32);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::sync::Once;

    use slint::ComponentHandle;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantToInt64};
    use windows::Win32::System::Variant::VT_I8;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{HSTRING, w};

    pub fn main_hwnd(ui: &crate::MainWindow) -> Option<HWND> {
        use slint::winit_030::WinitWindowAccessor;
        use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        ui.window()
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as _)),
                _ => None,
            })
            .flatten()
    }

    pub struct Player {
        player: IMFPMediaPlayer,
        /// Child window the video is drawn in (video only).
        hwnd: Option<HWND>,
        /// Shown only once the video has loaded and has a picture: an
        /// unpainted child window shows stale screen content.
        ready: std::cell::Cell<bool>,
        rect: std::cell::Cell<(i32, i32, i32, i32)>,
    }

    impl Player {
        pub fn new(path: &Path, video: bool, parent: Option<HWND>, muted: bool) -> windows::core::Result<Self> {
            static STARTUP: Once = Once::new();
            STARTUP.call_once(|| unsafe {
                let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
            });
            let hwnd = match (video, parent) {
                (true, Some(parent)) => unsafe {
                    // Parent must not paint over the child (softbuffer blits).
                    let style = GetWindowLongPtrW(parent, GWL_STYLE);
                    if style & WS_CLIPCHILDREN.0 as isize == 0 {
                        SetWindowLongPtrW(parent, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);
                    }
                    // A plain black STATIC: mouse clicks pass through to the app.
                    Some(CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("STATIC"),
                        None,
                        WS_CHILD | WS_CLIPSIBLINGS | WINDOW_STYLE(0x0004) /* SS_BLACKRECT */,
                        0,
                        0,
                        1,
                        1,
                        Some(parent),
                        None,
                        None,
                        None,
                    )?)
                },
                _ => None,
            };
            let mut player: Option<IMFPMediaPlayer> = None;
            let url = HSTRING::from(path.as_os_str());
            super::trace("create player");
            let made = unsafe {
                MFPCreateMediaPlayer(&url, true, MFP_OPTION_NONE, None::<&IMFPMediaPlayerCallback>, hwnd, Some(&mut player))
            };
            let made_ok = made.is_ok();
            let player = match (made, player) {
                (Ok(()), Some(p)) => p,
                (Err(e), _) => {
                    destroy(hwnd);
                    return Err(e);
                }
                (Ok(()), None) => {
                    destroy(hwnd);
                    return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
                }
            };
            super::trace(&format!("created {:?}", made_ok));
            unsafe {
                let _ = player.SetBorderColor(windows::Win32::Foundation::COLORREF(0));
                let _ = player.SetMute(muted);
            }
            Ok(Self { player, hwnd, ready: Default::default(), rect: Default::default() })
        }

        /// (position, duration) in 100 ns units, and whether it is playing.
        pub fn status(&self) -> (i64, i64, bool) {
            unsafe {
                let read = |pv: windows::core::Result<PROPVARIANT>| pv.ok().and_then(|v| PropVariantToInt64(&v).ok()).unwrap_or(0);
                let pos = read(self.player.GetPosition(&MFP_POSITIONTYPE_100NS));
                let dur = read(self.player.GetDuration(&MFP_POSITIONTYPE_100NS));
                let playing = self.player.GetState().map(|s| s == MFP_MEDIAPLAYER_STATE_PLAYING).unwrap_or(false);
                (pos, dur, playing)
            }
        }

        pub fn toggle(&self, duration: i64) {
            unsafe {
                match self.player.GetState() {
                    Ok(s) if s == MFP_MEDIAPLAYER_STATE_PLAYING => {
                        let _ = self.player.Pause();
                    }
                    _ => {
                        // Finished: start over.
                        let (pos, _, _) = self.status();
                        if duration > 0 && pos >= duration - 2_000_000 {
                            self.seek(0);
                        }
                        let _ = self.player.Play();
                    }
                }
            }
        }

        pub fn seek(&self, t: i64) {
            let mut pv = PROPVARIANT::default();
            unsafe {
                let inner = &mut *pv.Anonymous.Anonymous;
                inner.vt = VT_I8;
                inner.Anonymous.hVal = t;
                let _ = self.player.SetPosition(&MFP_POSITIONTYPE_100NS, &pv);
            }
        }

        pub fn set_mute(&self, mute: bool) {
            unsafe {
                let _ = self.player.SetMute(mute);
            }
        }

        /// The video has loaded: show its window if it has a picture.
        /// Returns false for files without a video stream.
        pub fn mark_ready(&self) -> bool {
            if self.ready.get() {
                return true;
            }
            let mut size = windows::Win32::Foundation::SIZE::default();
            unsafe {
                let _ = self.player.GetNativeVideoSize(Some(&mut size), None);
            }
            if size.cx <= 0 || size.cy <= 0 {
                return false;
            }
            self.ready.set(true);
            let (x, y, w, h) = self.rect.get();
            self.place(x, y, w, h);
            true
        }

        pub fn place(&self, x: i32, y: i32, w: i32, h: i32) {
            self.rect.set((x, y, w, h));
            let Some(hwnd) = self.hwnd else { return };
            unsafe {
                let flags = if self.ready.get() && w > 2 && h > 2 { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
                let _ = SetWindowPos(hwnd, Some(HWND_TOP), x, y, w.max(1), h.max(1), flags | SWP_NOACTIVATE);
                let _ = self.player.UpdateVideo();
            }
        }
    }

    fn destroy(hwnd: Option<HWND>) {
        if let Some(h) = hwnd {
            unsafe {
                let _ = DestroyWindow(h);
            }
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            super::trace("drop: stop");
            unsafe {
                let _ = self.player.Stop();
                super::trace("drop: shutdown");
                let _ = self.player.Shutdown();
            }
            super::trace("drop: destroy");
            destroy(self.hwnd);
            super::trace("drop: done");
        }
    }
}
