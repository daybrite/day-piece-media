// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Browser playback events and properties, staged with this crate by day-build.
day_bridge::bridge! {
    #[day_bridge::declare]
    extern "day" {
        fn attach_media(id: i32, volume: f64, muted: bool);
        fn set_volume(id: i32, volume: f64);
        fn stop_media(id: i32);
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
        function attach_media(id, volume, muted) {
            const el = dayHost.dom.element(id);
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
