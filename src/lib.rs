// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! day-piece-media: an external Day Piece (DESIGN.md §15) wrapping each toolkit's native media
//! player: AVPlayerView on AppKit, AVPlayerViewController on UIKit, QMediaPlayer + QVideoWidget on
//! Qt, `android.widget.VideoView` (or a bare `MediaPlayer` for sound) on Android, GtkVideo on GTK,
//! MediaPlayerElement on XAML, `<video>`/`<audio>` on the web, and an ArkTS `Video` / `AVPlayer` on
//! HarmonyOS. One Rust API registered link-time into each backend's renderer slice without touching
//! day. Like the webview it carries both a front-end and its native backends, including an
//! Android manifest permission contribution (INTERNET) and an iOS framework contribution (AVKit +
//! AVFoundation), see docs/extending.md.
//!
//! The player is a growing leaf that fills its space (constrain it with `.frame(w, h)`), unless it
//! is `.audio_only()`: a sound-only player draws nothing and measures zero, so a radio app can drop
//! it anywhere in its tree and build its own now-playing UI around it. The `url` source accepts a
//! plain string, a `Signal<String>`, or a closure, and may name a local file path or an
//! http(s)/file URL; every backend's loader takes both. Configure playback at build with
//! `.autoplay(bool)` / `.looping(bool)` / `.muted(bool)` / `.controls(bool)`; transport is
//! imperative and modeled with `Copy` `Trigger`s: `.play()` / `.pause()` / `.stop()` drive playback
//! and `.load()` re-reads the bound url (then plays), each `watch`ed to a `MediaPatch`.
//! `.volume(…)` is a tracked fraction (a constant, a `Signal<f64>`, or a closure) patched through
//! as it changes.
//!
//! Playback state comes back through `.state(binding)`: every arm reports what its native player is
//! doing (loading, playing, paused, ended, failed) on the piece's `Event::Custom` channel, and the
//! front-end writes it into the bound `PlaybackState` (a `Signal`, or any other `Binding`). That is
//! the readback docs/media.md reserved the channel for: native chrome, the network, and the app's
//! own triggers all move the player, and the binding is where they agree.
//!
//! Native chrome (`.controls(true)`, the default) is free where the toolkit has it: AVPlayerView's
//! inline controls, AVPlayerViewController's playback controls, Android's MediaController. Qt's
//! QVideoWidget has no built-in chrome (drive it with triggers), and GtkVideo's overlay controls
//! are always on. See docs/media.md for the per-backend caveats.

#[cfg(feature = "dom")]
mod browser;

use day_core::{BuildCx, Flex, Piece, RNode, with_tree};
use day_pieces::{Decorated, FractionSource, IntoFraction, IntoText, TextSource};
use day_reactive::{AnyBinding, Binding, Trigger, untrack, watch};
use day_spec::Event;

mod session;
pub use session::{AudioSession, BackgroundAudio, PlaybackSnapshot};
mod timeline;
pub use timeline::{MediaCapabilities, MediaError, MediaErrorKind, MediaTimeline, TimeRange};
/// Audio file metadata (ID3, MP4 atoms, FLAC and Vorbis comments, RIFF and AIFF chunks), read
/// from the headers alone; the scanner behind a music library.
pub mod tags;

pub const KIND: &str = "day.piece.media";

/// Full props (realize). The initial `url` loads when the native view is created; the flags are
/// fixed at build time.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaProps {
    /// A local file path or an http(s)/file URL.
    pub url: String,
    /// Start playing as soon as the media is ready (default true).
    pub autoplay: bool,
    /// Restart from the beginning when playback reaches the end (default false).
    pub looping: bool,
    /// Silence the audio track (default false).
    pub muted: bool,
    /// Show the toolkit's native transport chrome where it has one (default true).
    pub controls: bool,
    /// Sound only (default false): the leaf draws nothing, has no chrome, and measures zero.
    /// Each arm builds the toolkit's bare audio player rather than its video view.
    pub audio_only: bool,
    /// Output volume, `0.0` (silent) to `1.0` (full) (default 1.0).
    pub volume: f64,
}

impl Default for MediaProps {
    fn default() -> Self {
        MediaProps {
            url: String::new(),
            autoplay: true,
            looping: false,
            muted: false,
            controls: true,
            audio_only: false,
            volume: 1.0,
        }
    }
}

/// Sparse imperative commands sent to the native player after creation.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaPatch {
    /// Load a url (from `.load()`, which re-reads the bound source) and start playing it.
    Load(String),
    /// Resume/start playback (from `.play()`).
    Play,
    /// Pause playback (from `.pause()`).
    Pause,
    /// Stop playback and drop the source (from `.stop()`): a paused live stream keeps its
    /// connection open and its buffer filling, a stopped one lets both go. The next `Load`
    /// starts afresh.
    Stop,
    /// Set the output volume, `0.0..=1.0` (from `.volume(…)` changing).
    Volume(f64),
    /// Seek to an absolute position in seconds. Does not change paused/playing intent.
    Seek(f64),
    /// Positive playback speed (0.25–4); unsupported speeds report a command error.
    Rate(f64),
    /// Request current timeline/capabilities. Used internally by the observation timer.
    Poll,
    /// Configure host background playback. Text is supplied already localized by the app.
    Background(BackgroundAudio),
    /// What the system's own now-playing surface shows (the lock screen, the media keys'
    /// overlay, MPRIS clients): `None` clears it.
    NowPlaying(Option<NowPlayingInfo>),
}

/// What is on, as the system's now-playing surfaces show it (see [`AudioSession::now_playing`]).
/// Positions are seconds; `duration` is `None` for a live stream.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NowPlayingInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// A local image file for the artwork, where the app has one.
    pub artwork_path: Option<String>,
    /// Else a URL the arm may fetch itself (the web and HarmonyOS take a URL directly).
    pub artwork_url: Option<String>,
    pub duration: Option<f64>,
    pub position: f64,
    /// The playback rate while playing (`1.0` normally), so the system can advance the clock.
    pub rate: f64,
    pub playing: bool,
    /// A live stream: no seeking, the position means nothing.
    pub live: bool,
    pub has_next: bool,
    pub has_previous: bool,
}

/// A command from the system's transport controls: the media keys, the lock screen, a
/// headset button, an MPRIS client. Delivered through [`AudioSession::on_remote`] or
/// [`Media::on_remote`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RemoteCommand {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
    /// To an absolute position in seconds.
    Seek(f64),
    /// Ahead by seconds (a podcast app's 30-second skip).
    SkipForward(f64),
    SkipBackward(f64),
}

impl RemoteCommand {
    /// Parse a [`report::REMOTE`] payload: a verb, with `seek:<secs>`, `forward:<secs>` and
    /// `backward:<secs>` carrying a number.
    pub fn from_report(text: &str) -> Option<RemoteCommand> {
        let text = text.trim();
        let (verb, value) = text.split_once(':').unwrap_or((text, ""));
        let secs = || value.trim().parse::<f64>().ok().filter(|v| v.is_finite());
        Some(match verb {
            "play" => RemoteCommand::Play,
            "pause" => RemoteCommand::Pause,
            "toggle" => RemoteCommand::Toggle,
            "stop" => RemoteCommand::Stop,
            "next" => RemoteCommand::Next,
            "previous" => RemoteCommand::Previous,
            "seek" => RemoteCommand::Seek(secs()?),
            "forward" => RemoteCommand::SkipForward(secs().unwrap_or(30.0)),
            "backward" => RemoteCommand::SkipBackward(secs().unwrap_or(15.0)),
            _ => return None,
        })
    }

    /// The wire form [`from_report`](Self::from_report) reads.
    pub fn to_report(self) -> String {
        match self {
            RemoteCommand::Play => "play".into(),
            RemoteCommand::Pause => "pause".into(),
            RemoteCommand::Toggle => "toggle".into(),
            RemoteCommand::Stop => "stop".into(),
            RemoteCommand::Next => "next".into(),
            RemoteCommand::Previous => "previous".into(),
            RemoteCommand::Seek(s) => format!("seek:{s}"),
            RemoteCommand::SkipForward(s) => format!("forward:{s}"),
            RemoteCommand::SkipBackward(s) => format!("backward:{s}"),
        }
    }
}

/// What the native player is doing, as reported through [`Media::state`].
///
/// The arms agree on this vocabulary and nothing finer: a live stream has no position or
/// duration worth reporting, and the states an app draws a transport from are these five.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PlaybackState {
    /// No source, or the source was stopped.
    #[default]
    Idle,
    /// A source is set and the player is connecting or buffering; nothing audible yet.
    Loading,
    Playing,
    Paused,
    /// The source played to its end (a file; a live stream never does).
    Ended,
    /// The player gave up on the source. The text is the toolkit's own message.
    Error(String),
}

impl PlaybackState {
    /// Sound is (or is about to be) coming out: playing, or loading on the way to playing.
    pub fn is_active(&self) -> bool {
        matches!(self, PlaybackState::Playing | PlaybackState::Loading)
    }

    /// The report an arm sends: `num` is one of the [`report`] codes and `text` the detail
    /// (an error's message). Unknown codes read as `Idle` rather than being dropped, so a
    /// backend that grows a state the front-end does not know still resets a stale one.
    pub fn from_report(num: f64, text: &str) -> Self {
        match num as i32 {
            report::LOADING => PlaybackState::Loading,
            report::PLAYING => PlaybackState::Playing,
            report::PAUSED => PlaybackState::Paused,
            report::ENDED => PlaybackState::Ended,
            report::ERROR => PlaybackState::Error(text.to_string()),
            _ => PlaybackState::Idle,
        }
    }
}

/// The `num` codes the arms report a [`PlaybackState`] with on the node's `Event::Custom`
/// channel, tagged [`report::TAG`]. Plain integers rather than an enum, because they cross the
/// JNI, C-ABI, ArkTS, and wasm boundaries where only a number survives.
pub mod report {
    /// The `Event::Custom` tag the in-process arms attach (cross-boundary ones carry none).
    pub const TAG: &str = "media:state";
    pub const IDLE: i32 = 0;
    pub const LOADING: i32 = 1;
    pub const PLAYING: i32 = 2;
    pub const PAUSED: i32 = 3;
    pub const ENDED: i32 = 4;
    pub const ERROR: i32 = 5;
    /// Not a state: the stream said what it is playing. `text` is the raw in-band value,
    /// an ICY `StreamTitle` (`Artist - Title`) or the ID3/timed fields packed
    /// `title\u{1f}artist\u{1f}album`, which [`StreamMetadata::from_report`](crate::StreamMetadata::from_report) parses.
    pub const METADATA: i32 = 6;
    pub const TIMELINE: i32 = 7;
    pub const COMMAND_ERROR: i32 = 8;
    /// Not a state: the system's transport controls asked for something. `text` is a
    /// [`RemoteCommand`](crate::RemoteCommand) in its wire form.
    pub const REMOTE: i32 = 9;
}

/// What the stream says it is playing, as the arm delivered it and as parsed for display.
///
/// Internet radio carries "now playing" in one of two ways: the ICY/Shoutcast in-band
/// `StreamTitle` that Icecast servers interleave into an MP3/AAC stream on request, or
/// timed ID3 in an HLS stream. Both reach this struct; `raw` keeps the exact text for a
/// later lookup (lyrics, a catalog), `title`/`artist`/`album` are the best split of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamMetadata {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The value as received: a `StreamTitle` string, or the timed fields joined with ` - `.
    pub raw: String,
}

impl StreamMetadata {
    /// Parse a [`report::METADATA`] payload. Packed fields (`\u{1f}`-separated) are taken as
    /// they are; a bare `StreamTitle` splits on its first ` - `, the convention nearly every
    /// station follows (`Artist - Title`). A value with no dash is all title.
    pub fn from_report(text: &str) -> StreamMetadata {
        let text = text.trim();
        if text.contains('\u{1f}') {
            let mut parts = text.split('\u{1f}').map(|p| p.trim().to_string());
            let title = parts.next().unwrap_or_default();
            let artist = parts.next().unwrap_or_default();
            let album = parts.next().unwrap_or_default();
            let raw = [title.as_str(), artist.as_str(), album.as_str()]
                .iter()
                .filter(|p| !p.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" - ");
            return StreamMetadata {
                title,
                artist,
                album,
                raw,
            };
        }
        match text.split_once(" - ") {
            Some((artist, title)) if !artist.trim().is_empty() && !title.trim().is_empty() => {
                StreamMetadata {
                    title: title.trim().to_string(),
                    artist: artist.trim().to_string(),
                    album: String::new(),
                    raw: text.to_string(),
                }
            }
            _ => StreamMetadata {
                title: text.to_string(),
                artist: String::new(),
                album: String::new(),
                raw: text.to_string(),
            },
        }
    }

    /// Whether anything was said at all.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
}

/// A native media player bound to `url`. Attach command triggers with `.play()/.pause()/.load()`;
/// fire them (`Trigger::notify`) from buttons.
pub struct Media {
    url: TextSource,
    autoplay: bool,
    looping: bool,
    muted: bool,
    controls: bool,
    audio_only: bool,
    volume: Option<FractionSource>,
    play: Option<Trigger>,
    pause: Option<Trigger>,
    stop: Option<Trigger>,
    load: Option<Trigger>,
    state: Option<AnyBinding<PlaybackState>>,
    metadata: Option<AnyBinding<Option<StreamMetadata>>>,
    timeline: Option<AnyBinding<MediaTimeline>>,
    error: Option<AnyBinding<Option<MediaError>>>,
    seek: Option<(AnyBinding<f64>, Trigger)>,
    rate: Option<AnyBinding<f64>>,
    remote: Option<std::rc::Rc<dyn Fn(RemoteCommand)>>,
}

/// `media(url)`: a native audio/video player for `url` (a string, `Signal<String>`, or closure;
/// a file path or an http(s) URL). The initial value loads on creation and autoplays by default;
/// call `.load(trigger)` and fire the trigger to (re)load whatever `url` currently holds.
pub fn media<M>(url: impl IntoText<M>) -> Media {
    // Self-register the web renderer. wasm has no link-time renderer slice, and a constructor is
    // the earliest point the piece is known to be in play, always before its node is realized.
    #[cfg(all(feature = "dom", target_arch = "wasm32"))]
    dom_impl::register();
    Media {
        url: url.into_text(),
        autoplay: true,
        looping: false,
        muted: false,
        controls: true,
        audio_only: false,
        volume: None,
        play: None,
        pause: None,
        stop: None,
        load: None,
        state: None,
        metadata: None,
        timeline: None,
        error: None,
        seek: None,
        rate: None,
        remote: None,
    }
}

#[day_pieces::builder(MediaBuilder)]
impl Media {
    /// Wait for a command instead of playing as soon as the media is ready (the default
    /// autoplays).
    pub fn no_autoplay(mut self) -> Self {
        self.autoplay = false;
        self
    }
    /// Restart from the beginning when playback reaches the end (the default plays once).
    pub fn looping(mut self) -> Self {
        self.looping = true;
        self
    }
    /// Silence the audio track.
    pub fn muted(mut self) -> Self {
        self.muted = true;
        self
    }
    /// Hide the toolkit's native transport chrome (shown by default where the toolkit has one).
    /// Qt has no free chrome (use triggers) and GtkVideo's overlay controls cannot be hidden; see
    /// docs/media.md.
    pub fn no_controls(mut self) -> Self {
        self.controls = false;
        self
    }
    /// Sound only: the piece draws nothing, has no chrome, and measures zero, so it can sit
    /// anywhere in the tree while the app draws its own transport (a radio's now-playing bar).
    pub fn audio_only(mut self) -> Self {
        self.audio_only = true;
        self
    }
    /// Output volume, `0.0..=1.0`: a constant, a `Signal<f64>`, or a closure. A tracked source
    /// patches the player whenever it changes, so one `slider` binding drives it.
    pub fn volume<M>(mut self, volume: impl IntoFraction<M>) -> Self {
        self.volume = Some(volume.into_fraction());
        self
    }
    /// Resume/start playback whenever `trigger` fires.
    pub fn play(mut self, trigger: Trigger) -> Self {
        self.play = Some(trigger);
        self
    }
    /// Pause playback whenever `trigger` fires.
    pub fn pause(mut self, trigger: Trigger) -> Self {
        self.pause = Some(trigger);
        self
    }
    /// Stop playback and drop the source whenever `trigger` fires (see [`MediaPatch::Stop`]).
    pub fn stop(mut self, trigger: Trigger) -> Self {
        self.stop = Some(trigger);
        self
    }
    /// Re-read the bound `url` and load + play it whenever `trigger` fires.
    pub fn load(mut self, trigger: Trigger) -> Self {
        self.load = Some(trigger);
        self
    }
    /// Where the piece writes what the native player is doing. Written on every change the
    /// toolkit reports, whoever caused it: a trigger, the native chrome, or the network. A
    /// `Signal<PlaybackState>` or any other binding.
    pub fn state(mut self, state: impl Binding<PlaybackState>) -> Self {
        self.state = Some(AnyBinding::new(state));
        self
    }
    /// Observe the native timeline and source-dependent capabilities, approximately 4 Hz.
    pub fn timeline(mut self, timeline: impl Binding<MediaTimeline>) -> Self {
        self.timeline = Some(AnyBinding::new(timeline));
        self
    }
    /// Observe structured command and playback failures; native diagnostics are not UI text.
    pub fn error(mut self, error: impl Binding<Option<MediaError>>) -> Self {
        self.error = Some(AnyBinding::new(error));
        self
    }
    /// Seek to the binding's current seconds when the trigger fires (not on every slider tick).
    pub fn seek(mut self, seconds: impl Binding<f64>, trigger: Trigger) -> Self {
        self.seek = Some((AnyBinding::new(seconds), trigger));
        self
    }
    /// Desired playback speed, preserved across pause/resume. Observe `timeline.rate` for actual speed.
    pub fn rate(mut self, rate: impl Binding<f64>) -> Self {
        self.rate = Some(AnyBinding::new(rate));
        self
    }
    /// Receive what the stream says it is playing (docs/media.md). `None` until the stream
    /// says anything, and again on every load. Where the native player surfaces the stream's
    /// own metadata (AVFoundation's timed metadata on macOS and iOS: ICY `StreamTitle` and
    /// HLS ID3 alike) it arrives from there; elsewhere the piece asks the stream itself, on a
    /// second, short-lived connection with `Icy-MetaData: 1` every [`ICY_PROBE_SECS`]
    /// seconds (the same header the players send), read only as far as the first metadata
    /// block. Not available on the web (a page cannot read a cross-origin stream).
    pub fn metadata(mut self, metadata: impl Binding<Option<StreamMetadata>>) -> Self {
        self.metadata = Some(AnyBinding::new(metadata));
        self
    }
    /// Receive the system's transport commands (media keys, lock screen, MPRIS, a headset)
    /// aimed at this player. Nothing happens on its own: the app runs the command through
    /// its own triggers, so its queue and its UI stay in agreement.
    pub fn on_remote(mut self, f: impl Fn(RemoteCommand) + 'static) -> Self {
        self.remote = Some(std::rc::Rc::new(f));
        self
    }
}

impl Media {
    fn mount(self, create: impl FnOnce(&MediaProps) -> RNode) -> RNode {
        let Media {
            url,
            autoplay,
            looping,
            muted,
            controls,
            audio_only,
            volume,
            play,
            pause,
            stop,
            load,
            state,
            metadata,
            timeline,
            error,
            seek,
            rate,
            remote,
        } = self;
        let initial = MediaProps {
            url: url.initial(),
            autoplay,
            looping,
            muted,
            controls,
            audio_only,
            volume: volume.as_ref().map_or(1.0, FractionSource::initial),
        };
        let node = create(&initial);
        let mut cx = BuildCx::new(node);

        let send = move |patch: MediaPatch| {
            with_tree(|t| t.patch(node, Box::new(patch), false));
        };
        // The stream's own metadata: a probe of the stream on the side (icy.rs), started for
        // every source the player is given and stopped when playback is. The Apple arms also
        // report what AVFoundation hands over (HLS ID3 rides only that way), and the two say
        // the same thing when both speak; the web has no way to read a cross-origin stream,
        // so the probe is off there.
        let probe: Option<std::rc::Rc<icy::Probe>> = match metadata {
            Some(m) if cfg!(not(target_arch = "wasm32")) => Some(icy::Probe::new(m)),
            _ => None,
        };
        if let Some(p) = &probe
            && !initial.url.is_empty()
            && initial.autoplay
        {
            p.start(initial.url.clone());
        }

        // Each command trigger → one patch. `watch` never fires for the initial value, so wiring
        // these does not issue a spurious command at build time (the initial url loads via props).
        if let Some(play) = play {
            watch(move || play.track(), move |_, _| send(MediaPatch::Play));
        }
        if let Some(pause) = pause {
            watch(move || pause.track(), move |_, _| send(MediaPatch::Pause));
        }
        if let Some(stop) = stop {
            let probe = probe.clone();
            watch(
                move || stop.track(),
                move |_, _| {
                    if let Some(p) = &probe {
                        p.stop();
                    }
                    send(MediaPatch::Stop)
                },
            );
        }
        if let Some(load) = load {
            // Re-read the bound url when the trigger fires: a `Static` source re-loads the fixed
            // string (a restart-from-source), a `Signal`/closure source reads its current value.
            let read: std::rc::Rc<dyn Fn() -> String> = match url {
                TextSource::Static(s) => std::rc::Rc::new(move || s.clone()),
                TextSource::Dyn(f) => f,
            };
            let probe = probe.clone();
            watch(
                move || load.track(),
                move |_, _| {
                    let url = untrack(|| read());
                    if let Some(sink) = timeline {
                        sink.set(MediaTimeline::default());
                    }
                    if let Some(sink) = error {
                        sink.set(None);
                    }
                    if let Some(sink) = metadata {
                        sink.set(None);
                    }
                    if let Some(p) = &probe {
                        p.start(url.clone());
                    }
                    send(MediaPatch::Load(url))
                },
            );
        }
        // A tracked volume follows its source; the initial value already rode in on the props.
        if let Some(FractionSource::Dyn(f)) = volume {
            watch(
                move || f().clamp(0.0, 1.0),
                move |v, _| send(MediaPatch::Volume(*v)),
            );
        }
        if let Some((seconds, trigger)) = seek {
            watch(
                move || trigger.track(),
                move |_, _| {
                    let value = seconds.get_untracked();
                    if value.is_finite() && value >= 0.0 {
                        send(MediaPatch::Seek(value));
                    } else if let Some(error) = error {
                        error.set(Some(MediaError::invalid("seek")));
                    }
                },
            );
        }
        if timeline.is_some()
            && cfg!(any(
                feature = "appkit",
                feature = "uikit",
                feature = "gtk",
                feature = "qt",
                feature = "mdc",
                feature = "xaml",
                feature = "arkui",
                feature = "dom"
            ))
        {
            let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let cleanup = alive.clone();
            day_reactive::Scope::current()
                .on_cleanup(move || cleanup.store(false, std::sync::atomic::Ordering::Release));
            poll_node(day_core::rnode_to_id(node), alive);
        }
        // The readback rail: every arm reports its player's state on this node's Custom channel.
        // A cross-boundary Custom (JNI, C-ABI, ArkTS, wasm) carries only `num`/`text`, so the
        // code is the discriminator, never the tag.
        cx.on(node, move |ev| {
            if let Event::Custom { num, text, .. } = ev {
                if *num as i32 == report::TIMELINE {
                    if let (Some(sink), Some(value)) = (timeline, MediaTimeline::decode(text)) {
                        sink.set_if_changed(value);
                    }
                    return;
                }
                if *num as i32 == report::REMOTE {
                    if let (Some(f), Some(cmd)) = (&remote, RemoteCommand::from_report(text)) {
                        f(cmd);
                    }
                    return;
                }
                if *num as i32 == report::COMMAND_ERROR {
                    if let Some(sink) = error {
                        sink.set(Some(MediaError::command(text)));
                    }
                    return;
                }
                if *num as i32 == report::ERROR
                    && let Some(sink) = error
                {
                    sink.set(Some(MediaError {
                        kind: MediaErrorKind::Playback,
                        operation: "playback".into(),
                        detail: text.clone(),
                    }));
                }
                if *num as i32 == report::IDLE
                    && let Some(sink) = timeline
                {
                    sink.set_if_changed(MediaTimeline::default());
                }
                if *num as i32 == report::METADATA {
                    if let Some(metadata) = metadata {
                        let next = StreamMetadata::from_report(text);
                        let next = if next.is_empty() { None } else { Some(next) };
                        if metadata.get_untracked() != next {
                            metadata.set(next);
                        }
                    }
                    return;
                }
                if let Some(state) = state {
                    let next = PlaybackState::from_report(*num, text);
                    if state.get_untracked() != next {
                        state.set(next);
                    }
                }
            }
        });
        if let Some(rate) = rate {
            let apply = move |value: f64| {
                if valid_rate(value) {
                    send(MediaPatch::Rate(value));
                } else if let Some(error) = error {
                    error.set(Some(MediaError::invalid("rate")));
                }
            };
            // Native players may still be preparing; each backend retains desired speed.
            apply(rate.get_untracked());
            watch(move || rate.get(), move |value, _| apply(*value));
        }
        node
    }
}

// ---------------------------------------------------------------------------
// Per-toolkit native renderers, one file per backend. Each module registers a `Renderer`
// link-time into its backend's `RENDERERS` slice; `#[cfg]` gates each to its feature + target, and
// `#[path]` keeps the files grouped next to lib.rs. xaml/mock register nothing (the media kind
// falls back to day's placeholder leaf there).
// ---------------------------------------------------------------------------

impl Piece for Media {
    fn build(self, cx: &mut BuildCx) -> RNode {
        self.mount(|props| {
            cx.leaf(
                KIND,
                props,
                Flex {
                    grow_w: !props.audio_only,
                    grow_h: !props.audio_only,
                    ..Default::default()
                },
            )
        })
    }
}

day_pieces::glue_modules!(appkit, gtk, qt, uikit, mdc, xaml, arkui, dom);

/// How often the side probe re-reads a stream's in-band metadata, where it is used.
pub const ICY_PROBE_SECS: u32 = 20;

#[path = "icy.rs"]
mod icy;

#[cfg(all(any(feature = "gtk", feature = "qt"), target_os = "linux"))]
mod mpris;

// GtkVideo is core GTK, so this compiles on every gtk host, but playback needs a gstreamer media
// backend in the gtk4 build (Linux default; Homebrew gtk4 has none, so macos-gtk shows GtkVideo's
// own error UI; see Cargo.toml + docs/media.md).

fn valid_rate(rate: f64) -> bool {
    rate.is_finite() && (0.25..=4.0).contains(&rate)
}

// Scope cancellation also protects against IDs reused after an entire host restart.
fn poll_node(id: day_spec::NodeId, active: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    day_reactive::on_main_delayed(250, move || {
        if !active.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let alive = day_core::try_with_tree(|tree| {
            let node = day_core::id_to_rnode(id);
            if !tree.node_exists(node) {
                return false;
            }
            tree.patch(node, Box::new(MediaPatch::Poll), false);
            true
        })
        .unwrap_or(false);
        if alive {
            poll_node(id, active);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use day_mock::MockToolkit;
    use day_reactive::{Signal, flush_sync};
    use day_spec::WindowOptions;

    // Building + driving the piece must never panic, even with no native renderer registered
    // (the mock toolkit realizes unknown kinds as plain widgets and ignores unknown patches,
    // exactly like a backend built without this piece's feature).
    #[test]
    fn build_and_commands_do_not_panic() {
        let url = Signal::new("https://example.com/flower.mp4".to_string());
        let play = Trigger::new();
        let pause = Trigger::new();
        let stop = Trigger::new();
        let load = Trigger::new();
        let volume = Signal::new(0.5);
        let state = Signal::new(PlaybackState::Idle);

        day_core::uninstall_tree();
        let (mock, probe) = MockToolkit::new();
        let options = WindowOptions::default().title("test").size(400.0, 300.0);
        day_core::launch_with(mock, options, move || {
            day_core::AnyPiece::new(
                media(url)
                    .no_autoplay()
                    .looping()
                    .muted()
                    .no_controls()
                    .audio_only()
                    .volume(volume)
                    .play(play)
                    .pause(pause)
                    .stop(stop)
                    .load(load)
                    .state(state),
            )
        });

        let found = probe.find_by_kind(KIND);
        assert_eq!(found.len(), 1, "one media leaf realized");

        // Fire every command trigger; each becomes a MediaPatch the mock ignores gracefully.
        play.notify();
        pause.notify();
        stop.notify();
        volume.set(0.25);
        url.set("file:///tmp/other.mp4".to_string());
        load.notify();
        flush_sync();
        // Nothing reported: the mock has no player, so the state stays where the app left it.
        assert_eq!(state.get_untracked(), PlaybackState::Idle);
    }

    /// The wire codes round-trip into the states an app matches on, and an unknown code lands
    /// on `Idle` rather than being dropped.
    #[test]
    fn metadata_reports_parse() {
        let m = StreamMetadata::from_report("Miles Davis - So What");
        assert_eq!(
            (m.artist.as_str(), m.title.as_str()),
            ("Miles Davis", "So What")
        );
        assert_eq!(m.raw, "Miles Davis - So What");
        let m = StreamMetadata::from_report("Morning Show");
        assert_eq!(m.title, "Morning Show");
        assert!(m.artist.is_empty());
        let m = StreamMetadata::from_report("So What\u{1f}Miles Davis\u{1f}Kind of Blue");
        assert_eq!(m.album, "Kind of Blue");
        assert_eq!(m.raw, "So What - Miles Davis - Kind of Blue");
        assert!(StreamMetadata::from_report("  ").is_empty());
    }

    #[test]
    fn remote_commands_round_trip() {
        for c in [
            RemoteCommand::Play,
            RemoteCommand::Toggle,
            RemoteCommand::Seek(12.5),
            RemoteCommand::SkipForward(30.0),
            RemoteCommand::SkipBackward(15.0),
        ] {
            assert_eq!(RemoteCommand::from_report(&c.to_report()), Some(c));
        }
        assert_eq!(
            RemoteCommand::from_report("forward"),
            Some(RemoteCommand::SkipForward(30.0))
        );
        assert_eq!(RemoteCommand::from_report("seek:nan"), None);
        assert_eq!(RemoteCommand::from_report("dance"), None);
    }

    #[test]
    fn reports_decode_to_states() {
        assert_eq!(
            PlaybackState::from_report(report::PLAYING as f64, ""),
            PlaybackState::Playing
        );
        assert_eq!(
            PlaybackState::from_report(report::ERROR as f64, "no route to host"),
            PlaybackState::Error("no route to host".into())
        );
        assert_eq!(PlaybackState::from_report(42.0, ""), PlaybackState::Idle);
        assert!(PlaybackState::Loading.is_active());
        assert!(!PlaybackState::Paused.is_active());
    }
}
