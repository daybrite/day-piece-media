# Media Demo

A small Day app that plays the bundled `resource/assets/sample.mp4` clip. The controls exercise
play, pause, stop, reload, and volume. The status comes from the platform player, so the
walkthrough checks playback rather than just button clicks. Sound starts at zero.

From this directory, with the rebuilt Day CLI on your PATH:

```sh
day patch --local ../../day --local ..
day launch -p macos-appkit --script dayscript/walkthrough.yaml
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
Use `day ohos emulator launch --headless` to start an emulator and select its reported
`DAY_OHOS_TARGET` when launching. These are machine-local settings, not package dependencies.

The local OpenHarmony QEMU image lacks `libmedia_plugin_FileFdSource.z.so`. On that image,
the app builds and launches but reports a playback error; three playback assertions fail.
Use a device or image with the media plugins installed to run the full walkthrough.
