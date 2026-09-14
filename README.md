<!--
Copyright © The Daybrite Project
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# day-piece-media

Audio and video playback for [Day](https://daybrite.dev), a Rust framework that renders apps
with each platform's native widgets. This crate supplies the player; your app supplies its
source, transport controls, and presentation.

Use it for a video player, a sound-only stream, or internet radio. It supports play, pause,
stop, reload, looping, reactive volume, playback-state signals, and stream metadata. Available
formats and network protocols depend on the platform engine. There is no portable seek API;
some native controls provide their own timeline.

## Add it to a Day app

```toml
[dependencies]
day-piece-media = { git = "https://github.com/daybrite/day-piece-media.git" }
```

```rust
use day::prelude::*;
use day_piece_media::{media, PlaybackState};

let state = Signal::new(PlaybackState::Idle);
let play = Trigger::new();
let player = media("https://example.com/video.mp4")
    .autoplay(false)
    .play(play)
    .state(state)
    .height(280.0);
```

`day build` enables the renderer for your target and incorporates the crate's native
contributions. `.audio_only(true)` creates an invisible player; `.volume(signal)` accepts
values from zero to one. Use reload to load the source again and resume after stopping.

Use the same bare Day Git URL in your app and dependencies. This checkout requires Day's
`dayHost.dom` bridge API; update the CLI and framework together. Rust 1.89 or newer is required.

## Platforms and dependencies

| Target | Implementation | Requirements and limits |
|---|---|---|
| macos-appkit | [AVPlayerView](https://developer.apple.com/documentation/avkit/avplayerview) and AVPlayer | System AVKit, AVFoundation, CoreMedia; Rust bindings through `objc2`. |
| ios-uikit | [AVPlayerViewController](https://developer.apple.com/documentation/avkit/avplayerviewcontroller) | Also links AVFAudio for the playback audio session. Background playback needs app configuration. |
| android-mdc | [VideoView](https://developer.android.com/reference/android/widget/VideoView), [MediaPlayer](https://developer.android.com/reference/android/media/MediaPlayer) | Framework APIs, not ExoPlayer. Network sources require INTERNET, contributed by this crate. |
| linux-gtk, macos-gtk, windows-gtk | [GtkVideo](https://docs.gtk.org/gtk4/class.Video.html), GtkMediaFile | Requires a GTK media backend and codecs. Homebrew GTK has no playback backend; the demo shows its error. |
| linux-qt, macos-qt, windows-qt | [QMediaPlayer](https://doc.qt.io/qt-6/qmediaplayer.html), QVideoWidget | `Qt6MultimediaWidgets` through pkg-config. Without it, the crate builds a URL-label fallback. Qt provides no transport chrome here. |
| windows-xaml | [MediaPlayerElement](https://learn.microsoft.com/en-us/uwp/api/windows.ui.xaml.controls.mediaplayerelement) | Windows SDK and MSVC; system media support determines codec availability. |
| harmony-arkui | [ArkUI Video](https://github.com/openharmony/docs/blob/master/en/application-dev/reference/apis-arkui/arkui-ts/ts-media-components-video.md) and AVPlayer | HarmonyOS/OpenHarmony SDK and an accessible source supported by the engine. |
| web-dom | [HTML media elements](https://developer.mozilla.org/en-US/docs/Web/API/HTMLMediaElement) | Served URLs, browser codecs and origin policies; audible autoplay is restricted. |

HarmonyOS video supports mute, but not adjustable volume. Some OpenHarmony emulator images
lack media plugins; see the [demo notes](demo/README.md). GTK's transport controls cannot be hidden. Player errors are reported through
`PlaybackState::Error`; compiling a renderer does not guarantee that codecs are installed.

## Implementation

The Rust front end sends property updates and transport commands to per-toolkit renderers.
Each renderer converts native playback events into a common `PlaybackState`. Apple backends
receive timed metadata directly; other native backends can probe an ICY stream through
`day-part-http`. The browser does not perform that side probe.

All supporting code lives here: Objective-C bindings, Qt and Windows C++, Android Java,
HarmonyOS ArkTS, and the browser arm in [src/browser.rs](src/browser.rs). `build.rs` compiles
native shims and uses `day-build` to generate the JavaScript bridge. The browser arm owns event
listeners, volume properties, and teardown; Day supplies generic DOM access and event dispatch.

Day crates provide the piece, reactive, toolkit, HTTP, and bridge APIs. Other dependencies
include [linkme](https://docs.rs/linkme/latest/linkme/) for native renderer registration,
[log](https://docs.rs/log/latest/log/) for diagnostics, [cc](https://docs.rs/cc/latest/cc/) for
C++ builds, [gtk4](https://gtk-rs.org/gtk4-rs/stable/latest/docs/gtk4/) for GTK bindings,
[objc2](https://github.com/madsmtm/objc2) for Apple framework bindings, and
[dispatch2](https://docs.rs/dispatch2/latest/dispatch2/) for main-queue callbacks. Dependencies and
platform feature gates are listed in [Cargo.toml](Cargo.toml). Apple frameworks, Android sources
and permissions, and HarmonyOS sources are declared in `package.metadata.day` tables.
See [the implementation guide](docs/media.md) for details.

## Demo and development

[demo/](demo/) plays a generated clip bundled in the repository, with no network dependency.
Its walkthrough checks playback, pause, stop, and reload against the player state signal.

```sh
# From this repository, with day/ beside it:
day patch --local ../day
cd demo
day patch --local ../../day --local ..
day launch -p macos-appkit --script dayscript/walkthrough.yaml
```

Use `-p macos-qt`, `-p macos-gtk`, `-p ios-uikit`, `-p android-mdc`, `-p web-dom`, or another
configured target. GTK without a playback backend still builds and launches; its walkthrough
omits playback assertions on macOS and Windows. Native files are copied into the demo's app
sandbox; the browser serves the same asset beside the application.

`cargo test` checks state and metadata handling. `node --test tests/browser.mjs` checks browser
events, volume, and cleanup. CI builds desktop renderers, checks mobile and Wasm targets, and
runs the demo through the shared Day application workflow. See [local validation](docs/validation.md)
for the extraction checks and known environment limitations.

This crate was extracted from the Day workspace. Media-specific implementations now live here;
Day provides the shared toolkit and build interfaces.
