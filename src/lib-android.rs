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
use day_android::DayEnv;
use day_android::jni::objects::JValue;
use day_android::{AHandle, Android, with_env};
use day_spec::{NodeId, Proposal, Size};

/// This piece's Java class (src/DayMedia.java, on the app classpath at build).
const MEDIA_CLASS: &str = "dev/daybrite/day/piece/media/DayMedia";

fn make(_backend: &mut Android, p: &MediaProps, id: NodeId) -> AHandle {
    make_player(p, id)
}
fn make_player(p: &MediaProps, id: NodeId) -> AHandle {
    with_env(|env| {
        let url = env.new_string(&p.url).expect("url");
        let view = env
            .dcall_static(
                MEDIA_CLASS,
                "makeMedia",
                "(JLjava/lang/String;ZZZZZD)Landroid/view/View;",
                &[
                    JValue::Long(id.0 as i64),
                    JValue::Object(&url),
                    JValue::Bool(p.autoplay),
                    JValue::Bool(p.looping),
                    JValue::Bool(p.muted),
                    JValue::Bool(p.controls),
                    JValue::Bool(p.audio_only),
                    JValue::Double(p.volume.clamp(0.0, 1.0)),
                ],
            )
            .expect("DayMedia.makeMedia")
            .l()
            .expect("View");
        AHandle(std::sync::Arc::new(
            env.new_global_ref(view).expect("global ref"),
        ))
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
    with_env(|env| {
        let s = env.new_string(url).expect("cmd url");
        let _ = env.dcall_static(
            MEDIA_CLASS,
            "mediaCommand",
            "(Landroid/view/View;ILjava/lang/String;D)V",
            &[
                JValue::Object(h.0.as_obj()),
                JValue::Int(code),
                JValue::Object(&s),
                JValue::Double(value),
            ],
        );
    });
}

/// A sound-only player takes no room; a video fills what it is offered.
fn measure(backend: &mut Android, h: &AHandle, proposal: Proposal) -> Size {
    let audio_only = with_env(|env| {
        env.dcall_static(
            MEDIA_CLASS,
            "isAudioOnly",
            "(Landroid/view/View;)Z",
            &[JValue::Object(h.0.as_obj())],
        )
        .and_then(|v| v.z())
        .unwrap_or(false)
    });
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
    with_env(|env| {
        let _ = env.dcall_static(
            MEDIA_CLASS,
            "releaseMedia",
            "(Landroid/view/View;)V",
            &[JValue::Object(h.0.as_obj())],
        );
    });
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
        with_env(|env| {
            let object = env
                .dcall_static(
                    MEDIA_CLASS,
                    "sessionReports",
                    "(Landroid/view/View;)Ljava/lang/String;",
                    &[JValue::Object(self.0.0.as_obj())],
                )
                .ok()
                .and_then(|v| v.l().ok());
            let Some(object) = object else {
                return Vec::new();
            };
            let text = env
                .dstr(&day_android::as_jstring(object))
                .unwrap_or_default();
            serde_json::from_str(&text).unwrap_or_default()
        })
    }
}
impl Drop for SessionPlayer {
    fn drop(&mut self) {
        release_player(&self.0);
    }
}
