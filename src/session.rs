// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! A headless native player owned by a Rust handle, independent of window/page trees.
use super::*;
use day_reactive::{Scope, Signal};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// Android foreground-notification presentation. Call on the UI thread with generated,
/// localized app strings. iOS requires the application's audio background mode as well.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct BackgroundAudio {
    pub title: String,
    pub description: String,
    pub channel_name: String,
}

/// A coherent application snapshot; `generation` changes on load/stop, `sequence` on observations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaybackSnapshot {
    pub generation: u64,
    pub sequence: u64,
    pub state: PlaybackState,
    pub timeline: MediaTimeline,
    pub metadata: Option<StreamMetadata>,
    pub error: Option<MediaError>,
}
#[cfg(not(all(feature = "mdc", target_os = "android")))]
struct PlayerNode {
    node: RNode,
    scope: Scope,
}
#[cfg(not(all(feature = "mdc", target_os = "android")))]
impl Drop for PlayerNode {
    fn drop(&mut self) {
        day_core::try_with_tree(|t| {
            if t.node_exists(self.node) {
                t.patch(self.node, Box::new(MediaPatch::Stop), false);
                t.remove_subtree(self.node);
            }
        });
        self.scope.dispose();
    }
}
#[cfg(all(feature = "mdc", target_os = "android"))]
struct PlayerNode {
    player: crate::android_impl::SessionPlayer,
    _probe: Rc<icy::Probe>,
    scope: Scope,
}
#[cfg(all(feature = "mdc", target_os = "android"))]
impl Drop for PlayerNode {
    fn drop(&mut self) {
        self.scope.dispose();
    }
}

struct Inner {
    scope: Scope,
    node: RefCell<Option<PlayerNode>>,
    snapshot: Signal<PlaybackSnapshot>,
    generation: Cell<u64>,
    volume: Cell<f64>,
    rate: Cell<f64>,
    background: RefCell<Option<BackgroundAudio>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.node.get_mut().take();
        self.scope.dispose();
    }
}
/// Cloneable UI-thread handle. Retain in application state to outlive every window.
/// Last-handle drop stops playback and releases the player. No hidden global player is installed.
/// Requires a running Day host; browser lifetime is limited to the document.
#[derive(Clone)]
pub struct AudioSession(Rc<Inner>);
impl Default for AudioSession {
    fn default() -> Self {
        Self::new()
    }
}
impl AudioSession {
    pub fn new() -> Self {
        let scope = Scope::detached();
        let snapshot = scope.enter(|| Signal::new(PlaybackSnapshot::default()));
        Self(Rc::new(Inner {
            scope,
            node: RefCell::new(None),
            snapshot,
            generation: Cell::new(0),
            volume: Cell::new(1.0),
            rate: Cell::new(1.0),
            background: RefCell::new(None),
        }))
    }
    /// Signal remains valid while at least one session handle is retained.
    pub fn snapshot(&self) -> Signal<PlaybackSnapshot> {
        self.0.snapshot
    }
    /// Load a URL, file path, or platform content URI. Each load has a fresh event target;
    /// delayed callbacks from the previous source can never finish the new queue item.
    pub fn load(&self, source: impl Into<String>, autoplay: bool) -> Result<u64, MediaError> {
        let source = source.into();
        if source.trim().is_empty() || source.contains('\0') {
            return Err(MediaError::invalid("load"));
        }
        self.stop();
        let generation = self.0.generation.get();
        let sink = self.0.snapshot;
        let scope = self.0.scope.create_child();
        #[cfg(not(all(feature = "mdc", target_os = "android")))]
        let node = scope.enter(|| {
            let state = Signal::new(PlaybackState::Loading);
            let timeline = Signal::new(MediaTimeline::default());
            let metadata = Signal::new(None);
            let error = Signal::new(None);
            let rate = Signal::new(self.0.rate.get());
            let piece = media(source)
                .audio_only()
                .volume(self.0.volume.get())
                .state(state)
                .timeline(timeline)
                .metadata(metadata)
                .error(error)
                .rate(rate);
            let piece = if autoplay { piece } else { piece.no_autoplay() };
            let node = piece.mount(|props| {
                with_tree(|t| {
                    t.create_node(
                        KIND,
                        props,
                        Rc::new(day_core::LeafLayout),
                        Flex::default(),
                        true,
                        false,
                        scope,
                    )
                })
            });
            sink.set(PlaybackSnapshot {
                generation,
                state: state.get_untracked(),
                ..Default::default()
            });
            watch(
                move || (state.get(), timeline.get(), metadata.get(), error.get()),
                move |value, _| {
                    let previous = sink.get_untracked();
                    if previous.generation != generation {
                        return;
                    }
                    sink.set(PlaybackSnapshot {
                        generation,
                        sequence: previous.sequence + 1,
                        state: value.0.clone(),
                        timeline: value.1.clone(),
                        metadata: value.2.clone(),
                        error: value.3.clone(),
                    });
                },
            );
            node
        });
        #[cfg(not(all(feature = "mdc", target_os = "android")))]
        {
            *self.0.node.borrow_mut() = Some(PlayerNode { node, scope });
        }
        #[cfg(all(feature = "mdc", target_os = "android"))]
        {
            let (player, probe) = scope.enter(|| {
                let metadata = Signal::new(None);
                let probe = icy::Probe::new(AnyBinding::new(metadata));
                probe.start(source.clone());
                watch(
                    move || metadata.get(),
                    move |value, _| {
                        let mut snapshot = sink.get_untracked();
                        if snapshot.generation == generation {
                            snapshot.metadata = value.clone();
                            snapshot.sequence += 1;
                            sink.set(snapshot);
                        }
                    },
                );
                let player = crate::android_impl::SessionPlayer::new(&MediaProps {
                    url: source,
                    autoplay,
                    audio_only: true,
                    volume: self.0.volume.get(),
                    ..Default::default()
                });
                player.command(&MediaPatch::Rate(self.0.rate.get()));
                (player, probe)
            });
            sink.set(PlaybackSnapshot {
                generation,
                state: PlaybackState::Loading,
                ..Default::default()
            });
            *self.0.node.borrow_mut() = Some(PlayerNode {
                player,
                _probe: probe,
                scope,
            });
            android_observation::start(&self.0);
        }
        if let Some(background) = self.0.background.borrow().clone() {
            self.command(MediaPatch::Background(background));
        }
        // Detached HTML audio is not inserted into the document, so autoplay attribute alone
        // does not start it in every browser. Keep the explicit play in the user-action turn.
        #[cfg(feature = "dom")]
        if autoplay {
            self.command(MediaPatch::Play);
        }
        Ok(generation)
    }
    fn command(&self, patch: MediaPatch) {
        #[cfg(not(all(feature = "mdc", target_os = "android")))]
        {
            let node = self.0.node.borrow().as_ref().map(|n| n.node);
            if let Some(node) = node {
                with_tree(|t| t.patch(node, Box::new(patch), false));
            }
        }
        #[cfg(all(feature = "mdc", target_os = "android"))]
        if let Some(node) = self.0.node.borrow().as_ref() {
            node.player.command(&patch);
        }
    }
    /// Enable platform background playback for subsequent/current loads. On Android this
    /// supplies the foreground-service notification; configure before loading from a user action.
    pub fn background(&self, presentation: BackgroundAudio) {
        *self.0.background.borrow_mut() = Some(presentation.clone());
        self.command(MediaPatch::Background(presentation));
    }
    pub fn play(&self) {
        self.command(MediaPatch::Play);
    }
    pub fn pause(&self) {
        self.command(MediaPatch::Pause);
    }
    pub fn stop(&self) {
        let old = self.0.node.borrow_mut().take();
        self.0
            .generation
            .set(self.0.generation.get().wrapping_add(1));
        self.0.snapshot.set(PlaybackSnapshot {
            generation: self.0.generation.get(),
            ..Default::default()
        });
        drop(old);
    }
    pub fn set_volume(&self, volume: f64) -> Result<(), MediaError> {
        if !volume.is_finite() {
            return Err(MediaError::invalid("volume"));
        }
        let volume = volume.clamp(0.0, 1.0);
        self.0.volume.set(volume);
        self.command(MediaPatch::Volume(volume));
        Ok(())
    }
    pub fn set_rate(&self, rate: f64) -> Result<(), MediaError> {
        if !valid_rate(rate) {
            return Err(MediaError::invalid("rate"));
        }
        self.0.rate.set(rate);
        self.command(MediaPatch::Rate(rate));
        Ok(())
    }
    /// Only seeks a loaded source with a reported seek range. Completion is asynchronous.
    pub fn seek(&self, seconds: f64) -> Result<(), MediaError> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(MediaError::invalid("seek"));
        }
        let Some(target) = self
            .snapshot()
            .get_untracked()
            .timeline
            .seek_target(seconds)
        else {
            return Err(MediaError::command("seek:unsupported"));
        };
        self.command(MediaPatch::Seek(target));
        Ok(())
    }
}

#[cfg(all(feature = "mdc", target_os = "android"))]
mod android_observation {
    use super::*;
    use std::{collections::BTreeMap, rc::Weak};
    day_core::tls_group! {
        static SESSIONS: RefCell<BTreeMap<u64, Weak<Inner>>> = RefCell::new(BTreeMap::new());
        static NEXT: Cell<u64> = Cell::new(0);
    }
    pub(super) fn start(inner: &Rc<Inner>) {
        let id = NEXT.with(|n| {
            let id = n.get() + 1;
            n.set(id);
            id
        });
        SESSIONS.with(|sessions| sessions.borrow_mut().insert(id, Rc::downgrade(inner)));
        tick(id, inner.generation.get());
    }
    fn tick(id: u64, generation: u64) {
        day_reactive::on_main_delayed(250, move || {
            let inner =
                SESSIONS.with(|sessions| sessions.borrow().get(&id).and_then(Weak::upgrade));
            let Some(inner) = inner.filter(|inner| inner.generation.get() == generation) else {
                SESSIONS.with(|sessions| sessions.borrow_mut().remove(&id));
                return;
            };
            let reports = inner
                .node
                .borrow()
                .as_ref()
                .map(|node| node.player.reports())
                .unwrap_or_default();
            let mut snapshot = inner.snapshot.get_untracked();
            for (code, text) in reports {
                match code {
                    report::TIMELINE => {
                        if let Some(value) = MediaTimeline::decode(&text) {
                            snapshot.timeline = value;
                        }
                    }
                    report::COMMAND_ERROR => snapshot.error = Some(MediaError::command(&text)),
                    report::METADATA => {
                        snapshot.metadata = Some(StreamMetadata::from_report(&text))
                    }
                    _ => {
                        snapshot.state = PlaybackState::from_report(code as f64, &text);
                        if code == report::ERROR {
                            snapshot.error = Some(MediaError {
                                kind: MediaErrorKind::Playback,
                                operation: "playback".into(),
                                detail: text,
                            });
                        }
                    }
                }
            }
            snapshot.sequence += 1;
            inner.snapshot.set(snapshot);
            tick(id, generation);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use day_mock::MockToolkit;
    #[test]
    fn ownership_source_generations_and_stale_events() {
        day_core::uninstall_tree();
        let (mock, probe) = MockToolkit::new();
        let slot = Rc::new(RefCell::new(None));
        let out = slot.clone();
        day_core::launch_with(mock, day_spec::WindowOptions::default(), move || {
            let session = AudioSession::new();
            session.load("fixture:first.wav", false).unwrap();
            *out.borrow_mut() = Some(session);
            day_core::AnyPiece::new(media("fixture:view.mp4").no_autoplay())
        });
        let session = slot.borrow_mut().take().unwrap();
        let snapshot = session.snapshot();
        let ids = probe.find_by_kind(KIND);
        assert_eq!(ids.len(), 2);
        let first = day_core::rnode_to_id(session.0.node.borrow().as_ref().unwrap().node);
        // Removing the visible piece cannot remove the detached session node.
        for (_, widget) in ids {
            let id = day_spec::NodeId(widget.node);
            if id != first {
                with_tree(|t| t.remove_subtree(day_core::id_to_rnode(id)));
            }
        }
        assert!(with_tree(|t| t.node_exists(day_core::id_to_rnode(first))));
        let old_generation = snapshot.get_untracked().generation;
        let next_generation = session.load("fixture:second.wav", false).unwrap();
        assert!(next_generation > old_generation);
        probe.emit(
            first,
            Event::Custom {
                tag: report::TAG,
                num: report::ENDED as f64,
                text: String::new(),
            },
        );
        day_reactive::flush_sync();
        assert_ne!(snapshot.get_untracked().state, PlaybackState::Ended);
        let second = day_core::rnode_to_id(session.0.node.borrow().as_ref().unwrap().node);
        probe.emit(
            second,
            Event::Custom {
                tag: report::TAG,
                num: report::PAUSED as f64,
                text: String::new(),
            },
        );
        day_reactive::flush_sync();
        assert_eq!(snapshot.get_untracked().state, PlaybackState::Paused);
        let clone = session.clone();
        drop(session);
        assert!(with_tree(|t| t.node_exists(day_core::id_to_rnode(second))));
        drop(clone);
        assert!(!with_tree(|t| t.node_exists(day_core::id_to_rnode(second))));
    }
}
