# Media Demo

Two examples share bundled synthetic fixtures: a view-owned video player and an app-owned
`AudioSession`. Controls exercise play, pause, stop, reload, seek, speed and volume. Timeline
and state labels come from the native engine. Sound starts at zero.

The walkthrough observes a seek while paused, a speed change while paused, end-of-source,
replacement with a shorter tone, and continued audio after switching away from its controls.
Open another window to control the same audio session alongside an independent video player.
`dayscript/shared-window.yaml` checks desktop secondary-window teardown.
`dayscript/android-lifecycle.yaml` additionally checks playback and controls after appearance
changes recreate the Android activity. Run it after the main script on Android.

All shipped text and bundled assets use generated resource accessors. The 12-second mono WAV
is a synthetic 330 Hz sine at 22,050 Hz, signed 16-bit PCM (amplitude 3000). It provides a source
with a clearly different duration for replacement assertions.

From this directory, with the rebuilt Day CLI on your PATH:

```sh
day patch --local ../../day --local ..
day launch -t macos-appkit --script dayscript/walkthrough.yaml
```

The manifest lists the other targets. macOS GTK builds and launches but Homebrew's GTK has no
media backend; the demo displays its error and skips playback assertions for that target.
Use `ANDROID_SERIAL` as well as `--android-device` when several Android emulators are running.

The fixture is generated from FFmpeg's test sources and requires no download:

```sh
ffmpeg -f lavfi -i 'testsrc2=size=320x180:rate=24' \
  -f lavfi -i 'sine=frequency=440:sample_rate=44100' -t 30 \
  -c:v libx264 -profile:v baseline -pix_fmt yuv420p -preset veryfast -crf 30 \
  -c:a aac -b:a 48k -movflags +faststart resource/assets/sample.mp4
```

For the locally installed command-line OpenHarmony SDK, the build may need
`OHOS_BASE_SDK_HOME` set to its versioned SDK root and `NODE_PATH` set to Hvigor's `node_modules`.
Use `day devices boot -t harmony-arkui --headless` to start an emulator and select its reported
`DAY_OHOS_TARGET` when launching. These are machine-local settings, not package dependencies.

The local OpenHarmony QEMU image lacks `libmedia_plugin_FileFdSource.z.so`. On that image,
the app builds and launches but reports a playback error; three playback assertions fail.
Use a device or image with the media plugins installed to run the full walkthrough.


## Generated native projects

Use a Day CLI with automatic native project generation; Apple builds require **Xcode 27
or newer**. Xcode project documents, Android Gradle files, and HarmonyOS hvigor files are
created under `build/` by `day prepare`, `day build`, and `day launch`. Open the selected
project with `day open -t <target>`; edits to generated files are disposable. Apple Swift
sources, plists and `.xcconfig` files, and any Android resource overlays, remain app-owned
under `platform/`. For additional native configuration, use
`day project add-target <target> --persist-native-project` to create a persistent override.

The demo configures iOS's audio background mode and HarmonyOS's `audioPlayback` ability mode.
The Harmony native project is intentionally persisted to retain that ability configuration.
Android's foreground service and permissions are contributed by the media crate. Start shared
audio, background the app, return, and pause to check the OS lifecycle manually. The emulator
walkthrough is not a substitute for testing physical-device interruption and route changes.
