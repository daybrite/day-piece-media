// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
//! View-owned video and an application-owned audio session, using synthetic bundled fixtures.
use day::prelude::*;
use day_piece_media::{AudioSession, BackgroundAudio, MediaTimeline, PlaybackState, media};

day::resources!();
day::day_start!(options: window(), root);

pub fn window() -> day::WindowOptions {
    res::window_options().size(860.0, 840.0)
}

fn asset_url(asset: day::prelude::AssetName) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        format!("assets/data/{}", asset.as_str())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let directory = day_part_fs::data_dir().expect("app data directory");
        std::fs::create_dir_all(&directory).expect("create demo data directory");
        let path = directory.join(asset.as_str());
        let bytes = resource(asset).expect("bundled synthetic media fixture");
        std::fs::write(&path, bytes.as_slice()).expect("write bundled clip");
        path.to_string_lossy().into_owned()
    }
}

fn status(state: PlaybackState) -> String {
    match state {
        PlaybackState::Idle => res::str::state_idle().format(),
        PlaybackState::Loading => res::str::state_loading().format(),
        PlaybackState::Playing => res::str::state_playing().format(),
        PlaybackState::Paused => res::str::state_paused().format(),
        PlaybackState::Ended => res::str::state_ended().format(),
        PlaybackState::Error(error) => {
            warn!("media playback: {error}");
            res::str::state_error().format()
        }
    }
}
fn timeline_text(value: MediaTimeline) -> String {
    match value.duration {
        Some(duration) => res::str::timeline(res::str::Timeline {
            duration: duration.round() as i64,
            position: value.position.floor() as i64,
        })
        .format(),
        None => res::str::timeline_unknown().format(),
    }
}

#[derive(Clone)]
struct Demo {
    audio: AudioSession,
    volume: Signal<f64>,
    clip: String,
    tone: String,
}
impl Ambient for Demo {
    fn create() -> Self {
        let audio = AudioSession::new();
        audio.set_volume(0.0).expect("valid volume");
        let volume = Signal::new(0.0);
        let volume_player = audio.clone();
        watch(
            move || volume.get(),
            move |value, _| {
                let _ = volume_player.set_volume(*value);
            },
        );
        audio.background(BackgroundAudio {
            title: res::str::app_title().format(),
            description: res::str::background_description().format(),
            channel_name: res::str::background_channel().format(),
        });
        Self {
            audio,
            volume,
            clip: asset_url(res::assets::sample_mp4),
            tone: asset_url(res::assets::tone_wav),
        }
    }
}

pub fn root() -> impl Piece {
    let _ = Demo::app();
    day::register_new_window(window_content);
    window_content()
}

fn window_content() -> impl Piece {
    let app = Demo::app();
    let snapshot = app.audio.snapshot();
    let audio_page = Signal::new(false);
    scroll(
        column((
            label(res::str::app_title())
                .font(Font::Title)
                .id(day::id!(media_title)),
            row((
                button(res::str::video_example())
                    .action(move || audio_page.set(false))
                    .id(day::id!(show_video)),
                button(res::str::audio_example())
                    .action(move || audio_page.set(true))
                    .id(day::id!(show_audio)),
            ))
            .spacing(8.0),
            button(res::str::switch_appearance())
                .action(|| day::set_appearance(Some(!day::dark_mode())))
                .id(day::id!(switch_appearance)),
            button(res::str::new_window())
                .action(|| {
                    day::open_window(window().key("media-extra"), window_content);
                })
                .id(day::id!(new_window)),
            label(move || status(snapshot.get().state)).id(day::id!(session_state)),
            label(move || timeline_text(snapshot.get().timeline)).id(day::id!(session_timeline)),
            label(move || {
                if snapshot.get().timeline.capabilities.seek {
                    res::str::seek_ready().format()
                } else {
                    res::str::seek_unavailable().format()
                }
            })
            .id(day::id!(session_seek_capability)),
            switch(
                move || audio_page.get(),
                |audio| {
                    if *audio {
                        audio_example().any()
                    } else {
                        video_example().any()
                    }
                },
            ),
        ))
        .spacing(12.0)
        .padding(20.0),
    )
}

fn video_example() -> impl Piece {
    let state = Signal::new(PlaybackState::Idle);
    let timeline = Signal::new(MediaTimeline::default());
    let error = Signal::new(None);
    let volume = Signal::new(0.0_f64);
    let rate = Signal::new(1.0_f64);
    let seek_position = Signal::new(10.0_f64);
    let seek = Trigger::new();
    let play = Trigger::new();
    let pause = Trigger::new();
    let stop = Trigger::new();
    let reload = Trigger::new();
    column((
        label(res::str::caption()).font(Font::Footnote),
        media(Demo::app().clip)
            .no_autoplay()
            .volume(volume)
            .play(play)
            .pause(pause)
            .stop(stop)
            .load(reload)
            .state(state)
            .timeline(timeline)
            .error(error)
            .seek(seek_position, seek)
            .rate(rate)
            .id(day::id!(media_player))
            .height(180.0),
        label(move || status(state.get())).id(day::id!(media_state)),
        label(move || timeline_text(timeline.get())).id(day::id!(media_timeline)),
        row((
            button(res::str::play())
                .action(move || play.notify())
                .id(day::id!(media_play)),
            button(res::str::pause())
                .action(move || pause.notify())
                .id(day::id!(media_pause)),
            button(res::str::stop())
                .action(move || stop.notify())
                .id(day::id!(media_stop)),
            button(res::str::reload())
                .action(move || reload.notify())
                .id(day::id!(media_reload)),
        ))
        .spacing(8.0),
        row((button(res::str::seek_ten())
            .action(move || {
                seek_position.set(10.0);
                seek.notify();
            })
            .id(day::id!(media_seek)),))
        .spacing(8.0),
        row((
            button(res::str::speed_fast())
                .action(move || rate.set(1.25))
                .id(day::id!(media_fast)),
            button(res::str::speed_normal())
                .action(move || rate.set(1.0))
                .id(day::id!(media_normal)),
        ))
        .spacing(8.0),
        row((button(res::str::finish())
            .action(move || {
                if let Some(duration) = timeline.get_untracked().duration {
                    seek_position.set((duration - 0.5).max(0.0));
                    seek.notify();
                    play.notify();
                }
            })
            .id(day::id!(media_finish)),))
        .spacing(8.0),
        label(move || {
            if timeline.get().position >= 9.5 {
                res::str::seek_confirmed().format()
            } else {
                res::str::seek_waiting().format()
            }
        })
        .id(day::id!(media_seek_result)),
        label(move || {
            if (timeline.get().rate - 1.25).abs() < 0.01 {
                res::str::speed_confirmed().format()
            } else {
                res::str::speed_normal().format()
            }
        })
        .id(day::id!(media_rate_result)),
        label(move || {
            if timeline.get().capabilities.rate {
                res::str::rate_supported().format()
            } else {
                res::str::rate_unsupported().format()
            }
        })
        .id(day::id!(media_rate_capability)),
        label(move || {
            if error.get().is_some() {
                res::str::command_error().format()
            } else {
                res::str::command_ok().format()
            }
        })
        .id(day::id!(media_command_result)),
        labeled(
            res::str::volume(),
            slider(volume).id(day::id!(media_volume)),
        ),
        label(res::str::volume_hint()).font(Font::Footnote),
    ))
    .spacing(10.0)
}

fn audio_example() -> impl Piece {
    let app = Demo::app();
    let snapshot = app.audio.snapshot();
    let command_failed = Signal::new(false);
    let start = app.audio.clone();
    let replace = app.audio.clone();
    let pause = app.audio.clone();
    let play = app.audio.clone();
    let stop = app.audio.clone();
    let seek = app.audio.clone();
    let rate = app.audio.clone();
    let volume = app.volume;
    column((
        label(res::str::session_hint()),
        row((
            button(res::str::session_start())
                .action(move || {
                    command_failed.set(start.load(app.clip.clone(), true).is_err());
                })
                .id(day::id!(session_start)),
            button(res::str::session_replace())
                .action(move || {
                    command_failed.set(replace.load(app.tone.clone(), true).is_err());
                })
                .id(day::id!(session_replace)),
        ))
        .spacing(8.0),
        row((
            button(res::str::play())
                .action(move || play.play())
                .id(day::id!(session_play)),
            button(res::str::pause())
                .action(move || pause.pause())
                .id(day::id!(session_pause)),
            button(res::str::stop())
                .action(move || stop.stop())
                .id(day::id!(session_stop)),
        ))
        .spacing(8.0),
        row((
            button(res::str::seek_ten())
                .action(move || {
                    command_failed.set(seek.seek(10.0).is_err());
                })
                .id(day::id!(session_seek)),
            button(res::str::speed_fast())
                .action(move || {
                    command_failed.set(rate.set_rate(1.25).is_err());
                })
                .id(day::id!(session_fast)),
        ))
        .spacing(8.0),
        label(move || {
            if snapshot.get().timeline.position >= 9.5 {
                res::str::seek_confirmed().format()
            } else {
                res::str::seek_waiting().format()
            }
        })
        .id(day::id!(session_seek_result)),
        label(move || {
            if (snapshot.get().timeline.rate - 1.25).abs() < 0.01 {
                res::str::speed_confirmed().format()
            } else {
                res::str::speed_normal().format()
            }
        })
        .id(day::id!(session_rate_result)),
        label(move || {
            if snapshot.get().timeline.duration.is_some_and(|d| d < 13.0) {
                res::str::source_replaced().format()
            } else {
                res::str::source_original().format()
            }
        })
        .id(day::id!(session_source)),
        label(move || {
            if command_failed.get() || snapshot.get().error.is_some() {
                res::str::command_error().format()
            } else {
                res::str::command_ok().format()
            }
        })
        .id(day::id!(session_command_result)),
        labeled(
            res::str::volume(),
            slider(volume).id(day::id!(session_volume)),
        ),
        label(res::str::volume_hint()).font(Font::Footnote),
    ))
    .spacing(12.0)
}
