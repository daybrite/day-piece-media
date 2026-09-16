// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// The media piece's C++/WinRT shim, parallel to src/lib-qt-shim.cpp. day-xaml hosts the UWP
// system XAML (winrt::Windows::UI::Xaml, from the base Windows SDK, not WinAppSDK), so the matching
// player is Windows.UI.Xaml.Controls.MediaPlayerElement backed by a Windows.Media.Playback
// MediaPlayer. The element is boxed into a day handle via the `day_xaml_box`/`day_xaml_unbox`
// functions day-xaml-sys exports (no edits to day's toolkit crates), exactly like the
// picker/webview shims.
//
// Playback state goes back through one file-static callback (day_media_xaml_set_state_cb), fed by
// the player's PlaybackSession.PlaybackStateChanged, MediaEnded, and MediaFailed events, as the
// piece's own codes: 0 idle, 1 loading, 2 playing, 3 paused, 4 ended, 5 error. Those events arrive
// on a worker thread; the Rust side posts each report to the UI thread (src/lib-xaml.rs). A
// sound-only player keeps the same element, collapsed, so one code path serves both shapes.
//
// Written blind (no Windows host here); Windows-only, compiled by build.rs and linked alongside
// day-xaml-sys. MediaPlayerElement is core system XAML so construction can't fail like EdgeHTML,
// but creation still degrades to a URL TextBlock on any unexpected throw so the app keeps running.

#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Media.Core.h>     // MediaSource
#include <winrt/Windows.Media.Playback.h> // MediaPlayer
#include <winrt/Windows.UI.Xaml.h>
#include <winrt/Windows.UI.Xaml.Controls.h>

#include <windows.h>

#include <atomic>
#include <cstdint>
#include <map>
#include <memory>
#include <string>

using namespace winrt;
namespace WF = winrt::Windows::Foundation;
namespace WUX = winrt::Windows::UI::Xaml;
namespace WUXC = winrt::Windows::UI::Xaml::Controls;
namespace WMC = winrt::Windows::Media::Core;
namespace WMP = winrt::Windows::Media::Playback;

// The boxing functions, exported by day-xaml-sys (already linked into the app).
extern "C" void *day_xaml_box(void *iinspectable_abi);
extern "C" void *day_xaml_unbox(void *handle);

typedef void (*DayMediaStateCb)(uint64_t, int, const char *);
static DayMediaStateCb g_state_cb = nullptr;

// Set while a player is stopped, and read by its event handlers on the worker thread (wire_state).
using StoppedFlag = std::shared_ptr<std::atomic<bool>>;

// Per-handle bookkeeping the element itself cannot carry: the node to report to, whether the
// player is sound-only (which measures zero), and its stopped flag.
struct DayMediaInfo {
    uint64_t id = 0;
    bool audioOnly = false;
    StoppedFlag stopped;
};
static std::map<void *, DayMediaInfo> g_info;

static winrt::hstring hs(const char *s) {
    if (!s || !*s)
        return winrt::hstring{};
    int len = MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
    if (len <= 1)
        return winrt::hstring{};
    std::wstring w(static_cast<size_t>(len - 1), L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s, -1, w.data(), len);
    return winrt::hstring{w};
}

static std::string utf8(const winrt::hstring &h) {
    if (h.empty())
        return std::string{};
    int len = WideCharToMultiByte(CP_UTF8, 0, h.c_str(), -1, nullptr, 0, nullptr, nullptr);
    if (len <= 1)
        return std::string{};
    std::string s(static_cast<size_t>(len - 1), '\0');
    WideCharToMultiByte(CP_UTF8, 0, h.c_str(), -1, s.data(), len, nullptr, nullptr);
    return s;
}

static void report(uint64_t id, int code, const std::string &text = std::string{}) {
    if (g_state_cb)
        g_state_cb(id, code, text.c_str());
}

// A MediaSource for a url string: an http(s)/file URL is used directly; a bare local path (no
// scheme) becomes a file:/// URI (backslashes → forward). Returns null on empty/invalid input.
static WMC::MediaSource source_from(const char *url) {
    std::string s = url ? url : "";
    if (s.empty())
        return nullptr;
    if (s.find("://") == std::string::npos) {
        for (auto &ch : s)
            if (ch == '\\')
                ch = '/';
        s = "file:///" + s;
    }
    try {
        return WMC::MediaSource::CreateFromUri(WF::Uri{hs(s.c_str())});
    } catch (...) {
        return nullptr;
    }
}

static WMP::MediaPlayer player_of(void *handle) {
    WUX::UIElement e{nullptr};
    winrt::copy_from_abi(e, day_xaml_unbox(handle));
    if (auto mpe = e.try_as<WUXC::MediaPlayerElement>())
        return mpe.MediaPlayer();
    return nullptr;
}

// MediaPlayer has no Stop, so day_media_xaml_stop pauses and drops the source, then reports idle
// itself. The Paused and None states that pause and removal raise arrive later, from the worker
// thread, and would overwrite that idle; while `stopped` is set the handlers drop them. Play and
// Load clear it before they start the player again. A failure reports regardless.
static void wire_state(const WMP::MediaPlayer &player, uint64_t id, const StoppedFlag &stopped) {
    player.PlaybackSession().PlaybackStateChanged(
        [id, stopped](const WMP::MediaPlaybackSession &session, const WF::IInspectable &) {
            if (stopped->load())
                return;
            switch (session.PlaybackState()) {
            case WMP::MediaPlaybackState::None:
                report(id, 0);
                break;
            case WMP::MediaPlaybackState::Opening:
            case WMP::MediaPlaybackState::Buffering:
                report(id, 1);
                break;
            case WMP::MediaPlaybackState::Playing:
                report(id, 2);
                break;
            case WMP::MediaPlaybackState::Paused:
                report(id, 3);
                break;
            }
        });
    player.MediaEnded([id, stopped](const WMP::MediaPlayer &, const WF::IInspectable &) {
        if (!stopped->load())
            report(id, 4);
    });
    player.MediaFailed([id](const WMP::MediaPlayer &, const WMP::MediaPlayerFailedEventArgs &args) {
        std::string text = utf8(args.ErrorMessage());
        report(id, 5, text.empty() ? std::string{"playback failed"} : text);
    });
}

static void set_stopped(void *handle, bool on) {
    auto it = g_info.find(handle);
    if (it != g_info.end() && it->second.stopped)
        it->second.stopped->store(on);
}

extern "C" {

void day_media_xaml_set_state_cb(DayMediaStateCb cb) { g_state_cb = cb; }

void *day_media_xaml_new(uint64_t id, const char *url, int autoplay, int looping, int muted,
                         int controls, int audio_only, double volume) {
    void *handle = nullptr;
    auto stopped = std::make_shared<std::atomic<bool>>(false);
    try {
        WMP::MediaPlayer player;
        player.AutoPlay(autoplay != 0);
        player.IsMuted(muted != 0);
        player.IsLoopingEnabled(looping != 0);
        player.Volume(volume);
        wire_state(player, id, stopped);
        if (auto src = source_from(url))
            player.Source(src);
        WUXC::MediaPlayerElement mpe;
        mpe.AreTransportControlsEnabled(controls != 0 && audio_only == 0);
        if (audio_only != 0)
            mpe.Visibility(WUX::Visibility::Collapsed);
        mpe.SetMediaPlayer(player);
        handle = day_xaml_box(winrt::get_abi(mpe));
    } catch (...) {
        // On any unexpected failure, degrade to a label so the app still runs and screenshots.
        WUXC::TextBlock tb;
        tb.Text(hs(url ? url : ""));
        handle = day_xaml_box(winrt::get_abi(tb));
        report(id, 5, "the media player could not be created");
    }
    g_info[handle] = DayMediaInfo{id, audio_only != 0, stopped};
    return handle;
}

void day_media_xaml_load(void *handle, const char *url) {
    set_stopped(handle, false);
    try {
        if (auto p = player_of(handle)) {
            if (auto src = source_from(url))
                p.Source(src);
            p.Play();
        }
    } catch (...) {
    }
}
void day_media_xaml_play(void *handle) {
    set_stopped(handle, false);
    try {
        if (auto p = player_of(handle))
            p.Play();
    } catch (...) {
    }
}
void day_media_xaml_pause(void *handle) {
    try {
        if (auto p = player_of(handle))
            p.Pause();
    } catch (...) {
    }
}
// Dropping the source is what lets a live stream's connection go. The flag goes up first, so the
// Paused and None states this raises cannot follow the idle reported below (see wire_state).
void day_media_xaml_stop(void *handle) {
    set_stopped(handle, true);
    try {
        if (auto p = player_of(handle)) {
            p.Pause();
            p.Source(nullptr);
        }
    } catch (...) {
    }
    auto it = g_info.find(handle);
    if (it != g_info.end())
        report(it->second.id, 0);
}
void day_media_xaml_set_volume(void *handle, double volume) {
    try {
        if (auto p = player_of(handle))
            p.Volume(volume);
    } catch (...) {
    }
}
int day_media_xaml_is_audio_only(void *handle) {
    auto it = g_info.find(handle);
    return it != g_info.end() && it->second.audioOnly ? 1 : 0;
}

} // extern "C"
