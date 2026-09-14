// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! A bundled clip, transport controls, volume, and native playback-state readback.
use day::prelude::*;
use day_piece_media::{PlaybackState, media};

day::resources!();
day::day_start!(options: window(), root);

pub fn window() -> day::WindowOptions {
    day::WindowOptions {
        locales: Some((res::locales::DEFAULT, res::locales::CATALOG)),
        title_fn: Some(|| res::str::app_title().format()),
        size: Size::new(800.0, 640.0),
        ..Default::default()
    }
}

fn sample_url() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        "assets/data/sample.mp4".to_string()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Native engines need a filesystem URL. Keep the fixture in the app's sandbox,
        // including on Android where packaged assets are not ordinary files.
        let directory = day_part_fs::data_dir().expect("app data directory");
        std::fs::create_dir_all(&directory).expect("create demo data directory");
        let path = directory.join("media-demo-sample.mp4");
        std::fs::write(&path, include_bytes!("../resource/assets/sample.mp4"))
            .expect("write bundled clip");
        format!("file://{}", path.to_string_lossy())
    }
}

fn status(state: PlaybackState) -> String {
    match state {
        PlaybackState::Idle => res::str::state_idle().format(),
        PlaybackState::Loading => res::str::state_loading().format(),
        PlaybackState::Playing => res::str::state_playing().format(),
        PlaybackState::Paused => res::str::state_paused().format(),
        PlaybackState::Ended => res::str::state_ended().format(),
        PlaybackState::Error(error) => res::str::state_error(error).format(),
    }
}

pub fn root() -> impl Piece {
    let url = sample_url();
    let state = Signal::new(PlaybackState::Idle);
    let volume = Signal::new(0.0_f64);
    let play = Trigger::new();
    let pause = Trigger::new();
    let stop = Trigger::new();
    let reload = Trigger::new();
    column((
        label(res::str::app_title())
            .font(Font::Title)
            .id("media-title"),
        label(res::str::caption()).font(Font::Footnote),
        media(url)
            .autoplay(false)
            .looping(true)
            .volume(volume)
            .play(play)
            .pause(pause)
            .stop(stop)
            .load(reload)
            .state(state)
            .id("media-player")
            .height(200.0),
        label(move || status(state.get())).id("media-state"),
        row((
            button(res::str::play())
                .action(move || play.notify())
                .id("media-play"),
            button(res::str::pause())
                .action(move || pause.notify())
                .id("media-pause"),
        ))
        .spacing(8.0),
        row((
            button(res::str::stop())
                .action(move || stop.notify())
                .id("media-stop"),
            button(res::str::reload())
                .action(move || reload.notify())
                .id("media-reload"),
        ))
        .spacing(8.0),
        labeled(res::str::volume(), slider(volume).id("media-volume")),
        label(res::str::volume_hint()).font(Font::Footnote),
    ))
    .spacing(12.0)
    .padding(20.0)
}
