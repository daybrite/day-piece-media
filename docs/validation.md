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
