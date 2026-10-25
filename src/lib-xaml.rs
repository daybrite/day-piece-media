// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// XAML: this crate's shim (src/lib-xaml-shim.cpp) wrapping a Windows.UI.Xaml.Controls
// MediaPlayerElement, boxed into a day handle via day-toolkit-xaml-sys's `day_xaml_box`/`day_xaml_unbox`
// functions (like the picker/webview xaml pieces). MediaPlayerElement is core system XAML: no
// availability caveat like the EdgeHTML WebView. `.controls` maps to AreTransportControlsEnabled;
// looping/muted/autoplay/volume live on the backing MediaPlayer. A sound-only player is a
// collapsed element of no size. Playback state comes back through one file-static C callback the
// shim calls from the player's PlaybackSession, on a worker thread; each report is posted to the
// UI thread before it becomes an event (see `on_state`). Windows-only; built + verified in CI.
// What is on goes to the System Media Transport Controls through the shim, whose button and
// seek events come back as `REMOTE` reports on the same path.
// ---------------------------------------------------------------------------

use super::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

use day_spec::{NodeId, Platform, Proposal, Size};
use day_toolkit_winui::{WinHandle, Xaml};

unsafe extern "C" {
    fn day_media_xaml_new(
        id: u64,
        url: *const c_char,
        autoplay: c_int,
        looping: c_int,
        muted: c_int,
        controls: c_int,
        audio_only: c_int,
        volume: f64,
    ) -> *mut c_void;
    fn day_media_xaml_set_state_cb(cb: extern "C" fn(u64, c_int, *const c_char));
    fn day_media_xaml_load(w: *mut c_void, url: *const c_char);
    fn day_media_xaml_play(w: *mut c_void);
    fn day_media_xaml_pause(w: *mut c_void);
    fn day_media_xaml_stop(w: *mut c_void);
    fn day_media_xaml_seek(w: *mut c_void, seconds: f64);
    fn day_media_xaml_rate(w: *mut c_void, rate: f64);
    fn day_media_xaml_release(w: *mut c_void);
    fn day_media_xaml_poll(w: *mut c_void);
    fn day_media_xaml_set_volume(w: *mut c_void, volume: f64);
    fn day_media_xaml_is_audio_only(w: *mut c_void) -> c_int;
    fn day_media_xaml_now_playing(
        w: *mut c_void,
        present: c_int,
        title: *const c_char,
        artist: *const c_char,
        album: *const c_char,
        artwork_path: *const c_char,
        artwork_url: *const c_char,
        duration: f64,
        position: f64,
        rate: f64,
        playing: c_int,
        live: c_int,
        has_next: c_int,
        has_previous: c_int,
    );
}

/// One state report from the shim: the code is the piece's own, the text an error's message.
///
/// MediaPlayer raises its events on a worker thread, and day takes events only on the UI thread:
/// day-toolkit-winui's event sink is thread-local, so an emit made on the worker reached no sink and was
/// dropped (the video played while the demo's state label still read "Idle"). Every report rides
/// `Platform::post` home instead, which also queues the shim's own UI-thread reports (Stop's
/// idle) behind the worker's, in the order they were made.
extern "C" fn on_state(id: u64, code: c_int, text: *const c_char) {
    let text = if text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    };
    <Xaml as Platform>::post(Box::new(move || {
        day_toolkit_winui::emit(
            NodeId(id),
            Event::Custom {
                tag: report::TAG,
                num: code as f64,
                text,
            },
        );
    }));
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

fn make(_backend: &mut Xaml, p: &MediaProps, id: NodeId) -> WinHandle {
    static STATE_CB: std::sync::Once = std::sync::Once::new();
    STATE_CB.call_once(|| unsafe { day_media_xaml_set_state_cb(on_state) });
    WinHandle(unsafe {
        day_media_xaml_new(
            id.0,
            cstr(&p.url).as_ptr(),
            p.autoplay as c_int,
            p.looping as c_int,
            p.muted as c_int,
            p.controls as c_int,
            p.audio_only as c_int,
            p.volume.clamp(0.0, 1.0),
        )
    })
}

fn update(_backend: &mut Xaml, h: &WinHandle, patch: &MediaPatch) {
    unsafe {
        match patch {
            MediaPatch::Load(url) => day_media_xaml_load(h.0, cstr(url).as_ptr()),
            MediaPatch::Seek(value) => day_media_xaml_seek(h.0, *value),
            MediaPatch::Rate(value) => day_media_xaml_rate(h.0, *value),
            MediaPatch::Background(_) => {}
            MediaPatch::NowPlaying(info) => now_playing(h, info.as_ref()),
            MediaPatch::Poll => day_media_xaml_poll(h.0),
            MediaPatch::Play => day_media_xaml_play(h.0),
            MediaPatch::Pause => day_media_xaml_pause(h.0),
            MediaPatch::Stop => day_media_xaml_stop(h.0),
            MediaPatch::Volume(v) => day_media_xaml_set_volume(h.0, v.clamp(0.0, 1.0)),
        }
    }
}

/// Windows' now-playing surface is the System Media Transport Controls (the media keys' overlay,
/// the volume flyout's card), which the shim fills from the player's own controls. The fields
/// cross as plain C arguments, the shim having no JSON reader; `-1` is no duration, `present`
/// 0 clears the card.
fn now_playing(h: &WinHandle, info: Option<&NowPlayingInfo>) {
    let i = info.cloned().unwrap_or_default();
    let (title, artist, album) = (cstr(&i.title), cstr(&i.artist), cstr(&i.album));
    let artwork_path = cstr(i.artwork_path.as_deref().unwrap_or(""));
    let artwork_url = cstr(i.artwork_url.as_deref().unwrap_or(""));
    let duration = i
        .duration
        .filter(|d| d.is_finite() && *d > 0.0)
        .unwrap_or(-1.0);
    unsafe {
        day_media_xaml_now_playing(
            h.0,
            info.is_some() as c_int,
            title.as_ptr(),
            artist.as_ptr(),
            album.as_ptr(),
            artwork_path.as_ptr(),
            artwork_url.as_ptr(),
            duration,
            i.position,
            i.rate,
            i.playing as c_int,
            i.live as c_int,
            i.has_next as c_int,
            i.has_previous as c_int,
        );
    }
}

/// A sound-only player takes no room; a video fills what it is offered.
fn measure(backend: &mut Xaml, h: &WinHandle, proposal: Proposal) -> Size {
    if unsafe { day_media_xaml_is_audio_only(h.0) } != 0 {
        return Size::ZERO;
    }
    day_pieces::fill_measure(backend, h, proposal)
}

day_pieces::renderer!(day_toolkit_winui::RENDERERS, Xaml,
    kind: KIND, props: MediaProps, patch: MediaPatch,
    make: make, update: update, measure: measure, release: release);

fn release(_: &mut Xaml, h: &WinHandle) {
    unsafe { day_media_xaml_release(h.0) };
}
