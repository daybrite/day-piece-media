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
