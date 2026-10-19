// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// Android: android.widget.VideoView + android.widget.MediaController for pictures, and a bare
// android.media.MediaPlayer behind an empty View for sound only. These are framework classes, so
// this piece adds no Gradle dependencies (androidx.media3/ExoPlayer is the later upgrade). The
// Java factory (`dev.daybrite.day.piece.media.DayMedia`) is bundled with this crate in
// `src/DayMedia.java` and pulled into the app's Gradle build via `[package.metadata.day.android]`,
// which also contributes the INTERNET permission for network sources. Playback state comes back
// through DayBridge.nativeOnEvent's open Custom-event kind (12), `num` = the piece's report code.
// ---------------------------------------------------------------------------

use super::*;
use day_android::jni::objects::JString;
use day_android::{AHandle, Android, java, java_string, with_env};
use day_spec::{NodeId, Proposal, Size};

// This piece's Java class (src/DayMedia.java, on the app classpath at build).
day_android::java_class! {
    DayMedia => dev.daybrite.day.piece.media.DayMedia,
    methods {
        static fn makeMedia(id: jlong, url: JString, autoplay: jboolean, looping: jboolean,
            muted: jboolean, controls: jboolean, audio_only: jboolean, volume: jdouble)
            -> android.view.View,
        static fn mediaCommand(view: android.view.View, code: jint, url: JString, value: jdouble),
        static fn isAudioOnly(view: android.view.View) -> jboolean,
        static fn releaseMedia(view: android.view.View),
        static fn sessionReports(view: android.view.View) -> JString,
    },
}

fn make(_backend: &mut Android, p: &MediaProps, id: NodeId) -> AHandle {
    make_player(p, id)
}
fn make_player(p: &MediaProps, id: NodeId) -> AHandle {
    day_android::realize("media", |env| {
        let url = JString::from_str(env, &p.url)?;
        DayMedia::makeMedia(
            env,
            id.0 as i64,
            &url,
            p.autoplay,
            p.looping,
            p.muted,
            p.controls,
            p.audio_only,
            p.volume.clamp(0.0, 1.0),
        )
    })
}

fn update(_backend: &mut Android, h: &AHandle, patch: &MediaPatch) {
    command(h, patch);
}
fn command(h: &AHandle, patch: &MediaPatch) {
    // Commands cross as (code, url, value): 0=load, 1=play, 2=pause, 3=stop, 4=volume.
    let background;
    let (code, url, value) = match patch {
        MediaPatch::Load(u) => (0, u.as_str(), 0.0),
        MediaPatch::Seek(v) => (5, "", *v),
        MediaPatch::Rate(v) => (6, "", *v),
        MediaPatch::Background(value) => {
            background = serde_json::to_string(value).unwrap_or_default();
            (8, background.as_str(), 0.0)
        }
        MediaPatch::Poll => (7, "", 0.0),
        MediaPatch::Play => (1, "", 0.0),
        MediaPatch::Pause => (2, "", 0.0),
        MediaPatch::Stop => (3, "", 0.0),
        MediaPatch::Volume(v) => (4, "", v.clamp(0.0, 1.0)),
    };
    java(|env| {
        let url = JString::from_str(env, url)?;
        DayMedia::mediaCommand(env, h.0.as_obj(), code, &url, value)
    });
}

/// A sound-only player takes no room; a video fills what it is offered.
fn measure(backend: &mut Android, h: &AHandle, proposal: Proposal) -> Size {
    let audio_only = java(|env| DayMedia::isAudioOnly(env, h.0.as_obj())).unwrap_or(false);
    if audio_only {
        return Size::ZERO;
    }
    day_pieces::fill_measure(backend, h, proposal)
}

/// Release the MediaPlayer a sound-only view carries; a VideoView releases its own.
fn release(_backend: &mut Android, h: &AHandle) {
    release_player(h);
}
fn release_player(h: &AHandle) {
    java(|env| DayMedia::releaseMedia(env, h.0.as_obj()));
}

day_pieces::renderer!(day_android::RENDERERS, Android,
    kind: KIND, props: MediaProps, patch: MediaPatch,
    make: make, update: update, measure: measure, release: release);

/// Application-context player retained directly by AudioSession, outside Day's activity tree.
/// Node zero selects a private report queue instead of the UI event router.
pub(crate) struct SessionPlayer(AHandle);
impl SessionPlayer {
    pub(crate) fn new(props: &MediaProps) -> Self {
        Self(make_player(props, NodeId(0)))
    }
    pub(crate) fn command(&self, patch: &MediaPatch) {
        command(&self.0, patch);
    }
    pub(crate) fn reports(&self) -> Vec<(i32, String)> {
        let text = with_env(|env| {
            java_string(env, |env| DayMedia::sessionReports(env, self.0.0.as_obj()))
        })
        .unwrap_or_default();
        serde_json::from_str(&text).unwrap_or_default()
    }
}
impl Drop for SessionPlayer {
    fn drop(&mut self) {
        release_player(&self.0);
    }
}
