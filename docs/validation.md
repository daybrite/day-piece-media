# Extraction validation

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
