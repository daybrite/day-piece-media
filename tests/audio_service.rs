// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

use day_core::{AnyPiece, ApplicationService};
use day_mock::MockToolkit;
use day_piece_media::{KIND, MediaPatch, PlaybackState, media};
use day_pieces::prelude::*;
use day_spec::{Event, NodeId, WindowKind, WindowOptions};
use std::{cell::RefCell, rc::Rc};

#[test]
fn audio_commands_and_readback_survive_closing_the_initial_window() {
    day_core::uninstall_tree();
    let (mock, probe) = MockToolkit::new();
    probe.describe_patch(|p: &MediaPatch| format!("media {p:?}"));
    let handles = Rc::new(RefCell::new(None));
    let saved = handles.clone();
    day_core::launch_with(mock, WindowOptions::default(), move || {
        // These signals must belong to the application, not to its first window.
        let (state, play) = Scope::root().enter(|| (Signal::new(PlaybackState::Idle), Trigger::new()));
        let audio = media("").autoplay(false).state(state).play(play).start_audio();
        *saved.borrow_mut() = Some((audio, state, play));
        AnyPiece::new(label("First window"))
    });
    let initial = day_core::windows::initial_window().unwrap();
    let _second = day_core::open_window(None, WindowOptions::default(), WindowKind::Normal,
        || label("Second window"));
    let (_, widget) = probe.find_by_kind(KIND).into_iter().next().unwrap();
    initial.close();
    let (audio, state, play) = handles.borrow().as_ref().unwrap().clone();
    assert_eq!(probe.find_by_kind(KIND).len(), 1, "closing the initial window keeps audio alive");
    play.notify();
    assert!(probe.mutations().iter().any(|m| m.contains("media Play")));
    probe.emit(NodeId(widget.node), Event::Custom { tag: "test", num: 2.0, text: String::new() });
    assert_eq!(state.get_untracked(), PlaybackState::Playing);
    audio.shutdown();
    day_core::with_tree(|tree| tree.drain_releases());
    assert!(probe.find_by_kind(KIND).is_empty());
    // Global shutdown remains safe after explicit service shutdown.
    day_core::uninstall_tree();
}
