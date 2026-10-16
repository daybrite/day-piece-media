// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// UIKit: AVPlayerViewController (AVKit) fronting an AVPlayer (AVFoundation): the same player as
// AppKit, but objc2-av-kit 0.3 only generates the macOS (AVPlayerView) binding, so here we
// hand-roll the view controller via `extern_class!` + `msg_send!` (exactly how the webview
// hand-rolls WKWebView on iOS). Its `view` is the leaf UIView day-uikit manages; the controller
// itself is retained in a thread_local (nothing else holds it once its view is embedded). A
// sound-only player has no controller: the leaf is an empty, hidden UIView of no size.
//
// The observer (KVO on `timeControlStatus` / `currentItem.status`, the played-to-end and
// failed-to-play notifications) is the same shape as lib-appkit.rs's and reports the same codes.
//
// The audio session: the first player puts the process's AVAudioSession in the `playback`
// category and activates it. Without that, iOS treats the app's sound as ambient (silenced by
// the ring switch and stopped the moment the app leaves the foreground), which no media player
// wants. Background playback additionally needs the app's `UIBackgroundModes` `audio` entry.
// ---------------------------------------------------------------------------

use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::null_mut;

use day_spec::{NodeId, Proposal, Size};
use day_uikit::Uikit;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, extern_class, msg_send,
    sel,
};
use objc2_av_foundation::NSValueAVFoundationExtensions;
use objc2_av_foundation::{
    AVMetadataCommonIdentifierAlbumName, AVMetadataCommonIdentifierArtist,
    AVMetadataCommonIdentifierTitle, AVMetadataIdentifierIcyMetadataStreamTitle, AVPlayer,
    AVPlayerItem, AVPlayerItemDidPlayToEndTimeNotification,
    AVPlayerItemFailedToPlayToEndTimeNotification, AVPlayerItemMetadataOutput,
    AVPlayerItemMetadataOutputPushDelegate, AVPlayerItemOutputPushDelegate, AVPlayerItemStatus,
    AVPlayerItemTrack, AVPlayerTimeControlStatus, AVTimedMetadataGroup,
};
use objc2_core_media::{CMTime, kCMTimeZero};
use objc2_foundation::{
    NSArray, NSKeyValueObservingOptions, NSNotificationCenter,
    NSObjectNSKeyValueObserverRegistration, NSString, NSURL,
};
use objc2_ui_kit::{UIResponder, UIView, UIViewController};

// AVPlayerViewController lives in AVKit.framework, which must be linked or
// `objc_getClass("AVPlayerViewController")` returns nil and `alloc` aborts (SIGABRT). It is
// declared via this crate's `[package.metadata.day.ios].frameworks`, which the generated DayPieces
// SwiftPM package links into the app (the framework contribution). AVAudioSession lives in
// AVFAudio, declared the same way.

// The iOS AVPlayerViewController (a UIViewController subclass). We only need a handful of methods,
// called via msg_send!.
extern_class!(
    #[unsafe(super(UIViewController, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct AVPlayerViewController;
);

/// The KVO key paths the observer registers for. `currentItem.status` reaches through to
/// whatever item is current, so a `.load()` swap stays covered without re-registering.
const KEY_PATHS: [&str; 3] = [
    "timeControlStatus",
    "currentItem.status",
    // Observing it is what makes AVFoundation process timed metadata at all ("AVPlayerItem may
    // omit the processing of timed metadata when no observer of this property is
    // registered"); the ICY block of an Icecast stream lands here.
    "currentItem.timedMetadata",
];

struct ObserverIvars {
    player: Retained<AVPlayer>,
    node: NodeId,
    looping: bool,
    resume_after_interruption: std::cell::Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayMediaObserverUIKit"]
    #[ivars = ObserverIvars]
    struct MediaObserver;

    unsafe impl NSObjectProtocol for MediaObserver {}

    impl MediaObserver {
        #[unsafe(method(audioInterrupted:))]
        fn audio_interrupted(&self, note: *mut AnyObject) {
            let player = &self.ivars().player;
            let kind = notification_integer(note, "AVAudioSessionInterruptionTypeKey");
            if kind == 1 {
                self.ivars().resume_after_interruption.set(unsafe { player.rate() } != 0.0);
                unsafe { player.pause(); }
            } else if self.ivars().resume_after_interruption.replace(false)
                && notification_integer(note, "AVAudioSessionInterruptionOptionKey") & 1 != 0 {
                activate_audio_session();
                unsafe { player.play(); }
            }
        }
        #[unsafe(method(audioRouteChanged:))]
        fn audio_route_changed(&self, note: *mut AnyObject) {
            // Old output disappeared (e.g. headphones unplugged): do not play through speakers.
            if notification_integer(note, "AVAudioSessionRouteChangeReasonKey") == 2 {
                self.ivars().resume_after_interruption.set(false);
                unsafe { self.ivars().player.pause(); }
            }
        }
        // Fired when any player item plays to its end (registered with object: nil so `.load()`
        // swaps stay covered); act only when it is the observed player's current item.
        #[unsafe(method(itemDidPlayToEnd:))]
        fn item_did_play_to_end(&self, note: *mut AnyObject) {
            if !self.is_ours(note) {
                return;
            }
            let player = &self.ivars().player;
            if self.ivars().looping {
                unsafe {
                    player.seekToTime(kCMTimeZero);
                    player.play();
                }
            } else {
                self.report(report::ENDED, String::new());
            }
        }

        #[unsafe(method(itemFailedToPlayToEnd:))]
        fn item_failed_to_play_to_end(&self, note: *mut AnyObject) {
            if !self.is_ours(note) {
                return;
            }
            self.report(report::ERROR, failure_message(note));
        }

        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: *mut AnyObject,
            _object: *mut AnyObject,
            _change: *mut AnyObject,
            _context: *mut std::ffi::c_void,
        ) {
            // SAFETY: KVO hands the key path as the NSString it was registered with.
            let is_metadata = !key_path.is_null()
                && unsafe { &*(key_path as *const NSString) }.to_string()
                    == "currentItem.timedMetadata";
            if is_metadata {
                let items = unsafe { self.ivars().player.currentItem() }.and_then(|item| {
                    #[allow(deprecated)]
                    unsafe {
                        item.timedMetadata()
                    }
                });
                if let Some(text) = items.as_deref().and_then(items_text) {
                    self.report(report::METADATA, text);
                }
                return;
            }
            let (code, text) = state_of(&self.ivars().player);
            self.report(code, text);
        }
    }
);

impl MediaObserver {
    fn new(
        mtm: MainThreadMarker,
        player: Retained<AVPlayer>,
        node: NodeId,
        looping: bool,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ObserverIvars {
            player,
            node,
            looping,
            resume_after_interruption: std::cell::Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let center = NSNotificationCenter::defaultCenter();
        unsafe {
            center.addObserver_selector_name_object(
                &this,
                sel!(audioInterrupted:),
                Some(&NSString::from_str(
                    "AVAudioSessionInterruptionNotification",
                )),
                None,
            );
            center.addObserver_selector_name_object(
                &this,
                sel!(audioRouteChanged:),
                Some(&NSString::from_str("AVAudioSessionRouteChangeNotification")),
                None,
            );
            center.addObserver_selector_name_object(
                &this,
                sel!(itemDidPlayToEnd:),
                Some(AVPlayerItemDidPlayToEndTimeNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &this,
                sel!(itemFailedToPlayToEnd:),
                Some(AVPlayerItemFailedToPlayToEndTimeNotification),
                None,
            );
            for path in KEY_PATHS {
                this.ivars().player.addObserver_forKeyPath_options_context(
                    &this,
                    &NSString::from_str(path),
                    NSKeyValueObservingOptions::New,
                    null_mut(),
                );
            }
        }
        this
    }

    /// Deregister everything before the observer drops: a center or a KVO registration left
    /// pointing at a freed object messages garbage on the next change.
    fn detach(&self) {
        unsafe {
            NSNotificationCenter::defaultCenter().removeObserver(self);
            for path in KEY_PATHS {
                self.ivars()
                    .player
                    .removeObserver_forKeyPath(self, &NSString::from_str(path));
            }
        }
    }

    fn is_ours(&self, note: *mut AnyObject) -> bool {
        let Some(current) = (unsafe { self.ivars().player.currentItem() }) else {
            return false;
        };
        let object: *mut AnyObject = unsafe { msg_send![&*note, object] };
        object == Retained::as_ptr(&current).cast_mut().cast()
    }

    fn report(&self, code: i32, text: String) {
        day_uikit::emit(
            self.ivars().node,
            Event::Custom {
                tag: report::TAG,
                num: code as f64,
                text,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// The stream's own "now playing" (docs/media.md): an AVPlayerItemMetadataOutput on every item,
// delivering timed metadata to this delegate on the main queue: the ICY `StreamTitle` an
// Icecast server interleaves into an MP3/AAC stream (AVFoundation asks for it and decodes it),
// and the ID3 frames of an HLS stream, reported as one `report::METADATA` event.
// ---------------------------------------------------------------------------

struct MetadataIvars {
    node: NodeId,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "DayMediaMetadataUIKit"]
    #[ivars = MetadataIvars]
    struct MetadataDelegate;

    unsafe impl NSObjectProtocol for MetadataDelegate {}
    unsafe impl AVPlayerItemOutputPushDelegate for MetadataDelegate {}

    unsafe impl AVPlayerItemMetadataOutputPushDelegate for MetadataDelegate {
        #[unsafe(method(metadataOutput:didOutputTimedMetadataGroups:fromPlayerItemTrack:))]
        fn did_output(
            &self,
            _output: &AVPlayerItemMetadataOutput,
            groups: &NSArray<AVTimedMetadataGroup>,
            _track: Option<&AVPlayerItemTrack>,
        ) {
            log::debug!("day-piece-media: {} timed metadata group(s)", groups.len());
            if let Some(text) = metadata_text(groups) {
                day_uikit::emit(
                    self.ivars().node,
                    Event::Custom {
                        tag: report::TAG,
                        num: report::METADATA as f64,
                        text,
                    },
                );
            }
        }
    }
);

// The delegate protocol demands `Send + Sync`; the ivars are one plain id, and every callback
// arrives on the main queue (`setDelegate_queue` below).
unsafe impl Send for MetadataDelegate {}
unsafe impl Sync for MetadataDelegate {}

impl MetadataDelegate {
    fn new(node: NodeId) -> Retained<MetadataDelegate> {
        let this = MetadataDelegate::alloc().set_ivars(MetadataIvars { node });
        unsafe { msg_send![super(this), init] }
    }
}

/// The `report::METADATA` payload for a batch of timed groups: an ICY `StreamTitle` as it
/// came, else the ID3/common title, artist, and album packed for [`StreamMetadata::from_report`].
fn metadata_text(groups: &NSArray<AVTimedMetadataGroup>) -> Option<String> {
    groups.iter().find_map(|group| {
        let items = unsafe { group.items() };
        items_text(&items)
    })
}

/// The same, for one batch of items (a group's, or the item's `timedMetadata`).
fn items_text(items: &NSArray<objc2_av_foundation::AVMetadataItem>) -> Option<String> {
    let ident = |s: Option<&'static objc2_foundation::NSString>| s.map(|s| s.to_string());
    let icy = unsafe { ident(AVMetadataIdentifierIcyMetadataStreamTitle) };
    let title_id = unsafe { ident(AVMetadataCommonIdentifierTitle) };
    let artist_id = unsafe { ident(AVMetadataCommonIdentifierArtist) };
    let album_id = unsafe { ident(AVMetadataCommonIdentifierAlbumName) };
    let (mut title, mut artist, mut album) = (String::new(), String::new(), String::new());
    for item in items.iter() {
        let id = unsafe { item.identifier() }.map(|i| i.to_string());
        let Some(value) = (unsafe { item.stringValue() }).map(|v| v.to_string()) else {
            continue;
        };
        if id.is_some() && id == icy {
            return Some(value);
        } else if id.is_some() && id == title_id {
            title = value;
        } else if id.is_some() && id == artist_id {
            artist = value;
        } else if id.is_some() && id == album_id {
            album = value;
        }
    }
    if title.is_empty() && artist.is_empty() && album.is_empty() {
        return None;
    }
    Some(format!("{title}\u{1f}{artist}\u{1f}{album}"))
}

/// Attach a metadata output for `delegate` to `item`, before the item goes to the player.
fn attach_metadata(item: &AVPlayerItem, delegate: &MetadataDelegate) {
    unsafe {
        let output = AVPlayerItemMetadataOutput::initWithIdentifiers(
            AVPlayerItemMetadataOutput::alloc(),
            None,
        );
        output.setDelegate_queue(
            Some(ProtocolObject::from_ref(delegate)),
            Some(dispatch2::DispatchQueue::main()),
        );
        item.addOutput(&output);
    }
    log::debug!("day-piece-media: metadata output attached");
}

/// The failure a `…FailedToPlayToEndTime` notification carries, as text.
fn failure_message(note: *mut AnyObject) -> String {
    unsafe {
        let info: *mut AnyObject = msg_send![&*note, userInfo];
        if info.is_null() {
            return "playback failed".into();
        }
        let key = NSString::from_str("AVPlayerItemFailedToPlayToEndTimeErrorKey");
        let err: *mut AnyObject = msg_send![&*info, objectForKey: &*key];
        if err.is_null() {
            return "playback failed".into();
        }
        let desc: Retained<NSString> = msg_send![&*err, localizedDescription];
        desc.to_string()
    }
}

/// What the player is doing right now, as a report code and detail.
fn state_of(player: &AVPlayer) -> (i32, String) {
    let Some(item) = (unsafe { player.currentItem() }) else {
        return (report::IDLE, String::new());
    };
    if unsafe { item.status() } == AVPlayerItemStatus::Failed {
        let text = unsafe { item.error() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| "playback failed".into());
        return (report::ERROR, text);
    }
    // AVPlayer sends a final paused KVO notification as well as the end notification.
    // Preserve Ended once the finite item has reached its end.
    let position = unsafe { player.currentTime().seconds() };
    let duration = unsafe { item.duration().seconds() };
    if duration.is_finite() && duration > 0.0 && position >= duration - 0.01 {
        return (report::ENDED, String::new());
    }
    match unsafe { player.timeControlStatus() } {
        AVPlayerTimeControlStatus::Playing => (report::PLAYING, String::new()),
        AVPlayerTimeControlStatus::WaitingToPlayAtSpecifiedRate => (report::LOADING, String::new()),
        _ => (report::PAUSED, String::new()),
    }
}

/// Put the process's audio session in the `playback` category, once. Failures are logged and
/// otherwise ignored: the player still plays in the foreground without it.
fn activate_audio_session() {
    unsafe {
        let session: *mut AnyObject = msg_send![class!(AVAudioSession), sharedInstance];
        if session.is_null() {
            return;
        }
        // The constant's value is its own name, which is what lets this avoid a binding.
        let category = NSString::from_str("AVAudioSessionCategoryPlayback");
        let ok: bool =
            msg_send![&*session, setCategory: &*category, error: null_mut::<*mut AnyObject>()];
        if !ok {
            log::warn!("day-piece-media: AVAudioSession refused the playback category");
        }
        let ok: bool = msg_send![&*session, setActive: true, error: null_mut::<*mut AnyObject>()];
        if !ok {
            log::warn!("day-piece-media: AVAudioSession could not be activated");
        }
    }
}

/// What we retain per media view: the player, its controller when it has one (a sound-only
/// view does not), its observer, and whether it measures zero.
struct Live {
    player: Retained<AVPlayer>,
    controller: Option<Retained<AVPlayerViewController>>,
    observer: Retained<MediaObserver>,
    /// The timed-metadata delegate, one per player, re-attached to every item it loads.
    metadata: Retained<MetadataDelegate>,
    audio_only: bool,
}

day_core::tls_group! {
    // Keep each player (and controller) alive as long as its view; the update path finds the
    // player by the leaf view's pointer.
    static LIVE: RefCell<HashMap<usize, Live>> = RefCell::new(HashMap::new());
}

fn key_of(h: &UIView) -> usize {
    (h as *const UIView) as usize
}

/// `NSURL` from the one source string: an explicit scheme parses as a URL, anything else is a
/// local file path.
fn media_url(source: &str) -> Option<Retained<NSURL>> {
    let ns = NSString::from_str(source);
    if source.contains("://") {
        NSURL::URLWithString(&ns)
    } else {
        Some(NSURL::fileURLWithPath(&ns))
    }
}

fn load_url(player: &AVPlayer, source: &str, metadata: &MetadataDelegate, mtm: MainThreadMarker) {
    let Some(url) = media_url(source) else {
        return;
    };
    let item = unsafe { AVPlayerItem::playerItemWithURL(&url, mtm) };
    attach_metadata(&item, metadata);
    unsafe { player.replaceCurrentItemWithPlayerItem(Some(&item)) };
}

fn make(_backend: &mut Uikit, p: &MediaProps, id: NodeId) -> Retained<UIView> {
    let mtm = MainThreadMarker::new().unwrap();
    activate_audio_session();
    let player: Retained<AVPlayer> = unsafe { msg_send![AVPlayer::alloc(mtm), init] };
    unsafe {
        player.setMuted(p.muted);
        player.setVolume(p.volume.clamp(0.0, 1.0) as f32);
    }
    let (view, controller): (Retained<UIView>, Option<Retained<AVPlayerViewController>>) =
        if p.audio_only {
            let view: Retained<UIView> = unsafe { msg_send![UIView::alloc(mtm), init] };
            view.setHidden(true);
            (view, None)
        } else {
            let vc: Retained<AVPlayerViewController> =
                unsafe { msg_send![AVPlayerViewController::alloc(mtm), init] };
            unsafe {
                let _: () = msg_send![&vc, setPlayer: &*player];
                let _: () = msg_send![&vc, setShowsPlaybackControls: p.controls];
            }
            let view: Retained<UIView> = unsafe { msg_send![&vc, view] };
            (view, Some(vc))
        };
    // Observe before loading, so the first item's status lands in the signal too.
    let observer = MediaObserver::new(mtm, player.clone(), id, p.looping);
    let metadata = MetadataDelegate::new(id);
    LIVE.with(|m| {
        m.borrow_mut().insert(
            key_of(&view),
            Live {
                player: player.clone(),
                controller,
                observer,
                metadata: metadata.clone(),
                audio_only: p.audio_only,
            },
        )
    });
    if !p.url.is_empty() {
        load_url(&player, &p.url, &metadata, mtm);
    }
    if p.autoplay {
        unsafe { player.play() };
    }
    view
}

fn update(_backend: &mut Uikit, h: &Retained<UIView>, patch: &MediaPatch) {
    let Some((player, metadata)) = LIVE.with(|m| {
        m.borrow()
            .get(&key_of(h))
            .map(|l| (l.player.clone(), l.metadata.clone()))
    }) else {
        return;
    };
    if matches!(
        patch,
        MediaPatch::Pause | MediaPatch::Stop | MediaPatch::Load(_)
    ) {
        LIVE.with(|m| {
            if let Some(live) = m.borrow().get(&key_of(h)) {
                live.observer.ivars().resume_after_interruption.set(false);
            }
        });
    }
    match patch {
        MediaPatch::Background(_) => {}
        MediaPatch::Poll => {
            let timeline = timeline_of(&player);
            LIVE.with(|m| {
                if let Some(live) = m.borrow().get(&key_of(h)) {
                    live.observer.report(report::TIMELINE, timeline.encode());
                }
            });
        }
        MediaPatch::Seek(seconds) => {
            let timeline = timeline_of(&player);
            if let Some(target) = timeline.seek_target(*seconds) {
                unsafe {
                    player.seekToTime_toleranceBefore_toleranceAfter(
                        CMTime::with_seconds(target, 1_000_000),
                        kCMTimeZero,
                        kCMTimeZero,
                    );
                }
            } else {
                LIVE.with(|m| {
                    if let Some(live) = m.borrow().get(&key_of(h)) {
                        live.observer
                            .report(report::COMMAND_ERROR, "seek:unsupported".into());
                    }
                });
            }
        }
        MediaPatch::Rate(rate) => unsafe {
            if valid_rate(*rate) {
                player.setDefaultRate(*rate as f32);
                if player.rate() != 0.0 {
                    player.setRate(*rate as f32);
                }
            }
        },
        MediaPatch::Load(url) => {
            let mtm = MainThreadMarker::new().unwrap();
            load_url(&player, url, &metadata, mtm);
            unsafe { player.play() };
        }
        MediaPatch::Play => {
            activate_audio_session();
            unsafe { player.play() };
        }
        MediaPatch::Pause => unsafe { player.pause() },
        // Dropping the item is what lets a live stream's connection go; the KVO on
        // `currentItem.status` reports the resulting Idle.
        MediaPatch::Stop => unsafe {
            player.pause();
            player.replaceCurrentItemWithPlayerItem(None);
        },
        MediaPatch::Volume(v) => unsafe { player.setVolume(v.clamp(0.0, 1.0) as f32) },
    }
}

/// A sound-only player takes no room; a video fills what it is offered.
fn measure(backend: &mut Uikit, h: &Retained<UIView>, proposal: Proposal) -> Size {
    if LIVE.with(|m| m.borrow().get(&key_of(h)).is_some_and(|l| l.audio_only)) {
        return Size::ZERO;
    }
    day_pieces::fill_measure(backend, h, proposal)
}

/// Stop playback and drop the retained player, controller, and observer when the view goes
/// away.
///
/// Without this the map grows one retained player (and controller) per realized media view,
/// and, worse, its key is the view's address, which the allocator reuses: a later view landing
/// on a freed address would inherit the dead entry and drive the wrong player.
fn release(_backend: &mut Uikit, h: &Retained<UIView>) {
    let Some(live) = LIVE.with(|m| m.borrow_mut().remove(&key_of(h))) else {
        return;
    };
    // Pause before the drop below releases the player: teardown, not deallocation order,
    // should be what silences it.
    unsafe { live.player.pause() };
    live.observer.detach();
    drop(live.controller);
}

day_pieces::renderer!(day_uikit::RENDERERS, Uikit,
    kind: KIND, props: MediaProps, patch: MediaPatch,
    make: make, update: update, measure: measure, release: release);

fn timeline_of(player: &AVPlayer) -> MediaTimeline {
    let mut value = MediaTimeline::default();
    unsafe {
        value.rate = player.defaultRate() as f64;
        value.capabilities.volume = true;
        let Some(item) = player.currentItem() else {
            return value;
        };
        let position = player.currentTime().seconds();
        value.position = if position.is_finite() {
            position.max(0.0)
        } else {
            0.0
        };
        let duration = item.duration().seconds();
        value.duration = (duration.is_finite() && duration >= 0.0).then_some(duration);
        let ranges = |values: Retained<NSArray<objc2_foundation::NSValue>>| -> Vec<TimeRange> {
            values
                .iter()
                .filter_map(|v| {
                    let r = v.CMTimeRangeValue();
                    let start = r.start.seconds();
                    let end = start + r.duration.seconds();
                    (start.is_finite() && end.is_finite() && start >= 0.0 && end >= start)
                        .then_some(TimeRange { start, end })
                })
                .collect()
        };
        value.seekable = ranges(item.seekableTimeRanges());
        value.buffered = ranges(item.loadedTimeRanges());
        value.capabilities.seek = !value.seekable.is_empty();
        value.capabilities.rate = item.status() == AVPlayerItemStatus::ReadyToPlay;
    }
    value
}

fn notification_integer(note: *mut AnyObject, key: &str) -> usize {
    unsafe {
        let info: *mut AnyObject = msg_send![&*note, userInfo];
        if info.is_null() {
            return 0;
        }
        let number: *mut AnyObject = msg_send![&*info, objectForKey: &*NSString::from_str(key)];
        if number.is_null() {
            0
        } else {
            msg_send![&*number, unsignedIntegerValue]
        }
    }
}
