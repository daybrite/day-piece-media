// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// Web (web-dom): a plain `<video>` element, or a hidden `<audio>` for a sound-only player.
//
// The one backend where a media player is less work than the native ones: the browser supplies the
// transport chrome, buffering, scrubbing, fullscreen, captions and picture-in-picture, and every
// MediaProps field maps onto an attribute of the same meaning. Playback state comes back through
// this crate's browser bridge, which turns the element's `playing`/`pause`/
// `waiting`/`ended`/`error` events into the piece's report codes on the Custom channel; the
// bridge also owns the two things an attribute cannot express, muting (a property) and stopping
// (removing `src`), see browser.rs.
//
// Two web-only truths, both documented in docs/media.md:
//
// - A file path will not load. The other backends accept `/Users/me/clip.mp4`; a page can only
//   fetch a URL, and a cross-origin one needs CORS. Use a same-origin file under the app's `dist/`
//   or a permissive remote.
// - Autoplay with sound is blocked by every modern browser unless the user has interacted with
//   the page. `.muted(true)` is what makes autoplay start at all; otherwise the element loads
//   and waits, which looks like a broken player but is the browser's policy, not day's. A radio app
//   starts its stream from a tap, which is the interaction the policy wants.
// ---------------------------------------------------------------------------

use super::*;
use std::cell::RefCell;
use std::collections::HashSet;

use day_dom::{Dom, DomHandle};
use day_spec::{NodeId, Proposal, Size};

day_core::tls_group! {
    // The sound-only elements, which measure zero (a DomHandle is an element id).
    static AUDIO_ONLY: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
}

fn make(backend: &mut Dom, p: &MediaProps, _id: NodeId) -> DomHandle {
    let h = backend.element(if p.audio_only { "audio" } else { "video" });
    if !p.url.is_empty() {
        backend.set_attr(&h, "src", &p.url);
    }
    // A boolean attribute is set only when it is on. day-dom's `""`-removes convention covers
    // `disabled` and `readonly` alone; for any other name `""` is a PRESENT attribute, and
    // `autoplay=""` means autoplay. Nothing here changes after creation (MediaPatch carries no
    // flag), so there is never anything to remove. `muted` is not an attribute at all: one set by
    // a script never mutes, the property does (browser.rs).
    for (name, on) in [
        ("controls", p.controls && !p.audio_only),
        ("autoplay", p.autoplay),
        ("loop", p.looping),
        // `playsinline` keeps iOS Safari from hijacking the whole screen for playback; without
        // it a small inline player becomes a fullscreen takeover on iPhone.
        ("playsinline", true),
    ] {
        if on {
            backend.set_attr(&h, name, "-");
        }
    }
    super::browser::attach(h.0 as i32, p.volume, p.muted);
    if p.audio_only {
        backend.set_attr(&h, "style", "display:none");
        AUDIO_ONLY.with(|s| s.borrow_mut().insert(h.0));
    } else {
        // Fill the frame day's layout assigns, the same growing-leaf contract as the native arms.
        backend.set_attr(&h, "style", "width:100%;height:100%;object-fit:contain");
    }
    h
}

fn update(backend: &mut Dom, h: &DomHandle, patch: &MediaPatch) {
    match patch {
        MediaPatch::Load(url) => {
            backend.set_attr(h, "src", url);
            // `load()` re-reads the new src; `play()` then starts it, matching `.load()`'s
            // documented "reload and play" behavior on the native arms.
            backend.call(h, "load");
            super::browser::play(h.0 as i32);
        }
        MediaPatch::Seek(value) => super::browser::seek(h.0 as i32, *value),
        MediaPatch::Rate(value) => super::browser::rate(h.0 as i32, *value),
        MediaPatch::Background(_) => {}
        MediaPatch::Poll => super::browser::poll(h.0 as i32),
        MediaPatch::Play => super::browser::play(h.0 as i32),
        MediaPatch::Pause => backend.call(h, "pause"),
        // Removing the source and re-loading is what makes the element let its connection go
        // (a paused live stream keeps buffering); the `emptied` event reports the Idle. It goes
        // through the bridge because `src` must be REMOVED: day-dom's `set_attr` cannot remove
        // it, and an empty `src` is a format error, not an absence (browser.rs).
        MediaPatch::Stop => super::browser::stop(h.0 as i32),
        MediaPatch::Volume(v) => super::browser::volume(h.0 as i32, *v),
    }
}

/// A growing leaf: take whatever the layout proposes, with a modest default when it proposes
/// nothing (the same posture as the other arms, which report the native view's intrinsic size).
/// A sound-only element is `display:none` and takes no room.
fn measure(_backend: &mut Dom, h: &DomHandle, p: Proposal) -> Size {
    if AUDIO_ONLY.with(|s| s.borrow().contains(&h.0)) {
        return Size::ZERO;
    }
    Size::new(p.width.unwrap_or(320.0), p.height.unwrap_or(180.0))
}

// Defines `register()`, which `media()` calls: web-dom's registry is populated at runtime, unlike
// the link-time `renderer!` the other eight arms use.
day_pieces::dom_renderer!(day_dom::register_renderer, Dom,
    kind: KIND, props: MediaProps, patch: MediaPatch,
    make: make, update: update, measure: measure, release: release);

fn release(_backend: &mut Dom, h: &DomHandle) {
    AUDIO_ONLY.with(|s| s.borrow_mut().remove(&h.0));
}
