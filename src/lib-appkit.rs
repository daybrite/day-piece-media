// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// ---------------------------------------------------------------------------
// AppKit: AVPlayerView (AVKit) fronting an AVPlayer (AVFoundation), with native transport chrome
// via `controlsStyle`. A sound-only player skips the AVPlayerView: the leaf is an empty,
// hidden NSView of no size, and the AVPlayer behind it lives in this arm's own table.
//
// One NSObject per player observes what the player does (`timeControlStatus` and the current
// item's `status` over KVO, plus the played-to-end and failed-to-play notifications) and reports
// it on the node's Custom channel as a `PlaybackState`; the same observer seeks a looping player
// back to zero. It is retained here for the view's lifetime (neither KVO nor the notification
// center retains observers) and deregistered in `release`.
// ---------------------------------------------------------------------------

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ptr::{NonNull, null_mut};

use block2::RcBlock;
use day_appkit::AppKit;
use day_spec::ffi_guard;
use day_spec::{NodeId, Proposal, Size};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, extern_class,
    msg_send, sel,
};
use objc2_app_kit::{NSImage, NSView};
use objc2_av_foundation::NSValueAVFoundationExtensions;
use objc2_av_foundation::{
    AVMetadataCommonIdentifierAlbumName, AVMetadataCommonIdentifierArtist,
    AVMetadataCommonIdentifierTitle, AVMetadataIdentifierIcyMetadataStreamTitle, AVPlayer,
    AVPlayerItem, AVPlayerItemDidPlayToEndTimeNotification,
    AVPlayerItemFailedToPlayToEndTimeNotification, AVPlayerItemMetadataOutput,
    AVPlayerItemMetadataOutputPushDelegate, AVPlayerItemOutputPushDelegate, AVPlayerItemStatus,
    AVPlayerItemTrack, AVPlayerTimeControlStatus, AVTimedMetadataGroup,
};
use objc2_av_kit::{AVPlayerView, AVPlayerViewControlsStyle};
use objc2_core_media::{CMTime, kCMTimeZero};
use objc2_foundation::{
    NSArray, NSDictionary, NSKeyValueObservingOptions, NSMutableDictionary, NSNotificationCenter,
    NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration, NSSize, NSString, NSURL,
};

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
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DayMediaObserver"]
    #[ivars = ObserverIvars]
    struct MediaObserver;

    unsafe impl NSObjectProtocol for MediaObserver {}

    impl MediaObserver {
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

        // The item stopped mid-way (a dropped stream, an unreadable file). The error rides in the
        // notification's userInfo; the item's own `error` is often still nil at this point.
        #[unsafe(method(itemFailedToPlayToEnd:))]
        fn item_failed_to_play_to_end(&self, note: *mut AnyObject) {
            if !self.is_ours(note) {
                return;
            }
            self.report(report::ERROR, failure_message(note));
        }

        // KVO: either key path moved. Re-derive the whole state rather than reading the change
        // dictionary, since the answer depends on both.
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
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        let center = NSNotificationCenter::defaultCenter();
        unsafe {
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

    /// Whether a notification's object is our player's current item.
    fn is_ours(&self, note: *mut AnyObject) -> bool {
        let Some(current) = (unsafe { self.ivars().player.currentItem() }) else {
            return false;
        };
        let object: *mut AnyObject = unsafe { msg_send![&*note, object] };
        object == Retained::as_ptr(&current).cast_mut().cast()
    }

    fn report(&self, code: i32, text: String) {
        day_appkit::emit(
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
    #[name = "DayMediaMetadataAppKit"]
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
                day_appkit::emit(
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

// ---------------------------------------------------------------------------
// The system's now-playing surface (docs/media.md): MPNowPlayingInfoCenter carries what is on
// (the Now Playing item of the menu bar and the Touch Bar), and MPRemoteCommandCenter brings back what the system's transport controls ask
// (the media keys, a headset button, the lock screen), reported as `report::REMOTE` on the node
// of the player that last published. No MediaPlayer binding is vendored here, so the four
// classes are declared below and driven through `msg_send!`; the dictionary keys are the
// framework's own exported constants.
// ---------------------------------------------------------------------------

#[link(name = "MediaPlayer", kind = "framework")]
unsafe extern "C" {
    static MPMediaItemPropertyTitle: &'static NSString;
    static MPMediaItemPropertyArtist: &'static NSString;
    static MPMediaItemPropertyAlbumTitle: &'static NSString;
    static MPMediaItemPropertyPlaybackDuration: &'static NSString;
    static MPMediaItemPropertyArtwork: &'static NSString;
    static MPNowPlayingInfoPropertyElapsedPlaybackTime: &'static NSString;
    static MPNowPlayingInfoPropertyPlaybackRate: &'static NSString;
    static MPNowPlayingInfoPropertyIsLiveStream: &'static NSString;
    static MPNowPlayingInfoPropertyMediaType: &'static NSString;
}

extern_class!(
    #[unsafe(super(NSObject))]
    struct MPNowPlayingInfoCenter;
);

extern_class!(
    #[unsafe(super(NSObject))]
    struct MPRemoteCommandCenter;
);

extern_class!(
    #[unsafe(super(NSObject))]
    struct MPRemoteCommand;
);

extern_class!(
    #[unsafe(super(NSObject))]
    struct MPMediaItemArtwork;
);

/// `MPRemoteCommandHandlerStatus`: the command was applied, or nothing is on to apply it to.
const HANDLER_SUCCESS: isize = 0;
const HANDLER_NO_ITEM: isize = 110;
/// `MPNowPlayingInfoMediaTypeAudio`.
const MEDIA_TYPE_AUDIO: isize = 1;
/// `MPNowPlayingPlaybackState`: macOS routes the media keys by it (iOS derives it from the
/// rate, and has no such property).
const PLAYBACK_PLAYING: isize = 1;
const PLAYBACK_PAUSED: isize = 2;
const PLAYBACK_STOPPED: isize = 3;
/// The skip commands' intervals, in seconds (a podcast app's 30 ahead, 15 back).
const SKIP_FORWARD_SECS: f64 = 30.0;
const SKIP_BACKWARD_SECS: f64 = 15.0;

fn info_center() -> Retained<MPNowPlayingInfoCenter> {
    // SAFETY: a MediaPlayer singleton, valid for the process's life.
    unsafe { msg_send![MPNowPlayingInfoCenter::class(), defaultCenter] }
}

fn command_center() -> Retained<MPRemoteCommandCenter> {
    // SAFETY: as `info_center`.
    unsafe { msg_send![MPRemoteCommandCenter::class(), sharedCommandCenter] }
}

/// One of the command center's commands, by its property accessor (`sel!(playCommand)`).
fn command(center: &MPRemoteCommandCenter, accessor: Sel) -> Retained<MPRemoteCommand> {
    // SAFETY: every accessor passed is a MediaPlayer property returning an MPRemoteCommand.
    unsafe { msg_send![center, performSelector: accessor] }
}

fn set_enabled(center: &MPRemoteCommandCenter, accessor: Sel, enabled: bool) {
    let cmd = command(center, accessor);
    // SAFETY: `enabled` is MPRemoteCommand's BOOL property.
    unsafe {
        let _: () = msg_send![&cmd, setEnabled: enabled];
    }
}

/// Publish `info` on behalf of `node`'s player (which the system's transport commands then
/// reach), or clear the surface when it is `None`.
fn publish_now_playing(node: NodeId, info: Option<&NowPlayingInfo>) {
    install_remote_commands();
    let center = info_center();
    let Some(info) = info else {
        REMOTE_NODE.with(|n| {
            if n.get() == Some(node) {
                n.set(None);
            }
        });
        // SAFETY: a nil dictionary is how the center is told nothing is on.
        unsafe {
            let _: () = msg_send![&center, setNowPlayingInfo: None::<&NSDictionary<NSString, AnyObject>>];
            let _: () = msg_send![&center, setPlaybackState: PLAYBACK_STOPPED];
        }
        log::info!("day-piece-media: now playing cleared");
        return;
    };
    REMOTE_NODE.with(|n| n.set(Some(node)));
    let dict = NSMutableDictionary::<NSString, AnyObject>::new();
    let rate = if info.playing { info.rate } else { 0.0 };
    // SAFETY: the keys are MediaPlayer's exported constants, defined for the process's life.
    unsafe {
        dict.insert(MPMediaItemPropertyTitle, &NSString::from_str(&info.title));
        dict.insert(MPMediaItemPropertyArtist, &NSString::from_str(&info.artist));
        dict.insert(MPMediaItemPropertyAlbumTitle, &NSString::from_str(&info.album));
        if let Some(duration) = info.duration {
            dict.insert(MPMediaItemPropertyPlaybackDuration, &NSNumber::new_f64(duration));
        }
        dict.insert(MPNowPlayingInfoPropertyElapsedPlaybackTime, &NSNumber::new_f64(info.position));
        dict.insert(MPNowPlayingInfoPropertyPlaybackRate, &NSNumber::new_f64(rate));
        dict.insert(MPNowPlayingInfoPropertyIsLiveStream, &NSNumber::new_bool(info.live));
        dict.insert(MPNowPlayingInfoPropertyMediaType, &NSNumber::new_isize(MEDIA_TYPE_AUDIO));
        // `artwork_url` stays unused on Apple: the app passes a path when it has one.
        if let Some(artwork) = info.artwork_path.as_deref().and_then(artwork_for) {
            dict.insert(MPMediaItemPropertyArtwork, &artwork);
        }
        let dict: &NSDictionary<NSString, AnyObject> = &dict;
        let _: () = msg_send![&center, setNowPlayingInfo: Some(dict)];
        let state = if info.playing {
            PLAYBACK_PLAYING
        } else {
            PLAYBACK_PAUSED
        };
        let _: () = msg_send![&center, setPlaybackState: state];
    }
    let commands = command_center();
    set_enabled(&commands, sel!(nextTrackCommand), info.has_next);
    set_enabled(&commands, sel!(previousTrackCommand), info.has_previous);
    for accessor in [
        sel!(changePlaybackPositionCommand),
        sel!(skipForwardCommand),
        sel!(skipBackwardCommand),
    ] {
        set_enabled(&commands, accessor, !info.live);
    }
    log::info!(
        "day-piece-media: now playing {:?} by {:?} at {:.1}s ({})",
        info.title,
        info.artist,
        info.position,
        if info.playing { "playing" } else { "paused" }
    );
}

/// Register the transport handlers once per process. Each one reports on the node of the
/// player that last published now-playing info; with none, the system learns there is no item
/// to act on.
fn install_remote_commands() {
    if REMOTE_READY.with(|r| r.replace(true)) {
        return;
    }
    let center = command_center();
    let fixed = |accessor: Sel, text: &'static str| {
        add_handler(&command(&center, accessor), move |_event| text.to_string());
    };
    fixed(sel!(playCommand), "play");
    fixed(sel!(pauseCommand), "pause");
    fixed(sel!(togglePlayPauseCommand), "toggle");
    fixed(sel!(stopCommand), "stop");
    fixed(sel!(nextTrackCommand), "next");
    fixed(sel!(previousTrackCommand), "previous");
    add_handler(&command(&center, sel!(changePlaybackPositionCommand)), |event| {
        // SAFETY: an MPChangePlaybackPositionCommandEvent, whose positionTime is a double.
        let secs: f64 = unsafe { msg_send![event, positionTime] };
        format!("seek:{secs}")
    });
    for (accessor, verb, default_secs) in [
        (sel!(skipForwardCommand), "forward", SKIP_FORWARD_SECS),
        (sel!(skipBackwardCommand), "backward", SKIP_BACKWARD_SECS),
    ] {
        let cmd = command(&center, accessor);
        // SAFETY: `preferredIntervals` is MPSkipIntervalCommand's NSArray<NSNumber> property.
        unsafe {
            let intervals = NSArray::from_retained_slice(&[NSNumber::new_f64(default_secs)]);
            let _: () = msg_send![&cmd, setPreferredIntervals: &*intervals];
        }
        add_handler(&cmd, move |event| {
            // SAFETY: an MPSkipIntervalCommandEvent, whose interval is a double.
            let secs: f64 = unsafe { msg_send![event, interval] };
            let secs = if secs.is_finite() && secs > 0.0 {
                secs
            } else {
                default_secs
            };
            format!("{verb}:{secs}")
        });
    }
}

/// Attach a handler to `command` that reports `text(event)` as a [`report::REMOTE`] event.
fn add_handler(command: &MPRemoteCommand, text: impl Fn(&AnyObject) -> String + 'static) {
    let handler = RcBlock::new(move |event: NonNull<AnyObject>| -> isize {
        // The block runs inside a C ABI frame, so its body is contained like every other
        // trampoline in this arm.
        ffi_guard::contain(HANDLER_NO_ITEM, || {
            let Some(node) = REMOTE_NODE.with(|n| n.get()) else {
                return HANDLER_NO_ITEM;
            };
            // SAFETY: the command center hands a live event for the call's duration.
            let text = text(unsafe { event.as_ref() });
            log::info!("day-piece-media: remote command {text}");
            day_appkit::emit(
                node,
                Event::Custom {
                    tag: report::TAG,
                    num: report::REMOTE as f64,
                    text,
                },
            );
            HANDLER_SUCCESS
        })
    });
    // SAFETY: the command copies the block. The returned target token only serves to remove
    // the handler again, which never happens: the handlers live as long as the process.
    let _: *mut AnyObject = unsafe { msg_send![command, addTargetWithHandler: &*handler] };
}

/// The `MPMediaItemArtwork` for an image file, cached by path: every position update
/// republishes the whole dictionary and must not reread the file.
fn artwork_for(path: &str) -> Option<Retained<MPMediaItemArtwork>> {
    let cached = ARTWORK.with(|a| {
        a.borrow()
            .as_ref()
            .filter(|(cached_path, _)| cached_path == path)
            .map(|(_, artwork)| artwork.clone())
    });
    if cached.is_some() {
        return cached;
    }
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path))?;
    let size = image.size();
    let request = RcBlock::new(move |_wanted: NSSize| -> *mut NSImage {
        // The one image serves every size asked for; the system scales it. Contained like
        // the command handlers above.
        ffi_guard::contain(null_mut(), || Retained::as_ptr(&image).cast_mut())
    });
    // SAFETY: the artwork copies the block, which owns the image it hands back at +0.
    let artwork: Retained<MPMediaItemArtwork> = unsafe {
        msg_send![MPMediaItemArtwork::alloc(), initWithBoundsSize: size, requestHandler: &*request]
    };
    ARTWORK.with(|a| *a.borrow_mut() = Some((path.to_string(), artwork.clone())));
    Some(artwork)
}

/// What this arm keeps per realized view: the player (a sound-only view has no other route to
/// it), its observer, and whether it measures zero.
struct Live {
    player: Retained<AVPlayer>,
    observer: Retained<MediaObserver>,
    /// The timed-metadata delegate, one per player, re-attached to every item it loads.
    metadata: Retained<MetadataDelegate>,
    audio_only: bool,
}

day_core::tls_group! {
    static LIVE: RefCell<HashMap<usize, Live>> = RefCell::new(HashMap::new());
    /// The node of the player that last published now-playing info: where the system's
    /// transport commands go.
    static REMOTE_NODE: Cell<Option<NodeId>> = Cell::new(None);
    /// Whether the command center's handlers are in place (once per process).
    static REMOTE_READY: Cell<bool> = Cell::new(false);
    /// The last artwork, by path (see `artwork_for`).
    static ARTWORK: RefCell<Option<(String, Retained<MPMediaItemArtwork>)>> = RefCell::new(None);
}

fn key_of(h: &NSView) -> usize {
    (h as *const NSView) as usize
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

fn make(backend: &mut AppKit, p: &MediaProps, id: NodeId) -> Retained<NSView> {
    let mtm = backend.mtm();
    // SAFETY: creates the player (and its view) on the main thread.
    let player: Retained<AVPlayer> = unsafe { msg_send![AVPlayer::alloc(mtm), init] };
    unsafe {
        player.setMuted(p.muted);
        player.setVolume(p.volume.clamp(0.0, 1.0) as f32);
    }
    let ns: Retained<NSView> = if p.audio_only {
        // Nothing to draw: an empty, hidden view that measures zero (see `measure`).
        let view: Retained<NSView> = unsafe { msg_send![NSView::alloc(mtm), init] };
        view.setHidden(true);
        view
    } else {
        let view: Retained<AVPlayerView> = unsafe { msg_send![AVPlayerView::alloc(mtm), init] };
        unsafe {
            view.setPlayer(Some(&player));
            view.setControlsStyle(if p.controls {
                AVPlayerViewControlsStyle::Inline
            } else {
                AVPlayerViewControlsStyle::None
            });
        }
        Retained::from(<AVPlayerView as AsRef<NSView>>::as_ref(&view))
    };
    // Observe before loading, so the first item's status lands in the signal too.
    let observer = MediaObserver::new(mtm, player.clone(), id, p.looping);
    let metadata = MetadataDelegate::new(id);
    LIVE.with(|m| {
        m.borrow_mut().insert(
            key_of(&ns),
            Live {
                player: player.clone(),
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
    ns
}

fn update(backend: &mut AppKit, h: &Retained<NSView>, patch: &MediaPatch) {
    let Some((player, metadata, node)) = LIVE.with(|m| {
        m.borrow().get(&key_of(h)).map(|l| {
            (
                l.player.clone(),
                l.metadata.clone(),
                l.observer.ivars().node,
            )
        })
    }) else {
        return;
    };
    match patch {
        MediaPatch::Background(_) => {}
        MediaPatch::NowPlaying(info) => publish_now_playing(node, info.as_ref()),
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
            load_url(&player, url, &metadata, backend.mtm());
            unsafe { player.play() };
        }
        MediaPatch::Play => unsafe { player.play() },
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
fn measure(backend: &mut AppKit, h: &Retained<NSView>, proposal: Proposal) -> Size {
    if LIVE.with(|m| m.borrow().get(&key_of(h)).is_some_and(|l| l.audio_only)) {
        return Size::ZERO;
    }
    day_pieces::fill_measure(backend, h, proposal)
}

/// Stop playback and drop the retained player + observer when the view goes away.
///
/// Without this the table grows by one entry per realized media view, and, worse, its key
/// is the view's address, which the allocator reuses: a later view landing on a freed address
/// would inherit the dead entry's player and drive the wrong one.
fn release(_backend: &mut AppKit, h: &Retained<NSView>) {
    let Some(live) = LIVE.with(|m| m.borrow_mut().remove(&key_of(h))) else {
        return;
    };
    // Pause before the drop below releases the player: teardown, not deallocation order,
    // should be what silences it.
    unsafe { live.player.pause() };
    live.observer.detach();
}

day_pieces::renderer!(day_appkit::RENDERERS, AppKit,
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
