<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: MPL-2.0
-->

# day-piece-media

Play audio and video through native platform players.

[Day](https://daybrite.dev/docs/) is a Rust framework for building apps with native desktop and
mobile controls, plus HTML controls on the web.

Use a media piece for playback tied to a screen, or retain an audio session across screens.
Observe playback, errors, timeline, and stream metadata; codecs and background behavior depend
on the platform.

## Changes since 0.4.6

- `Media::muted_while(…)` mutes and unmutes the player from a `Signal<bool>` or a closure.
- `Media::now_playing(…)` publishes a `Signal<Option<NowPlayingInfo>>` or closure to the
  system's now-playing surfaces from the view.
- `MediaPatch` has a new `Muted(bool)` variant; code that matches on `MediaPatch` needs an arm.

## Use

Add `day-piece-media = "0.*"` to your app’s `[dependencies]`.
Build with the Day CLI to select the matching toolkit integration.

## Documentation

- [Setup and usage guide](https://daybrite.dev/docs/reference/media/)
- [Rust API](https://docs.rs/day-piece-media/0.4.3/day_piece_media/)
- [Crates.io](https://crates.io/crates/day-piece-media)
- [Run the demo](demo/README.md)

[Source](https://github.com/daybrite/day-piece-media) · [MPL-2.0 license](https://github.com/daybrite/day-piece-media/blob/main/LICENSE)
