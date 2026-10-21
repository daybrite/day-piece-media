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
    .no_autoplay()
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
The remaining switches are opt-ins: `.looping()` restarts at the end, `.muted()` silences the
track, and `.no_controls()` hides the toolkit's transport chrome.

The URL accepts a string, signal, or closure. Native engines support local sources and network
URLs, with formats and protocols determined by the engine. The browser needs a URL it can
fetch, including a relative URL served with the app. It cannot open arbitrary filesystem paths.
Autoplay defaults to true, but browser policy may block audible playback until a user gesture.

A video fills its allocated space. `.audio_only()` creates an invisible player that measures
zero, useful when an app provides its own audio controls. `.no_controls()` has no effect there.
Use `.timeline(binding)`, `.seek(seconds_binding, trigger)`, and `.rate(rate_binding)` for
portable timing and transport; native controls remain available. Every observer and binding
takes a `Signal` or any other `Binding` (a day-model field).

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

## Application-owned audio

`AudioSession` is a cloneable UI-thread handle. Create it after the Day host starts and retain
it in `Ambient::app()` state, as the demo does. It has no visible piece to mount. Removing a
page or closing a secondary window drops that view's controls, not the shared player. Dropping
the last handle or calling `stop()` releases the player and its source. Application exit, OS
process termination, and browser document closure still end playback.

```rust
use day_piece_media::AudioSession;

let audio = AudioSession::new();
let observed = audio.snapshot();
audio.load("https://example.com/episode.mp3", false)?;
audio.set_volume(0.7)?;
audio.set_rate(1.25)?;
audio.play();
// Once observed.get().timeline.capabilities.seek is true:
audio.seek(60.0)?;
```

`load(source, autoplay)` accepts local paths and engine-supported URLs, including Android
`content://` URIs. The app must acquire and retain document-provider permissions itself. Other
platforms likewise require the app to retain any security-scoped file access. The browser uses
served URLs or app-managed object URLs, not arbitrary local paths. The demo copies bundled
fixtures using generated `res::assets` accessors.

Every load creates a fresh native event target (a private report queue on Android). The
snapshot's `generation` changes on load/stop and `sequence` advances with observations. Delayed
end, error, and metadata events from an old source cannot advance a new queue item. Queue,
playlist, retry, podcast download, persistence and resume-position policy remain app concerns.
`Media` stays view-owned; use `AudioSession` when you need this source-generation guarantee.

On Android the handle retains an application-context MediaPlayer directly, outside the Day
activity tree. Its report queue and UI-thread observation timer survive appearance/language
recreation. Other renderers use an unattached native player node with a detached reactive scope.

## Timeline, seeking and speed

```rust
use day_piece_media::MediaTimeline;
let timeline = Signal::new(MediaTimeline::default());
let seconds = Signal::new(10.0);
let seek = Trigger::new();
let rate = Signal::new(1.0);
media(url).timeline(timeline).seek(seconds, seek).rate(rate)
```

Timeline values use seconds and refresh approximately four times per second. `duration: None`
means unknown or unbounded, not zero. `seekable` can describe a moving live/DVR window or
multiple ranges. `seek_target` clamps into the nearest available range; an empty range list or
`capabilities.seek == false` rejects seeking. Seek completion is asynchronous and preserves
paused/playing intent. Use observed position to confirm it, not the command return value.

`rate` reports the configured nonzero playback speed even while paused. Inputs must be finite
and in `0.25..=4.0`; individual engines/sources can accept a smaller set. Requested speed is
retained across pause/resume and session source replacement. Native command rejection appears
through `.error(signal)` or `snapshot.error`. `Result::Ok` means the command was accepted for
dispatch, not that asynchronous preparation, seeking or a speed change succeeded.

Errors separate invalid input, unsupported operations, playback failures and other command
failures. `operation` and `detail` are diagnostic identifiers/text: log them and map to generated
localized app messages. Do not display engine diagnostic strings as app UI. A new load clears
previous errors and observations.

| Backend | Timeline and seek | Speed | Buffered ranges |
|---|---|---|---|
| AppKit / UIKit | AVPlayer time, duration and native seekable ranges | AVPlayer default/current rate | Native loaded ranges |
| Android MDC | MediaPlayer time/duration; seek for finite sources | PlaybackParams on API 23+, preserving pause | Buffer-percentage estimate for finite sources |
| GTK | GtkMediaStream timestamp/duration and seekability | Unsupported by GTK API; reports an error | Unavailable |
| Qt | QMediaPlayer position/duration and seekability | QMediaPlayer playback rate | Unavailable |
| Windows XAML / WinUI | MediaPlaybackSession position/duration and CanSeek | MediaPlaybackSession playback rate | Unavailable |
| Harmony ArkUI | Video/AVPlayer time and finite duration | 0.75, 1, 1.25, 1.75, 2 | Unavailable |
| Web DOM | HTMLMediaElement time/duration and native TimeRanges | Browser playbackRate limits apply | Native TimeRanges |

Empty buffered ranges mean the backend has no observation, not necessarily that the stream has
no buffer. A capability is specific to the loaded source and platform, not a codec-installation
guarantee. Gapless transitions and normalization currently report false on every backend;
this API does not claim a sample-accurate queue or a DSP/limiter pipeline.

## Background audio

Call `audio.background(BackgroundAudio { title, description, channel_name })` before loading
from a user action. Format those presentation strings on the UI thread with your generated
`res::str` accessors (see `Demo::create`). This opts into mobile background execution:

- Android contributes a non-exported media-playback foreground service, permissions and a wake
  lock. It manages audio focus, transient interruptions, ducking and headphone removal. A manual
  pause cancels automatic resume. The notification stops when no opted-in player is active.
  Start from an eligible foreground user action; OS background-start restrictions still apply.
- UIKit uses the playback audio-session category, reactivates on play, and handles interruptions
  and disconnected output routes. Add `audio` to the app's `UIBackgroundModes`; the demo does.
- Harmony creates an AVSession and an audio continuous task, shared across opted-in players.
  The crate contributes `KEEP_BACKGROUND_RUNNING`; the app's ability must declare
  `backgroundModes: ["audioPlayback"]`. The demo keeps a customized native host for this entry.
  Devices without the continuous-task system capability report a background command failure.
- Desktop playback remains bound to the app process. Browsers control tab suspension and audible
  autoplay; a service worker cannot keep this audio session alive after document closure.

System transport controls, rich lock-screen metadata and media-library integration are separate
extensions. The Android foreground notification currently provides a route back into the app.

## Stream metadata

```rust
use day_piece_media::StreamMetadata;

let now: Signal<Option<StreamMetadata>> = Signal::new(None);
media(url).audio_only().metadata(now)
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
| Android | [DayMedia.java](../src/DayMedia.java): VideoView for video, MediaPlayer for audio. | Java sources and INTERNET permission from `.android`; no additional Gradle libraries. |
| GTK | [lib-gtk.rs](../src/lib-gtk.rs): GtkVideo and GtkMediaFile. | `gtk4` bindings; playback backend and codecs installed with GTK. |
| Qt | [lib-qt-shim.cpp](../src/lib-qt-shim.cpp): QMediaPlayer, QAudioOutput, QVideoWidget. | `build.rs` compiles the shim and probes `Qt6MultimediaWidgets` with pkg-config. |
| Windows XAML | [lib-xaml-shim.cpp](../src/lib-xaml-shim.cpp): MediaPlayerElement and MediaPlayer. | `build.rs` compiles C++/WinRT against the Windows SDK. |
| HarmonyOS | [Index.ets](../platform/harmony/ets/Index.ets): ArkUI Video and AVPlayer. | `.ohos` metadata stages ArkTS into the app project. |
| Browser | [lib-dom.rs](../src/lib-dom.rs) creates audio/video elements; [browser.rs](../src/browser.rs) owns events, volume, muting, stop, and teardown. | `day-build` generates the crate's JavaScript bridge; Day stages it with the app. |

The browser uses `dayHost.dom.element`, `emit`, and `onRelease`. Stop and release remove the
`src` attribute and reload the element, which empties it without an error; an empty `src` would
report a format error instead. Muting sets the element property, since a `muted` attribute set
from a script has no effect. Release also removes listeners first. Core Day supplies generic
element and event operations; it contains no media-specific browser listener or volume handling.

Android preparation is asynchronous. Volume changes apply to the current MediaPlayer and are
reapplied after a source change. VideoView reports preparation before completing a queued start,
so the crate defers its initial state report until that start has run.

Windows MediaPlayer raises its playback events on a worker thread. The crate posts each state
report to the UI thread, where Day takes events. MediaPlayer has no stop, so the crate pauses and
drops the source, then reports idle; the pause and removal events that follow are ignored until
the next play or load.

## Platform limitations

- UIKit configures the process audio session for playback. Background audio additionally needs
  the app's `UIBackgroundModes` audio entry. The embedded controller currently supports inline
  playback; fullscreen presentation is not implemented.
- Android uses framework players. ExoPlayer/Media3 and system transport controls are not included. Plain HTTP may require app-level cleartext configuration.
- GTK requires a media backend and codecs. Homebrew GTK has none, so it displays a playback
  error. GtkVideo's transport overlay cannot be hidden.
- A missing decoder is not an error to GTK's GStreamer backend: it prepares the stream without the
  track it can't decode and plays on, so a video stayed black while its state read Playing. When
  a video player's stream prepares with no video, the GTK arm asks GStreamer's discoverer (loaded
  at run time from `libgstpbutils`, so building needs no GStreamer development packages) which
  decoders the file lacks, and sets the stream's error to name them. GtkVideo then shows its error
  icon with that message as the tooltip, and the state reports `PlaybackState::Error`. A file
  with sound and no video keeps playing. On Debian and Ubuntu, `gstreamer1.0-libav` supplies the
  usual H.264 and AAC decoders.
- Qt builds a URL-label fallback when Qt Multimedia is absent. Its player has no transport
  controls; use the piece's triggers.
- A linux-qt AppImage plays through Qt's FFmpeg backend. The AppImage bundles libgstreamer but
  none of GStreamer's element plugins, and the bundled library looks for plugins only beside
  itself, so the GStreamer backend would find no elements and crash. The crate's
  `[package.metadata.day.appimage]` table sets `QT_MEDIA_BACKEND=ffmpeg` in the AppImage
  launcher. A `QT_MEDIA_BACKEND` already set in the user's environment takes precedence.
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

The [demo](../demo/README.md) bundles a generated clip and tone, exercises timing and transport,
and verifies shared-session ownership and source replacement. Run its dayscript on a platform with a working media engine. The shared CI
workflow builds the app and runs the same script on its configured targets.
