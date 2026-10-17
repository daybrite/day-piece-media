# Media-session validation (2026-10-16)

These are local results on macOS, using the sibling Day checkout. Windows and Linux runtime
checks require their CI hosts; no CI run is claimed here.

| Target | Result |
|---|---|
| macos-appkit | Final transport/timeline and secondary-window walkthroughs 59/59 |
| macos-qt | Final transport/timeline and secondary-window walkthroughs 59/59 |
| ios-uikit | Simulator build and walkthrough 49/49 |
| android-mdc | API 36 emulator: final walkthrough plus activity recreation 65/65 |
| web-dom | Playwright WebKit: walkthrough 49/49, including detached audio and screenshots |
| macos-gtk | Build and walkthrough 32/32 with 18 playback assertions skipped; Homebrew GTK has no media backend |
| harmony-arkui | Rust, native and ArkTS build/package succeeded against API 18; no local emulator acceptance run |
| Windows XAML/WinUI, Linux GTK/Qt | Implementations included; runtime validation remains in CI |

The final main script has 50 steps: it now selects the video example explicitly so scripts can
run in sequence. Native playback observations cover seek while paused, configured speed while
paused, completion, source replacement and audio continuing after its controls disappear.
Android's separate script verifies the player and new controls survive activity recreation.
The desktop script verifies opening and closing a second window leaves shared audio playing.
The separate mobile-window screenshot experiment exposed a Day host window-capture limitation;
the secondary-window script is a desktop check, not claimed as mobile validation.

Seven offline Rust tests pass; one network ICY probe stays ignored. Eight browser bridge tests
pass. The Day core regression test verifies that releasing an unattached native node does not
send a removal to the primary window. Screenshots from AppKit, GTK, WebKit, iOS and Android
were inspected. Physical-device call interruptions, headphone routing and OS background limits
still need device acceptance testing.

Crate formatting and Clippy pass for the default/test targets, AppKit, GTK, Qt, UIKit, Android
and DOM. The framework `scripts/ci/check-ready.sh` gate passed after the unattached-node fix.
Windows backend Clippy cannot run on this macOS host. Harmony's complete build is the local SDK
check; emulator acceptance belongs in CI under the Day repository's instructions.

`day lint --strict` reports five existing demo project-metadata findings: two pinned bundle
display names and three storefront migration/locale findings. These are separate from media
code and resource localization. The historical extraction results below are retained as history.

---

# Validation

## WinUI audio-only failure diagnosis — 2026-10-16

[Job 117077045908](https://github.com/daybrite/day-piece-media/actions/runs/38931555892/job/117077045908)
builds and packages successfully. The walkthrough passes 48/50 steps: MP4 playback,
paused seeking, speed changes, natural completion, and shared-session lifetime all pass.
Only the replacement WAV's duration and Playing assertions fail. `MediaFailed` fires,
but its `ErrorMessage` is empty, leaving only `playback failed` in the old log.

Both packaged fixtures were downloaded and compared byte-for-byte with this checkout.
The WAV is valid 12-second, 22,050 Hz, mono, signed 16-bit PCM; its RIFF/data lengths
are consistent. Missing/corrupt packaged assets are therefore unlikely.

The leading hypothesis is unavailable Windows runner audio output. The MP4 also has
a video track, so successful MP4 playback does not establish successful audio rendering.
GitHub's runner-image maintainers [document absent virtual audio hardware](https://github.com/actions/runner-images/issues/6983).
This remains a hypothesis: the old log discarded the HRESULT needed to confirm it.
Windows [supports PCM WAV](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/supported-codecs),
so replacing the tone with another container or skipping these assertions would obscure
the failure.

Changes for the next run:

- CI starts `AudioEndpointBuilder` and `Audiosrv`, enabling them if disabled, and logs
  the default audio endpoint plus sound-device status. Starting services repairs that
  prerequisite but cannot create a missing device; no virtual driver is installed.
- Native playback errors retain `MediaPlayerError`, hexadecimal `ExtendedErrorCode`,
  the message, and whether Windows reports a default output. These are diagnostics;
  the app's visible error remains localized.
- `audio-formats.yaml` runs first on WinUI. It loads the WAV at 1x in a fresh app,
  then tests seeking, 1.25x playback, and MP4/WAV replacement at the retained rate.
  The existing walkthrough keeps all its assertions.

A manual Windows run with working speakers/headphones is useful. From `demo/`, run:

```powershell
day launch -t windows-winui --script dayscript/audio-formats.yaml
day launch -t windows-winui --script dayscript/walkthrough.yaml
```

If the cold WAV fails, capture the new `MediaFailed` line and try opening
`resource/assets/tone.wav` in Windows Media Player. If only the replacement fails,
capture which phase failed and its HRESULT; that points toward replacement or rate
handling instead of basic WAV/device support. If both scripts pass locally but CI
reports a missing endpoint, CI needs a Windows runner with an audio render device.
Neither native WinUI compilation nor the PowerShell service setup can be validated
on this macOS host; a passing Windows run is still required.
Local checks passed: the new audio script 24/24 and the original walkthrough 50/50
on AppKit, with screenshots inspected; workflow actionlint and `git diff --check`
also passed.

## CI failure diagnosis — 2026-10-16

[Run 38927269908](https://github.com/daybrite/day-piece-media/actions/runs/38927269908)
tested `f649d390052ced52ec976d583d47a4b0ac08ce3e` and had two independent failures:

- The XAML and WinUI crate builds, and the WinUI demo build, failed at
  `lib-xaml-shim.cpp:269`. Windows headers expand `min` even in `std::min(...)`.
  The shim now calls `(std::min)(...)` to prevent macro expansion. A local C++20
  syntax check reproduced the failure with the Windows-style macro and passed with
  the corrected expression. Full MSVC/XAML/WinUI verification requires Windows CI.
- Chromium passed 47/50 walkthrough steps but failed both paused seeks and completion
  after seeking near the end. This reproduced locally. The Day CLI's loopback server
  ignored HTTP Range headers; adding single HTTP byte-range responses in the sibling
  `day/crates/day-cli/src/web.rs` made the unchanged walkthrough pass **50/50**, with
  no skips. The server regression tests cover partial response bytes and headers,
  open-ended/suffix ranges, unsatisfiable requests, complete-file fallbacks, and HEAD.
  WebKit also passed 50/50 steps. All seven CLI web tests, all eight media browser
  bridge tests, and Day's full `scripts/ci/check-ready.sh` gate passed.

The Chromium fix is a **Day CLI dependency**, not a media bridge change. CI installs
the CLI from Day's `main`, so the framework fix must be available there before rerunning
this media workflow. The local results above do not constitute a passing Windows CI run.

## Windows CI failure diagnosis — 2026-10-16 (afternoon)

Two Windows jobs failed after `fab629a8` landed, with different symptoms:

- **Day-Showcase `windows-winui` and `windows-xaml`** ([run 38931414052](https://github.com/daybrite/Day-Showcase/actions/runs/38931414052))
  crashed the app the moment the walkthrough navigated from the media page to the webview
  page: "engine connection lost (os error 10054)", no panic, no WER record. The Showcase
  commit touched only macOS packaging; between the last green run and this one the only
  dependencies that moved were `day` (a CLI/packaging commit plus the `remove_subtree`
  null-parent fix in day-core) and this crate (`3191fb3e` → `fab629a8`). The crash is the new
  xaml `release` hook: it closed the `MediaPlayer` while the `MediaPlayerElement` was still
  bound to it, and day-xaml then freed the element. An element that was in the visual tree
  keeps a presenter on the player, and finalizing it against a closed player fails fast inside
  XAML. The hook now follows the order MediaPlayerElement documents for an app-owned player:
  pause, drop the source, `SetMediaPlayer(null)`, then `Close()`. The demo did not crash on the
  same path because its audio session's element is never inserted into a window.
- **This repository's `demo / windows-winui`** ([run 38931555892](https://github.com/daybrite/day-piece-media/actions/runs/38931555892))
  passed 48/50 steps; `session-replace` (the audio session loading the bundled `tone.wav`,
  a 16-bit PCM mono 22.05 kHz file) reported `MediaFailed` with an empty `ErrorMessage`, so
  the log only said "playback failed". The same session had just played `sample.mp4`, so the
  player, the data directory and the file URI path all work on that runner; what differs is an
  audio-only source (and a 1.25× rate carried over from the previous load). The shim now
  reports the `MediaPlayerError` kind and the extended HRESULT when the message is empty, so
  the next run names the cause (for example `0xC00D4E86`, no audio playback device, would point
  at the hosted runner rather than the piece). The seek/rate error paths also stopped indexing
  `g_info` with `.at()` inside a `catch`, which would terminate the process for an unknown handle.

Both changes compiled under an MSVC `/Zs` syntax check against the 10.0.26100 cppwinrt
headers; the Windows runtime verification is CI's.

[Run 38943064080](https://github.com/daybrite/day-piece-media/actions/runs/38943064080) built
`5aa17110` with those changes: the Showcase-style crash is gone from this demo's teardown paths,
and the replacement load now reports `playback failed: source not supported (0xC00D36FA)`,
`MF_E_CANNOT_CREATE_SINK`. Media Foundation could not create the audio renderer, which confirms
the hypothesis above: the hosted runner has no audio endpoint, and `sample.mp4` played because a
video branch can resolve without its audio branch.

[Run 38970405945](https://github.com/daybrite/day-piece-media/actions/runs/38970405945) tried
the remedy of installing Scream, a virtual sound card, from the shared `daybrite/actions` dayapp
workflow before the app launched. Its `devcon install` printed "Updating drivers for *Scream" and
never returned; the step sat 77 minutes until the job's limit cancelled it. Scream's driver
signature expired in July 2023 ([duncanthrax/scream#202](https://github.com/duncanthrax/scream/issues/202))
and no re-signed build exists, so that step is removed again and the hosted runner stays without
an audio render device. The Windows leg therefore runs `walkthrough.yaml` and
`shared-window.yaml` only, with the two assertions after `session-replace` gated by
`skip_on: [windows-winui]`; `audio-formats.yaml` stays a manual script for a Windows host with a
playback device. `windows-audio.ps1` still logs the endpoint state before each run, so a runner
image that gains one will show it in the job log.

## Historical extraction validation

Local checks used the sibling `day/`, `day-piece-media/`, and Day-Showcase checkouts on macOS.
The new media repository has no commits; its Git origin is configured but nothing was pushed.
These results describe local validation, not a completed GitHub Actions run.

## Demo

The walkthrough checks the native playback-state signal after transport commands and captures
three screenshots. The clip is bundled with the app.

| Target | Build and launch | Walkthrough |
|---|---|---|
| macos-appkit | Passed | 15/15 |
| macos-qt | Passed | 15/15 |
| macos-gtk | Passed | 15/15, with playback assertions skipped because Homebrew GTK has no media backend |
| ios-uikit | Passed on simulator | 15/15 |
| android-mdc | Passed on emulator | 15/15 |
| web-dom | Passed in Chromium | 15/15 |
| harmony-arkui | Passed on OpenHarmony QEMU | 12/15; the image lacks its file-source media plugin |

AppKit, Android, and iOS screenshots were inspected. The phone controls were split into two
rows to avoid clipping. The HarmonyOS emulator also shows incorrect placement of the video
surface and labels; successful launch should not be read as a clean visual or playback result.
Windows and Linux builds require their own hosts and were not run locally. CI covers them.

## Windows WinUI follow-up (2026-10-01)

The Windows WinUI job in run 36895719799 failed at the first `wait_for`, after a 20-second
socket timeout (10060), without a recorded crash. Its build and packages had succeeded.
Rerunning that job without source changes passed all 15 steps and captured all three images:
[successful rerun](https://github.com/daybrite/day-piece-media/actions/runs/36895719799/job/110600449966).
Even on that successful runner, the first UI response took 11.4 seconds.

On Windows 11 ARM64, both the downloaded x64 CI package and a locally built ARM64 debug app
passed all 15 walkthrough steps, including playback, pause, stop, reload, and screenshots.

A separate experiment suspended only the demo's UI thread for 25 seconds while leaving its
script server running. The original Day CLI disconnected after 20.3 seconds with the same
10060 error; the corrected CLI received a successful response at 25 seconds. The correction
is in Day's CLI, shared by `launch --script` and `drive`: the socket must outwait the engine's
30-second UI-dispatch allowance, its implicit retries, and any configured longer allowance.
No media renderer change or skipped playback assertion was needed. The original runner's
reason for stalling remains unknown; the rerun and experiment establish a timeout defect,
not a proven MediaPlayer deadlock.

## Crate, framework, and consumer checks

- Media crate: four Rust tests passed; the live-network ICY test remains ignored.
- Browser bridge: three Node tests passed for event mapping, volume, and teardown.
- `day lint --strict`: no demo findings.
- Formatting and Clippy: passed for the crate and demo. Renderer Clippy passed for AppKit, GTK,
  Qt, UIKit, Android, ArkUI, and DOM.
- Day CLI/build suites: 407 tests passed; one documentation example remains ignored.
- Day-Showcase: built and launched on AppKit and web-dom; each full walkthrough passed 840/840
  steps. Its mock and all locally available backend Clippy checks passed against local sources.
- Website: built successfully; 1,928 internal links across 154 pages passed validation.
- CI workflow: actionlint passed. Core documentation links, symlinks, spelling, and web shim ABI
  checks passed.

## Local lint wrapper caveats

The core lint wrapper's patch-table verification skipped Showcase. Complete machine-local
Cargo overrides were installed, and every available Showcase Clippy leg was run directly.
No patch-tool implementation or metadata schema was changed as part of this extraction.

The wrapper also reports coverage-table drift because it compares generated output with Git
HEAD, which still contains the extracted crate. The updated table was checked separately:
regenerating it produced identical bytes. The remaining core lint legs passed; Windows was
unavailable on this host. Nothing was staged or committed to satisfy that comparison.
