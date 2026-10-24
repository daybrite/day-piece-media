// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Browser playback events and properties, staged with this crate by day-build.
day_bridge::bridge! {
    #[day_bridge::declare]
    extern "day" {
        fn attach_media(id: i32, volume: f64, muted: bool);
        fn set_volume(id: i32, volume: f64);
        fn stop_media(id: i32);
        fn seek_media(id: i32, seconds: f64);
        fn rate_media(id: i32, rate: f64);
        fn poll_media(id: i32);
        fn play_media(id: i32);
        fn now_playing_media(id: i32, json: &str);
    }
    #[day_bridge::impl(js, platforms = [web])]
    js!(r#"
        function set_volume(id, volume) {
            const el = dayHost.dom.element(id);
            if (el && Number.isFinite(volume)) el.volume = Math.max(0, Math.min(1, volume));
        }
        // Drop the source and let the connection go. The attribute is REMOVED, never set to "":
        // an empty `src` fails resource selection with MEDIA_ERR_SRC_NOT_SUPPORTED, and that
        // `error` event lands after the `emptied` one, so a stop would read as a format failure.
        // With no `src` at all, `load()` empties the element and selects nothing.
        function unload(el) {
            el.pause();
            el.removeAttribute('src');
            el.load();
        }
        function stop_media(id) {
            const el = dayHost.dom.element(id);
            if (el) unload(el);
        }
        function play_media(id) {
            const el = dayHost.dom.element(id);
            if (!el) return;
            const source = el.currentSrc || el.src;
            el.play()?.catch(error => {
                if (!el.dayMediaReleased && (el.currentSrc || el.src) === source && error.name !== 'AbortError')
                    dayHost.dom.emit(id, 8, 'play:' + error.name);
            });
        }
        function seek_media(id, seconds) {
            const el = dayHost.dom.element(id);
            if (!el || !Number.isFinite(seconds) || seconds < 0) return;
            let target = null;
            for (let i = 0; i < el.seekable.length; i++) {
                const candidate = Math.max(el.seekable.start(i), Math.min(el.seekable.end(i), seconds));
                if (target === null || Math.abs(candidate - seconds) < Math.abs(target - seconds)) target = candidate;
            }
            if (target === null) { dayHost.dom.emit(id, 8, 'seek:unsupported'); return; }
            try { el.currentTime = target; } catch (error) { dayHost.dom.emit(id, 8, 'seek:' + error.name); }
        }
        function rate_media(id, rate) {
            const el = dayHost.dom.element(id);
            if (!el || !Number.isFinite(rate) || rate < .25 || rate > 4) return;
            try { el.defaultPlaybackRate = rate; el.playbackRate = rate; }
            catch (error) { dayHost.dom.emit(id, 8, 'rate:' + error.name); }
        }
        function poll_media(id) {
            const el = dayHost.dom.element(id);
            if (!el) return;
            const ranges = value => {
                const result = [];
                if (value) for (let i = 0; i < value.length; i++) result.push({start: value.start(i), end: value.end(i)});
                return result;
            };
            const seekable = ranges(el.seekable);
            dayHost.dom.emit(id, 7, JSON.stringify({
                position: Number.isFinite(el.currentTime) ? Math.max(0, el.currentTime) : 0,
                duration: Number.isFinite(el.duration) ? el.duration : null,
                rate: el.playbackRate || 1, seeking: !!el.seeking,
                seekable, buffered: ranges(el.buffered),
                capabilities: {seek: seekable.length > 0, rate: true, volume: true}
            }));
        }
        // The system's now-playing surface (docs/media.md) is the Media Session API where the
        // browser has one: the metadata and position it shows, and the transport actions it
        // offers, which come back as REMOTE (9) reports on the element that published last. An
        // empty `json` clears both. A handler set to null withdraws that action's button.
        let nowPlayingId = null;
        function now_playing_media(id, json) {
            if (typeof navigator === 'undefined' || !('mediaSession' in navigator)) return;
            const ms = navigator.mediaSession;
            const actions = ['play', 'pause', 'stop', 'previoustrack', 'nexttrack', 'seekto', 'seekforward', 'seekbackward'];
            // A browser throws for an action it does not know; that action is then simply absent.
            const handle = (action, handler) => { try { ms.setActionHandler(action, handler); } catch (error) {} };
            if (!json) {
                if (nowPlayingId !== null && nowPlayingId !== id) return;
                nowPlayingId = null;
                ms.metadata = null;
                ms.playbackState = 'none';
                for (const action of actions) handle(action, null);
                return;
            }
            const info = JSON.parse(json);
            nowPlayingId = id;
            ms.metadata = new MediaMetadata({
                title: info.title || '', artist: info.artist || '', album: info.album || '',
                artwork: info.artwork_url ? [{src: info.artwork_url}] : [],
            });
            ms.playbackState = info.playing ? 'playing' : 'paused';
            if (typeof ms.setPositionState === 'function') {
                try {
                    if (!info.live && Number.isFinite(info.duration) && info.duration > 0) ms.setPositionState({
                        duration: info.duration,
                        playbackRate: info.rate > 0 ? info.rate : 1,
                        position: Math.max(0, Math.min(info.duration, info.position || 0)),
                    });
                    else ms.setPositionState();
                } catch (error) {}
            }
            const remote = text => dayHost.dom.emit(id, 9, text);
            handle('play', () => remote('play'));
            handle('pause', () => remote('pause'));
            handle('stop', () => remote('stop'));
            handle('previoustrack', info.has_previous ? () => remote('previous') : null);
            handle('nexttrack', info.has_next ? () => remote('next') : null);
            handle('seekto', info.live ? null : details => { if (Number.isFinite(details?.seekTime)) remote('seek:' + details.seekTime); });
            handle('seekforward', info.live ? null : details => remote('forward:' + (details?.seekOffset > 0 ? details.seekOffset : 30)));
            handle('seekbackward', info.live ? null : details => remote('backward:' + (details?.seekOffset > 0 ? details.seekOffset : 15)));
        }
        function attach_media(id, volume, muted) {
            const el = dayHost.dom.element(id);
            el.dayMediaReleased = false;
            // A `muted` attribute only counts when the parser sets it; a script sets the property.
            el.muted = !!muted;
            set_volume(id, volume);
            const handlers = [];
            const on = (name, handler) => { el.addEventListener(name, handler); handlers.push([name, handler]); };
            const report = (code, text = '') => dayHost.dom.emit(id, code, text);
            on('loadstart', () => report(1));
            on('waiting', () => report(1));
            on('playing', () => report(2));
            on('pause', () => { if (!el.ended) report(3); });
            on('ended', () => report(4));
            on('emptied', () => report(0));
            on('error', () => {
                const code = el.error?.code ?? 0;
                const message = code === 2 ? 'network error reaching the stream'
                    : code === 3 ? 'stream is corrupt or unreadable'
                    : code === 4 ? 'stream format not supported by this browser'
                    : el.error?.message || 'playback failed';
                if (code !== 1) report(5, message);
            });
            dayHost.dom.onRelease(id, () => {
                el.dayMediaReleased = true;
                for (const [name, handler] of handlers) el.removeEventListener(name, handler);
                unload(el);
                if (nowPlayingId === id) now_playing_media(id, '');
            });
        }
    "#);
    #[day_bridge::impl(rust, platforms = [other])]
    fn attach_media(id: i32, volume: f64, muted: bool) { let _ = (id, volume, muted); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn set_volume(id: i32, volume: f64) { let _ = (id, volume); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn stop_media(id: i32) { let _ = id; }
    #[day_bridge::impl(rust, platforms = [other])]
    fn seek_media(id: i32, seconds: f64) { let _ = (id, seconds); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn rate_media(id: i32, rate: f64) { let _ = (id, rate); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn poll_media(id: i32) { let _ = id; }
    #[day_bridge::impl(rust, platforms = [other])]
    fn play_media(id: i32) { let _ = id; }
    #[day_bridge::impl(rust, platforms = [other])]
    fn now_playing_media(id: i32, json: &str) { let _ = (id, json); }
}
pub(crate) fn attach(id: i32, volume: f64, muted: bool) {
    attach_media(id, volume, muted);
}
pub(crate) fn volume(id: i32, value: f64) {
    set_volume(id, value);
}
pub(crate) fn stop(id: i32) {
    stop_media(id);
}

pub(crate) fn seek(id: i32, value: f64) {
    seek_media(id, value);
}
pub(crate) fn rate(id: i32, value: f64) {
    rate_media(id, value);
}
pub(crate) fn poll(id: i32) {
    poll_media(id);
}
pub(crate) fn play(id: i32) {
    play_media(id);
}
/// `json` is a serialized `NowPlayingInfo`, or empty to clear the surface.
pub(crate) fn now_playing(id: i32, json: &str) {
    now_playing_media(id, json);
}
