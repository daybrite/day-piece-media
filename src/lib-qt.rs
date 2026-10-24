// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// Qt: this crate's shim (src/lib-qt-shim.cpp) wrapping QMediaPlayer + QAudioOutput (+ a
// QVideoWidget for pictures) behind a flat C ABI. build.rs compiles it and links
// Qt6MultimediaWidgets (which day-qt-sys does not); where that module is absent the shim degrades
// to a URL label (see the shim's #else). QVideoWidget has no built-in chrome, so `.controls` is a
// no-op on Qt; drive playback with the `.play()/.pause()` triggers. Playback state comes back
// through one file-static C callback the shim calls from the player's own signals. What is on
// goes to the desktop through MPRIS on the session bus (src/mpris.rs, Linux only), whose
// commands come back as `REMOTE` reports.
// ---------------------------------------------------------------------------

use super::*;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

use day_qt::{Qt, QtHandle};
use day_spec::{NodeId, Proposal, Size};

unsafe extern "C" {
    fn day_media_new(
        id: u64,
        url: *const c_char,
        autoplay: c_int,
        looping: c_int,
        muted: c_int,
        audio_only: c_int,
        volume: f64,
    ) -> *mut c_void;
    fn day_media_set_state_cb(cb: extern "C" fn(u64, c_int, *const c_char));
    fn day_media_load(w: *mut c_void, url: *const c_char);
    fn day_media_play(w: *mut c_void);
    fn day_media_pause(w: *mut c_void);
    fn day_media_stop(w: *mut c_void);
    fn day_media_seek(w: *mut c_void, seconds: f64);
    fn day_media_rate(w: *mut c_void, rate: f64);
    fn day_media_poll(w: *mut c_void);
    fn day_media_set_volume(w: *mut c_void, volume: f64);
    fn day_media_is_audio_only(w: *mut c_void) -> c_int;
}

/// One state report from the shim: the code is the piece's own, the text an error's message.
extern "C" fn on_state(id: u64, code: c_int, text: *const c_char) {
    let text = if text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    };
    day_qt::emit(
        NodeId(id),
        Event::Custom {
            tag: report::TAG,
            num: code as f64,
            text,
        },
    );
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

fn make(_backend: &mut Qt, p: &MediaProps, id: NodeId) -> QtHandle {
    // The state callback is a single file-static in the shim, shared by every player (a report
    // carries its own node id), so register it once rather than per view.
    static STATE_CB: std::sync::Once = std::sync::Once::new();
    STATE_CB.call_once(|| unsafe { day_media_set_state_cb(on_state) });
    let handle = QtHandle(unsafe {
        day_media_new(
            id.0,
            cstr(&p.url).as_ptr(),
            p.autoplay as c_int,
            p.looping as c_int,
            p.muted as c_int,
            p.audio_only as c_int,
            p.volume.clamp(0.0, 1.0),
        )
    });
    nodes::remember(&handle, id);
    handle
}

/// The node behind each shim handle: `update` gets only the handle, and the MPRIS service
/// reports a client's commands to a node. Linux only, like the service.
#[cfg(target_os = "linux")]
mod nodes {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static NODES: RefCell<HashMap<usize, NodeId>> = RefCell::new(HashMap::new());
    }

    pub(super) fn remember(h: &QtHandle, id: NodeId) {
        NODES.with(|n| n.borrow_mut().insert(h.0 as usize, id));
    }

    pub(super) fn forget(h: &QtHandle) {
        NODES.with(|n| n.borrow_mut().remove(&(h.0 as usize)));
    }

    pub(super) fn of(h: &QtHandle) -> Option<NodeId> {
        NODES.with(|n| n.borrow().get(&(h.0 as usize)).copied())
    }
}

#[cfg(not(target_os = "linux"))]
mod nodes {
    use super::*;

    pub(super) fn remember(_h: &QtHandle, _id: NodeId) {}
    pub(super) fn forget(_h: &QtHandle) {}
}

/// Linux's now-playing surface is MPRIS (src/mpris.rs): the shell's media controls and
/// `playerctl` read it, and their commands come back as `REMOTE` reports on this node. macOS
/// has no session bus by default, so the macos-qt build publishes nothing.
#[cfg(target_os = "linux")]
fn now_playing(h: &QtHandle, info: Option<&NowPlayingInfo>) {
    if let Some(id) = nodes::of(h) {
        crate::mpris::install(|node, text| {
            day_qt::emit(
                node,
                Event::Custom {
                    tag: report::TAG,
                    num: report::REMOTE as f64,
                    text,
                },
            )
        });
        crate::mpris::publish(id, info.cloned());
    }
}

#[cfg(not(target_os = "linux"))]
fn now_playing(_h: &QtHandle, _info: Option<&NowPlayingInfo>) {}

fn update(_backend: &mut Qt, h: &QtHandle, patch: &MediaPatch) {
    unsafe {
        match patch {
            MediaPatch::Load(url) => day_media_load(h.0, cstr(url).as_ptr()),
            MediaPatch::Seek(value) => day_media_seek(h.0, *value),
            MediaPatch::Rate(value) => day_media_rate(h.0, *value),
            MediaPatch::Background(_) => {}
            MediaPatch::NowPlaying(info) => now_playing(h, info.as_ref()),
            MediaPatch::Poll => day_media_poll(h.0),
            MediaPatch::Play => day_media_play(h.0),
            MediaPatch::Pause => day_media_pause(h.0),
            MediaPatch::Stop => day_media_stop(h.0),
            MediaPatch::Volume(v) => day_media_set_volume(h.0, v.clamp(0.0, 1.0)),
        }
    }
}

/// A sound-only player takes no room; a video fills what it is offered.
fn measure(backend: &mut Qt, h: &QtHandle, proposal: Proposal) -> Size {
    if unsafe { day_media_is_audio_only(h.0) } != 0 {
        return Size::ZERO;
    }
    day_pieces::fill_measure(backend, h, proposal)
}

/// Forget the handle's node; day-qt frees the widget itself.
fn release(_backend: &mut Qt, h: &QtHandle) {
    nodes::forget(h);
}

day_pieces::renderer!(day_qt::RENDERERS, Qt,
    kind: KIND, props: MediaProps, patch: MediaPatch,
    make: make, update: update, measure: measure, release: release);
