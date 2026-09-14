---
title: "Media player"
description: "Playback controls, state, metadata, and platform implementations."
---

<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Media player

`day-piece-media` plays audio and video through the platform's media engine. It is an external
Day piece: the Rust API, renderers, and supporting native code all live in this repository.
See the [README](../README.md) for installation and platform dependencies.

## Controls and state

```rust
use day::prelude::*;
use day_piece_media::{PlaybackState, media};

let url = Signal::new("https://example.com/video.mp4".to_string());
let play = Trigger::new();
let pause = Trigger::new();
let stop = Trigger::new();
let load = Trigger::new();
let volume = Signal::new(0.8);
let state = Signal::new(PlaybackState::Idle);

media(url)
    .autoplay(false)
    .looping(false)
    .muted(false)
    .controls(true)
    .volume(volume)
    .play(play)
    .pause(pause)
    .stop(stop)
    .load(load)
    .state(state)
    .id("media")
    .height(280.0)
```

Connect each trigger to a button action, such as `move || play.notify()`. Play resumes the
current source; pause suspends playback. Stop ends playback; load reads the current URL and
starts it again. Use load after stop. Volume accepts a constant, signal, or closure returning a
fraction from zero to one. HarmonyOS video supports mute only; its audio player supports volume.

The URL accepts a string, signal, or closure. Native engines support local sources and network
URLs, with formats and protocols determined by the engine. The browser needs a URL it can
fetch, including a relative URL served with the app. It cannot open arbitrary filesystem paths.
Autoplay defaults to true, but browser policy may block audible playback until a user gesture.

A video fills its allocated space. `.audio_only(true)` creates an invisible player that measures
zero, useful when an app provides its own audio controls. `.controls()` has no effect there.
There is no portable seek API; native transport controls may provide a timeline.

The player writes the bound state signal when its engine reports a change:

| State | Meaning |
|---|---|
| `Idle` | No source, or playback was stopped. |
| `Loading` | Connecting, preparing, or buffering. |
| `Playing` | Playback is running. |
| `Paused` | Playback is suspended. |
| `Ended` | The source reached its end. |
| `Error(String)` | The engine failed; the string describes the error. |

`PlaybackState::is_active()` is true for Loading and Playing. State changes also reflect actions
in the native controls. Renderers translate platform callbacks into `Event::Custom` reports;
the front end decodes them into the signal. Apps do not need to handle those events directly.

## Stream metadata

```rust
use day_piece_media::StreamMetadata;

let now: Signal<Option<StreamMetadata>> = Signal::new(None);
media(url).audio_only(true).metadata(now)
```

`StreamMetadata` contains `title`, `artist`, `album`, and the original `raw` text. The signal
starts at `None` and clears when a new source loads. Availability depends on the stream and
backend:

- AppKit and UIKit observe AVFoundation timed metadata, including ICY and HLS ID3 reports.
- Other native backends use [src/icy.rs](../src/icy.rs) to request one ICY metadata block through
  `day-part-http` every 20 seconds while the source is loaded. This adds a short connection per
  interval. A server without `icy-metaint` stops the probe; stop and disposal cancel it.
- The browser backend does not extract stream metadata, so the signal remains `None`.

## Source layout and integration

[src/lib.rs](../src/lib.rs) defines the piece, properties, patches, state codes, and metadata
handling. Each `src/lib-<toolkit>.rs` implements a renderer. Native renderers register through
`linkme`; the browser registers at runtime when `media()` is called.

| Backend | Supporting implementation | Build integration |
|---|---|---|
| AppKit / UIKit | Rust Objective-C bindings around AVPlayer and native views. UIKit declares AVPlayerViewController bindings locally. | Apple frameworks from `package.metadata.day.macos` and `.ios`. |
| Android | [DayMedia.java](../platform/android/java/dev/daybrite/day/piece/media/DayMedia.java): VideoView for video, MediaPlayer for audio. | Java sources and INTERNET permission from `.android`; no additional Gradle libraries. |
| GTK | [lib-gtk.rs](../src/lib-gtk.rs): GtkVideo and GtkMediaFile. | `gtk4` bindings; playback backend and codecs installed with GTK. |
| Qt | [lib-qt-shim.cpp](../src/lib-qt-shim.cpp): QMediaPlayer, QAudioOutput, QVideoWidget. | `build.rs` compiles the shim and probes `Qt6MultimediaWidgets` with pkg-config. |
| Windows XAML | [lib-xaml-shim.cpp](../src/lib-xaml-shim.cpp): MediaPlayerElement and MediaPlayer. | `build.rs` compiles C++/WinRT against the Windows SDK. |
| HarmonyOS | [Index.ets](../platform/harmony/ets/Index.ets): ArkUI Video and AVPlayer. | `.ohos` metadata stages ArkTS into the app project. |
| Browser | [lib-dom.rs](../src/lib-dom.rs) creates audio/video elements; [browser.rs](../src/browser.rs) owns events, volume, and teardown. | `day-build` generates the crate's JavaScript bridge; Day stages it with the app. |

The browser uses `dayHost.dom.element`, `emit`, and `onRelease`. Release removes listeners before
unloading the source. Core Day supplies generic element and event operations; it contains no
media-specific browser listener or volume handling.

Android preparation is asynchronous. Volume changes apply to the current MediaPlayer and are
reapplied after a source change. VideoView reports preparation before completing a queued start,
so the crate defers its initial state report until that start has run.

## Platform limitations

- UIKit configures the process audio session for playback. Background audio additionally needs
  the app's `UIBackgroundModes` audio entry. The embedded controller currently supports inline
  playback; fullscreen presentation is not implemented.
- Android uses framework players. ExoPlayer/Media3, media sessions, and lock-screen controls are
  not included. Plain HTTP may require app-level cleartext configuration.
- GTK requires a media backend and codecs. Homebrew GTK has none, so it displays a playback
  error. GtkVideo's transport overlay cannot be hidden.
- Qt builds a URL-label fallback when Qt Multimedia is absent. Its player has no transport
  controls; use the piece's triggers.
- XAML codec support depends on Windows. Failure to create its native player produces an error
  and a URL-label fallback.
- HarmonyOS requires a working system media service and plugins. The local OpenHarmony QEMU
  image used during extraction lacks `libmedia_plugin_FileFdSource.z.so`: the demo builds and
  launches, but cannot play its bundled file. Its playback walkthrough consequently fails.
- Browser codecs and autoplay policies vary. Cross-origin sources are subject to browser policy;
  the crate does not bypass those restrictions.
- The mock feature renders a placeholder and ignores transport commands.

## Testing

`cargo test` checks state decoding, metadata parsing, and the piece's command wiring on the mock
backend. The live ICY probe test is ignored by default because it requires a network station.
`node --test tests/browser.mjs` checks event mapping, volume, and listener cleanup.

The [demo](../demo/README.md) bundles a generated clip and exercises play, pause, stop, reload,
and state readback. Run its dayscript on a platform with a working media engine. The shared CI
workflow builds the app and runs the same script on its configured targets.
