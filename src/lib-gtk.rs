// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// GTK: gtk4::Video, a core GTK widget (compiles everywhere) backed by GtkMediaFile, which needs a
// gstreamer media backend in the gtk4 build for actual playback. Linux distro packages ship one
// (-Dmedia-gstreamer=enabled); Homebrew's gtk4 ships none, so on macos-gtk GtkVideo shows its own
// "no media backend" error UI (the same caveat class as webkitgtk; see docs/media.md). GtkVideo's
// overlay controls are always on; `.controls(false)` is a no-op here.
//
// A missing decoder is not an error to GTK: its GStreamer backend reports the stream prepared and
// plays it to the end with no video track, so the widget stays black and the state says Playing.
// `diagnose` notices a video player whose stream came up without video, asks GStreamer's
// discoverer what is missing, and puts the stream into GTK's error state: GtkVideo then shows its
// own error UI with the message, and `observe` reports `PlaybackState::Error`.
//
// A sound-only player needs no Video widget at all: GtkMediaStream plays on its own, so the leaf
// is an empty, invisible GtkBox of no size that carries the stream as widget data. Playback state
// comes from the stream's own notify signals (`playing`, `error`, `prepared`, `ended`).
//
// What is on goes to the desktop through MPRIS on the session bus (src/mpris.rs, Linux only),
// whose commands come back as `REMOTE` reports.
// ---------------------------------------------------------------------------

use super::*;
use day_toolkit_gtk::Gtk;
use day_spec::{NodeId, Proposal, Size};
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

/// The stream a sound-only leaf carries, keyed as widget data (a `Video` owns its stream itself).
const PROPS_KEY: &str = "day-media-props";
const STREAM_KEY: &str = "day-media-stream";
/// Marks a sound-only leaf, which measures zero.
const AUDIO_ONLY_KEY: &str = "day-media-audio-only";

/// `GtkMediaFile` from the one source string: an explicit scheme parses as a URI, anything else is
/// a local file path.
fn media_file(source: &str) -> gtk4::MediaFile {
    let file = if source.contains("://") {
        gio::File::for_uri(source)
    } else {
        gio::File::for_path(source)
    };
    gtk4::MediaFile::for_file(&file)
}

fn report(id: NodeId, code: i32, text: String) {
    day_toolkit_gtk::emit(
        id,
        Event::Custom {
            tag: report::TAG,
            num: code as f64,
            text,
        },
    );
}

/// Watch one stream: every notify becomes a report. A `Load` swaps streams, so this is called
/// per stream rather than per widget. `video` says whether the stream feeds a picture, so a
/// stream that prepares without one is worth diagnosing (see [`diagnose`]).
fn observe(stream: &gtk4::MediaStream, id: NodeId, video: bool) {
    let state = move |s: &gtk4::MediaStream| {
        if let Some(err) = s.error() {
            (report::ERROR, err.message().to_string())
        } else if s.is_ended() {
            (report::ENDED, String::new())
        } else if s.is_playing() {
            if s.is_prepared() {
                (report::PLAYING, String::new())
            } else {
                (report::LOADING, String::new())
            }
        } else if s.is_prepared() {
            (report::PAUSED, String::new())
        } else {
            (report::LOADING, String::new())
        }
    };
    let on = move |s: &gtk4::MediaStream| {
        let (code, text) = state(s);
        report(id, code, text);
    };
    stream.connect_playing_notify(on);
    stream.connect_prepared_notify(on);
    if video {
        stream.connect_prepared_notify(diagnose::check);
    }
    stream.connect_ended_notify(on);
    stream.connect_error_notify(on);
}

fn new_stream(url: &str, p: &MediaProps, id: NodeId) -> gtk4::MediaFile {
    let media = media_file(url);
    media.set_muted(p.muted);
    media.set_volume(p.volume.clamp(0.0, 1.0));
    media.set_loop(p.looping);
    observe(media.upcast_ref(), id, !p.audio_only);
    media
}

/// The node a leaf reports to, carried on the widget so a `Load` swap knows whom to tell.
const NODE_KEY: &str = "day-media-node";

fn remember_node(h: &gtk4::Widget, id: NodeId) {
    // SAFETY: a `u64` under a key private to this arm.
    unsafe { h.set_data(NODE_KEY, id.0) };
}

fn node_of(h: &gtk4::Widget) -> Option<NodeId> {
    // SAFETY: only ever a `u64` (see `remember_node`).
    unsafe { h.data::<u64>(NODE_KEY).map(|p| NodeId(*p.as_ref())) }
}

/// The stream behind a leaf, whichever shape it is.
fn stream_of(h: &gtk4::Widget) -> Option<gtk4::MediaStream> {
    if let Some(video) = h.downcast_ref::<gtk4::Video>() {
        return video.media_stream();
    }
    // SAFETY: the only value ever stored under this key is a `MediaStream` (see `set_stream`).
    unsafe {
        h.data::<gtk4::MediaStream>(STREAM_KEY)
            .map(|p| p.as_ref().clone())
    }
}

fn set_stream(h: &gtk4::Widget, stream: Option<gtk4::MediaStream>) {
    if let Some(old) = stream_of(h) {
        old.pause();
    }
    if let Some(video) = h.downcast_ref::<gtk4::Video>() {
        video.set_media_stream(stream.as_ref());
        return;
    }
    // SAFETY: replaces (and drops) whatever `MediaStream` the key held; the key is private to
    // this arm and only ever holds that type.
    unsafe {
        match stream {
            Some(s) => h.set_data(STREAM_KEY, s),
            None => {
                let _ = h.steal_data::<gtk4::MediaStream>(STREAM_KEY);
            }
        }
    }
}

fn make(_backend: &mut Gtk, p: &MediaProps, id: NodeId) -> gtk4::Widget {
    if p.audio_only {
        let host = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        host.set_visible(false);
        // SAFETY: a plain marker under a key private to this arm.
        unsafe { host.set_data(AUDIO_ONLY_KEY, true) };
        let widget: gtk4::Widget = host.upcast();
        remember_node(&widget, id);
        unsafe {
            widget.set_data(PROPS_KEY, p.clone());
        }
        if !p.url.is_empty() {
            let media = new_stream(&p.url, p, id);
            if p.autoplay {
                media.play();
            }
            set_stream(&widget, Some(media.upcast()));
        }
        return widget;
    }
    let video = gtk4::Video::new();
    video.set_autoplay(p.autoplay);
    video.set_loop(p.looping);
    if !p.url.is_empty() {
        let media = new_stream(&p.url, p, id);
        video.set_media_stream(Some(&media));
    }
    let widget: gtk4::Widget = video.upcast();
    remember_node(&widget, id);
    unsafe {
        widget.set_data(PROPS_KEY, p.clone());
    }
    widget
}

fn update(_backend: &mut Gtk, h: &gtk4::Widget, patch: &MediaPatch) {
    match patch {
        MediaPatch::Background(_) => {}
        MediaPatch::NowPlaying(info) => now_playing(h, info.as_ref()),
        MediaPatch::Poll => {
            if let Some(id) = node_of(h) {
                let mut value = MediaTimeline::default();
                value.capabilities.volume = true;
                if let Some(stream) = stream_of(h) {
                    // A MediaFile can fail synchronously, before notify handlers are attached.
                    if let Some(error) = stream.error() {
                        report(id, report::ERROR, error.message().to_string());
                    }
                    value.position = (stream.timestamp() as f64 / 1_000_000.0).max(0.0);
                    let duration = stream.duration() as f64 / 1_000_000.0;
                    value.duration = (duration > 0.0).then_some(duration);
                    value.seeking = stream.is_seeking();
                    value.capabilities.seek =
                        stream.is_prepared() && stream.is_seekable() && duration > 0.0;
                    if value.capabilities.seek {
                        value.seekable.push(TimeRange {
                            start: 0.0,
                            end: duration,
                        });
                    }
                }
                report(id, report::TIMELINE, value.encode());
            }
        }
        MediaPatch::Seek(seconds) => {
            if let Some(stream) = stream_of(h) {
                if stream.is_prepared()
                    && stream.is_seekable()
                    && seconds.is_finite()
                    && *seconds >= 0.0
                {
                    stream.seek(
                        (*seconds * 1_000_000.0)
                            .min(stream.duration() as f64)
                            .max(0.0) as i64,
                    );
                } else if let Some(id) = node_of(h) {
                    report(id, report::COMMAND_ERROR, "seek:unsupported".into());
                }
            }
        }
        MediaPatch::Rate(rate) => {
            if *rate != 1.0
                && let Some(id) = node_of(h)
            {
                report(id, report::COMMAND_ERROR, "rate:unsupported".into());
            }
        }
        MediaPatch::Load(url) => {
            // Keep properties even after Stop has released the stream.
            let props = unsafe { h.data::<MediaProps>(PROPS_KEY).map(|p| p.as_ref().clone()) }
                .unwrap_or_default();
            let media = media_file(url);
            media.set_muted(props.muted);
            media.set_volume(props.volume);
            media.set_loop(props.looping);
            if let Some(id) = node_of(h) {
                observe(
                    media.upcast_ref(),
                    id,
                    h.downcast_ref::<gtk4::Video>().is_some(),
                );
            }
            set_stream(h, Some(media.clone().upcast()));
            media.play();
        }
        MediaPatch::Play => {
            if let Some(stream) = stream_of(h) {
                stream.play();
            }
        }
        MediaPatch::Pause => {
            if let Some(stream) = stream_of(h) {
                stream.pause();
            }
        }
        MediaPatch::Stop => {
            if let Some(stream) = stream_of(h) {
                stream.pause();
                set_stream(h, None);
                if let Some(id) = node_of(h) {
                    report(id, report::IDLE, String::new());
                }
            }
        }
        MediaPatch::Volume(v) => {
            unsafe {
                if let Some(mut props) = h.data::<MediaProps>(PROPS_KEY) {
                    props.as_mut().volume = v.clamp(0.0, 1.0);
                }
            }
            if let Some(stream) = stream_of(h) {
                stream.set_volume(v.clamp(0.0, 1.0));
            }
        }
    }
}

/// Linux's now-playing surface is MPRIS (src/mpris.rs): the shell's media controls and
/// `playerctl` read it, and their commands come back as `REMOTE` reports on this node. macOS
/// has no session bus by default, so the macos-gtk build publishes nothing.
#[cfg(target_os = "linux")]
fn now_playing(h: &gtk4::Widget, info: Option<&NowPlayingInfo>) {
    if let Some(id) = node_of(h) {
        crate::mpris::install(|node, text| report(node, report::REMOTE, text));
        crate::mpris::publish(id, info.cloned());
    }
}

#[cfg(not(target_os = "linux"))]
fn now_playing(_h: &gtk4::Widget, _info: Option<&NowPlayingInfo>) {}

/// A sound-only leaf takes no room; a video fills what it is offered.
fn measure(backend: &mut Gtk, h: &gtk4::Widget, proposal: Proposal) -> Size {
    // SAFETY: only ever a `bool` (see `make`).
    if unsafe { h.data::<bool>(AUDIO_ONLY_KEY) }.is_some() {
        return Size::ZERO;
    }
    day_pieces::fill_measure(backend, h, proposal)
}

/// Silence the stream when the widget goes: a detached GtkMediaStream keeps playing.
fn release(_backend: &mut Gtk, h: &gtk4::Widget) {
    if let Some(stream) = stream_of(h) {
        stream.pause();
    }
    set_stream(h, None);
}

/// Why a video player shows no picture. GTK's GStreamer backend never reports a missing decoder:
/// it prepares the stream without the track it can't decode and plays on. When a video player's
/// stream prepares with no video, this asks GStreamer's discoverer, off the main thread, which
/// decoders the file needs and lacks, and on the main thread sets the stream's error to name
/// them. A file that really has no video (sound in a video player) keeps playing.
///
/// The discoverer lives in `libgstpbutils`, which the GTK media backend has already loaded; it is
/// opened at run time rather than linked, so building needs no GStreamer development packages.
/// Where it can't be loaded, only a stream with neither video nor audio is called an error.
mod diagnose {
    use gtk4::glib;
    use gtk4::prelude::*;
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::sync::OnceLock;

    /// Marks a stream already diagnosed, so a looping stream is asked about once.
    const CHECKED_KEY: &str = "day-media-diagnosed";

    /// What the discoverer found.
    enum Verdict {
        /// Descriptions of the elements the file needs and GStreamer lacks, such as
        /// "H.264 (High Profile) decoder".
        Missing(Vec<String>),
        /// Nothing missing: the file has no video to show.
        Complete,
        /// The discoverer couldn't be loaded or run.
        Unknown,
    }

    pub(super) fn check(stream: &gtk4::MediaStream) {
        if !stream.is_prepared() || stream.has_video() || stream.error().is_some() {
            return;
        }
        // SAFETY: a plain marker under a key private to this module.
        if unsafe { stream.data::<bool>(CHECKED_KEY) }.is_some() {
            return;
        }
        unsafe { stream.set_data(CHECKED_KEY, true) };
        let Some(uri) = stream
            .downcast_ref::<gtk4::MediaFile>()
            .and_then(|m| m.file())
            .map(|f| f.uri().to_string())
        else {
            return;
        };
        // The stream is main-thread only; the guard carries a weak reference to the worker and
        // back, and is only opened again on the main context's thread.
        let weak = glib::thread_guard::ThreadGuard::new(stream.downgrade());
        std::thread::spawn(move || {
            let verdict = discover(&uri);
            glib::MainContext::default().invoke(move || {
                let Some(stream) = weak.into_inner().upgrade() else {
                    return;
                };
                if stream.has_video() || stream.error().is_some() {
                    return;
                }
                if let Some(message) = message(&verdict, stream.has_audio(), &uri) {
                    stream.set_error(glib::Error::new(
                        gtk4::gio::IOErrorEnum::NotSupported,
                        &message,
                    ));
                }
            });
        });
    }

    /// The error to show, or `None` when the stream should keep playing (a file with sound
    /// and no video, or one the discoverer found nothing missing in).
    fn message(verdict: &Verdict, has_audio: bool, uri: &str) -> Option<String> {
        match verdict {
            Verdict::Missing(needed) => {
                let hint = if cfg!(target_os = "linux") {
                    " On Debian or Ubuntu, the gstreamer1.0-libav package provides the usual \
                     H.264 and AAC decoders."
                } else {
                    ""
                };
                Some(format!(
                    "This video can't be played: GStreamer needs the {} and doesn't have it \
                     installed.{hint}",
                    needed.join(" and ")
                ))
            }
            Verdict::Complete => None,
            Verdict::Unknown if has_audio => None,
            Verdict::Unknown => Some(format!(
                "This media can't be played: GStreamer decoded neither video nor sound from it. \
                 `gst-discoverer-1.0 {uri}` names what is missing."
            )),
        }
    }

    type InitCheck =
        unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char, *mut *mut c_void) -> c_int;
    type DiscovererNew = unsafe extern "C" fn(u64, *mut *mut c_void) -> *mut c_void;
    type DiscoverUri =
        unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> *mut c_void;
    type MissingDetails = unsafe extern "C" fn(*const c_void) -> *const *const c_char;

    /// The four GStreamer entry points, with the libraries that hold them kept open.
    struct Gst {
        _core: libloading::Library,
        _pbutils: libloading::Library,
        init_check: InitCheck,
        discoverer_new: DiscovererNew,
        discover_uri: DiscoverUri,
        missing_details: MissingDetails,
    }

    // SAFETY: plain C function pointers into libraries that stay loaded for the process.
    unsafe impl Send for Gst {}
    unsafe impl Sync for Gst {}

    fn open(names: &[&str]) -> Option<libloading::Library> {
        // SAFETY: GStreamer's libraries run no initialization with side effects on load.
        names
            .iter()
            .find_map(|n| unsafe { libloading::Library::new(n) }.ok())
    }

    fn gst() -> Option<&'static Gst> {
        static GST: OnceLock<Option<Gst>> = OnceLock::new();
        GST.get_or_init(|| {
            let core = open(&[
                "libgstreamer-1.0.so.0",
                "libgstreamer-1.0.0.dylib",
                "libgstreamer-1.0-0.dll",
                "gstreamer-1.0-0.dll",
            ])?;
            let pbutils = open(&[
                "libgstpbutils-1.0.so.0",
                "libgstpbutils-1.0.0.dylib",
                "libgstpbutils-1.0-0.dll",
                "gstpbutils-1.0-0.dll",
            ])?;
            // SAFETY: the signatures are GStreamer's (gst.h, gstdiscoverer.h), stable since 1.0.
            unsafe {
                let init_check = *core.get::<InitCheck>(b"gst_init_check\0").ok()?;
                let discoverer_new = *pbutils.get::<DiscovererNew>(b"gst_discoverer_new\0").ok()?;
                let discover_uri = *pbutils
                    .get::<DiscoverUri>(b"gst_discoverer_discover_uri\0")
                    .ok()?;
                let missing_details = *pbutils
                    .get::<MissingDetails>(
                        b"gst_discoverer_info_get_missing_elements_installer_details\0",
                    )
                    .ok()?;
                Some(Gst {
                    _core: core,
                    _pbutils: pbutils,
                    init_check,
                    discoverer_new,
                    discover_uri,
                    missing_details,
                })
            }
        })
        .as_ref()
    }

    /// Ask the discoverer what `uri` needs. Blocking (it reads the file's headers), so it runs
    /// on a worker thread.
    fn discover(uri: &str) -> Verdict {
        let Some(g) = gst() else {
            return Verdict::Unknown;
        };
        let Ok(curi) = CString::new(uri) else {
            return Verdict::Unknown;
        };
        // SAFETY: GStreamer calls with its documented contracts: null argc/argv and GError
        // out-params are allowed; the discoverer and its info are GObjects released with
        // g_object_unref; the installer-detail strings belong to the info and are copied out
        // before it is released.
        unsafe {
            if (g.init_check)(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == 0
            {
                return Verdict::Unknown;
            }
            let discoverer = (g.discoverer_new)(10_000_000_000, std::ptr::null_mut());
            if discoverer.is_null() {
                return Verdict::Unknown;
            }
            let info = (g.discover_uri)(discoverer, curi.as_ptr(), std::ptr::null_mut());
            let verdict = if info.is_null() {
                Verdict::Unknown
            } else {
                let mut needed = Vec::new();
                let details = (g.missing_details)(info);
                if !details.is_null() {
                    let mut i = 0;
                    while !(*details.add(i)).is_null() {
                        let detail = CStr::from_ptr(*details.add(i)).to_string_lossy();
                        needed.push(describe(&detail));
                        i += 1;
                    }
                }
                glib::gobject_ffi::g_object_unref(info.cast());
                if needed.is_empty() {
                    Verdict::Complete
                } else {
                    Verdict::Missing(needed)
                }
            };
            glib::gobject_ffi::g_object_unref(discoverer.cast());
            verdict
        }
    }

    /// The human description in a GStreamer installer detail,
    /// `gstreamer|1.0|<app>|<description>|<element detail>`.
    fn describe(detail: &str) -> String {
        detail.split('|').nth(3).unwrap_or(detail).to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn installer_details_yield_their_description() {
            let d = "gstreamer|1.0|day|H.264 (High Profile) decoder|decoder-video/x-h264, level=(string)3.1";
            assert_eq!(describe(d), "H.264 (High Profile) decoder");
            assert_eq!(describe("unexpected"), "unexpected");
        }

        #[test]
        fn only_a_missing_decoder_or_an_empty_stream_is_an_error() {
            let missing = Verdict::Missing(vec!["H.264 (High Profile) decoder".into()]);
            let m = message(&missing, true, "u").unwrap();
            assert!(m.contains("H.264 (High Profile) decoder"), "{m}");
            // Sound with no video, nothing missing: keep playing.
            assert!(message(&Verdict::Complete, true, "u").is_none());
            // No discoverer: only neither-video-nor-sound is called an error.
            assert!(message(&Verdict::Unknown, true, "u").is_none());
            assert!(
                message(&Verdict::Unknown, false, "u")
                    .unwrap()
                    .contains("gst-discoverer-1.0 u")
            );
        }
    }
}

// glib is what the widget-data helpers above are generic over; naming it lets a reader trace
// `set_data`/`data` to their trait.
#[allow(unused_imports)]
use glib::object::ObjectExt as _;

day_pieces::renderer!(day_toolkit_gtk::RENDERERS, Gtk,
    kind: KIND, props: MediaProps, patch: MediaPatch,
    make: make, update: update, measure: measure, release: release);
