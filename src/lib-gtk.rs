// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// GTK: gtk4::Video, a core GTK widget (compiles everywhere) backed by GtkMediaFile, which needs a
// gstreamer media backend in the gtk4 build for actual playback. Linux distro packages ship one
// (-Dmedia-gstreamer=enabled); Homebrew's gtk4 ships none, so on macos-gtk GtkVideo shows its own
// "no media backend" error UI (the same caveat class as webkitgtk; see docs/media.md). GtkVideo's
// overlay controls are always on; `.controls(false)` is a no-op here.
//
// A sound-only player needs no Video widget at all: GtkMediaStream plays on its own, so the leaf
// is an empty, invisible GtkBox of no size that carries the stream as widget data. Playback state
// comes from the stream's own notify signals (`playing`, `error`, `prepared`, `ended`).
// ---------------------------------------------------------------------------

use super::*;
use day_gtk::Gtk;
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
    day_gtk::emit(
        id,
        Event::Custom {
            tag: report::TAG,
            num: code as f64,
            text,
        },
    );
}

/// Watch one stream: every notify becomes a report. A `Load` swaps streams, so this is called
/// per stream rather than per widget.
fn observe(stream: &gtk4::MediaStream, id: NodeId) {
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
    stream.connect_ended_notify(on);
    stream.connect_error_notify(on);
}

fn new_stream(url: &str, p: &MediaProps, id: NodeId) -> gtk4::MediaFile {
    let media = media_file(url);
    media.set_muted(p.muted);
    media.set_volume(p.volume.clamp(0.0, 1.0));
    media.set_loop(p.looping);
    observe(media.upcast_ref(), id);
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
                observe(media.upcast_ref(), id);
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

// glib is what the widget-data helpers above are generic over; naming it lets a reader trace
// `set_data`/`data` to their trait.
#[allow(unused_imports)]
use glib::object::ObjectExt as _;

day_pieces::renderer!(day_gtk::RENDERERS, Gtk,
    kind: KIND, props: MediaProps, patch: MediaPatch,
    make: make, update: update, measure: measure, release: release);
