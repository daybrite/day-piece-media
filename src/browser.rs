// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

//! Browser playback events and properties, staged with this crate by day-build.
day_bridge::bridge! {
    #[day_bridge::declare]
    extern "day" {
        fn attach_media(id: i32, volume: f64);
        fn set_volume(id: i32, volume: f64);
    }
    #[day_bridge::impl(js, platforms = [web])]
    js!(r#"
        function set_volume(id, volume) {
            const el = dayHost.dom.element(id);
            if (el && Number.isFinite(volume)) el.volume = Math.max(0, Math.min(1, volume));
        }
        function attach_media(id, volume) {
            const el = dayHost.dom.element(id);
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
                el.pause();
                el.removeAttribute('src');
                el.load();
            });
        }
    "#);
    #[day_bridge::impl(rust, platforms = [other])]
    fn attach_media(id: i32, volume: f64) { let _ = (id, volume); }
    #[day_bridge::impl(rust, platforms = [other])]
    fn set_volume(id: i32, volume: f64) { let _ = (id, volume); }
}
pub(crate) fn attach(id: i32, volume: f64) {
    attach_media(id, volume);
}
pub(crate) fn volume(id: i32, value: f64) {
    set_volume(id, value);
}
