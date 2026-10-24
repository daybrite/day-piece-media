// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// The day-piece-media crate's Android backend, bundled here and folded into the app's Gradle
// build via [package.metadata.day.android], with no edits to day-mdc. It uses only
// day-mdc's public Java surface: DayBridge.ctx (the Context) and DayBridge.nativeOnEvent (the
// event trampoline). android.widget.VideoView + MediaController and android.media.MediaPlayer are
// framework classes, so the piece adds no Gradle dependencies; it declares the INTERNET permission
// in Cargo.toml for network sources, which `day build` merges into the app manifest. See
// docs/extending.md.
package dev.daybrite.day.piece.media;

import android.media.AudioAttributes;
import android.media.AudioManager;
import android.media.AudioFocusRequest;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.BroadcastReceiver;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.os.PowerManager;
import android.media.MediaMetadata;
import android.media.MediaPlayer;
import android.media.PlaybackParams;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.util.LruCache;
import org.json.JSONObject;
import org.json.JSONArray;
import android.net.Uri;
import android.os.Handler;
import android.os.Looper;
import android.view.View;
import android.widget.MediaController;
import android.widget.VideoView;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.lang.ref.WeakReference;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.Map;
import java.util.WeakHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import dev.daybrite.day.bridge.DayBridge;

/**
 * Wraps android.widget.VideoView (with optional MediaController chrome) for pictures, or a bare
 * MediaPlayer behind an empty View for sound only. Playback state is reported to the piece's Rust
 * front-end through DayBridge.nativeOnEvent's Custom kind (12) as the piece's own codes.
 */
public final class DayMedia {
    private DayMedia() {}

    private static final int K_CUSTOM = 12;
    private static final int IDLE = 0, LOADING = 1, PLAYING = 2, PAUSED = 3, ENDED = 4, ERROR = 5;
    /** A transport command from the system, in RemoteCommand's wire form (lib.rs `report::REMOTE`). */
    private static final int REMOTE = 9;

    /** Everything a live player carries: the node it reports to and its MediaPlayer. */
    private static final class Live {
        final long id;
        final JSONArray sessionReports = new JSONArray();
        final boolean audioOnly;
        final boolean looping;
        final boolean muted;
        float volume;
        float rate = 1f;
        float appliedRate = 1f;
        int bufferedPercent;
        boolean seeking;
        JSONObject background;
        boolean active;
        boolean resumeAfterFocus;
        AudioFocusRequest focusRequest;
        AudioManager.OnAudioFocusChangeListener focusListener;
        BroadcastReceiver noisy;
        boolean focusHeld;
        AudioManager audioManager() { return (AudioManager) DayBridge.ctx.getApplicationContext().getSystemService(Context.AUDIO_SERVICE); }
        boolean acquireFocus() {
            if (focusHeld) return true;
            AudioManager manager = audioManager();
            focusListener = change -> {
                MediaPlayer mp = audioOnly ? player : videoPlayer;
                if (mp == null || !prepared) return;
                try {
                    if (change == AudioManager.AUDIOFOCUS_GAIN) {
                        applyVolume(mp);
                        if (resumeAfterFocus) { resumeAfterFocus = false; playWhenReady = true; mp.start(); report(PLAYING, ""); }
                    } else if (change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK) {
                        mp.setVolume(muted ? 0f : volume * .2f, muted ? 0f : volume * .2f);
                    } else {
                        resumeAfterFocus = change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT && mp.isPlaying();
                        mp.pause();
                        report(PAUSED, "");
                        if (change == AudioManager.AUDIOFOCUS_LOSS) { resumeAfterFocus = false; playWhenReady = false; abandonFocus(); }
                    }
                } catch (IllegalStateException ignored) {}
            };
            int result;
            if (android.os.Build.VERSION.SDK_INT >= 26) {
                focusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
                    .setOnAudioFocusChangeListener(focusListener, MAIN).build();
                result = manager.requestAudioFocus(focusRequest);
            } else result = manager.requestAudioFocus(focusListener, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN);
            focusHeld = result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
            if (!focusHeld) report(8, "play:audio-focus-denied");
            if (focusHeld && noisy == null) {
                noisy = new BroadcastReceiver() {
                    @Override public void onReceive(Context context, Intent intent) {
                        MediaPlayer mp = audioOnly ? player : videoPlayer;
                        resumeAfterFocus = false;
                        playWhenReady = false;
                        if (mp != null && prepared) { mp.pause(); report(PAUSED, ""); }
                        abandonFocus();
                    }
                };
                Context context = DayBridge.ctx.getApplicationContext();
                if (android.os.Build.VERSION.SDK_INT >= 33) context.registerReceiver(noisy,
                    new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY), Context.RECEIVER_NOT_EXPORTED);
                else context.registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
            }
            return focusHeld;
        }
        void abandonFocus() {
            if (focusHeld) {
                if (android.os.Build.VERSION.SDK_INT >= 26 && focusRequest != null) audioManager().abandonAudioFocusRequest(focusRequest);
                else if (focusListener != null) audioManager().abandonAudioFocus(focusListener);
            }
            focusHeld = false;
            if (noisy != null) { DayBridge.ctx.getApplicationContext().unregisterReceiver(noisy); noisy = null; }
        }
        /** The sound-only player; a VideoView owns its own MediaPlayer and this stays null. */
        MediaPlayer player;
        /** Whether the current source has finished preparing (so a play is a real start). */
        MediaPlayer videoPlayer;
        boolean prepared;
        /** Whether the app asked to play before the source was ready. */
        boolean playWhenReady;

        Live(long id, boolean audioOnly, boolean looping, boolean muted, float volume) {
            this.id = id;
            this.audioOnly = audioOnly;
            this.looping = looping;
            this.muted = muted;
            this.volume = volume;
        }

        void report(int code, String text) {
            if (code <= ERROR) {
                boolean next = code == PLAYING || code == LOADING || (code == PAUSED && resumeAfterFocus);
                if (active != next) { active = next; updateBackgroundService(); }
                if (code == IDLE || code == ENDED || code == ERROR) abandonFocus();
            }
            if (id == 0) {
                // Drained by the Rust session timer, independent of activity/tree generations.
                JSONArray event = new JSONArray();
                event.put(code); event.put(text == null ? "" : text);
                sessionReports.put(event);
            } else DayBridge.nativeOnEvent(id, K_CUSTOM, (double) code, text == null ? "" : text);
        }

        void applyVolume(MediaPlayer mp) {
            float v = muted ? 0f : volume;
            mp.setVolume(v, v);
        }
    }

    // Weak keys: a released view must not pin itself here.
    private static final Map<View, Live> LIVE = new WeakHashMap<>();

    static boolean hasBackgroundPlayback() {
        for (Live live : LIVE.values()) if (live.audioOnly && live.background != null && live.active) return true;
        return false;
    }
    private static void updateBackgroundService() {
        Context context = DayBridge.ctx.getApplicationContext();
        for (Live live : LIVE.values()) {
            if (!live.audioOnly || live.background == null || !live.active) continue;
            Intent intent = new Intent(context, DayMediaService.class)
                .putExtra("title", live.background.optString("title"))
                .putExtra("description", live.background.optString("description"))
                .putExtra("channel", live.background.optString("channel_name"));
            try {
                if (android.os.Build.VERSION.SDK_INT >= 26) context.startForegroundService(intent);
                else context.startService(intent);
            } catch (RuntimeException error) {
                live.report(8, "background:" + error.getClass().getSimpleName());
            }
            return;
        }
        context.stopService(new Intent(context, DayMediaService.class));
    }

    /** Where a source is opened and an old player torn down. `setDataSource` on an http URI
     *  resolves and connects before returning, and `reset`/`release` wait on the same socket:
     *  on the UI thread either one is an ANR on a slow network. The player itself is created
     *  on the UI thread, so its listeners still run there. */
    private static final ExecutorService IO = Executors.newSingleThreadExecutor();
    private static final Handler MAIN = new Handler(Looper.getMainLooper());

    private static String errorText(int what, int extra) {
        String base;
        switch (what) {
            case MediaPlayer.MEDIA_ERROR_SERVER_DIED:
                base = "the media server died";
                break;
            default:
                base = "playback failed";
                break;
        }
        switch (extra) {
            case MediaPlayer.MEDIA_ERROR_IO:
                return "network error reaching the stream";
            case MediaPlayer.MEDIA_ERROR_MALFORMED:
                return "stream is corrupt or unreadable";
            case MediaPlayer.MEDIA_ERROR_UNSUPPORTED:
                return "stream format not supported";
            case MediaPlayer.MEDIA_ERROR_TIMED_OUT:
                return "the stream timed out";
            default:
                return base + " (" + what + "/" + extra + ")";
        }
    }

    /** An unplayed video must not steal focus from application-owned audio when a page mounts. */
    private static final class MediaVideoView extends VideoView {
        private final Live live;
        MediaVideoView(Context context, Live live) {
            super(context);
            this.live = live;
            if (android.os.Build.VERSION.SDK_INT >= 26) setAudioFocusRequest(AudioManager.AUDIOFOCUS_NONE);
        }
        @Override public void start() {
            if (!live.acquireFocus()) return;
            super.start();
            if (isPlaying()) live.report(PLAYING, "");
        }
        @Override public void pause() {
            super.pause();
            live.resumeAfterFocus = false;
            live.abandonFocus();
            live.report(PAUSED, "");
        }
        @Override public void stopPlayback() { live.abandonFocus(); super.stopPlayback(); }
    }

    public static View makeMedia(
            long id, String url, boolean autoplay, boolean looping, boolean muted,
            boolean controls, boolean audioOnly, double volume) {
        final Live live = new Live(id, audioOnly, looping, muted, (float) volume);
        if (audioOnly) {
            View host = new View(DayBridge.ctx.getApplicationContext());
            host.setVisibility(View.GONE);
            LIVE.put(host, live);
            if (url != null && !url.isEmpty()) {
                loadAudio(live, url, autoplay);
            }
            return host;
        }
        VideoView video = new MediaVideoView(DayBridge.ctx, live);
        LIVE.put(video, live);
        if (controls) {
            MediaController mc = new MediaController(DayBridge.ctx);
            mc.setAnchorView(video);
            video.setMediaController(mc);
        }
        // looping/muted/volume live on the underlying MediaPlayer, only reachable once prepared.
        // The listener re-fires for every setVideoURI (mediaCommand's load), keeping them sticky.
        video.setOnPreparedListener(new MediaPlayer.OnPreparedListener() {
            @Override
            public void onPrepared(MediaPlayer mp) {
                mp.setWakeMode(DayBridge.ctx.getApplicationContext(), PowerManager.PARTIAL_WAKE_LOCK);
                mp.setLooping(live.looping);
                live.applyVolume(mp);
                live.prepared = true;
                live.videoPlayer = mp;
                mp.setOnSeekCompleteListener(p -> { live.seeking = false; });
                mp.setOnBufferingUpdateListener((p, percent) -> { live.bufferedPercent = percent; });
                applyRate(live, mp);
                // VideoView invokes this callback before applying a queued start(). Read the
                // final state on the next UI turn, after its internal preparation finishes.
                video.post(() -> {
                    if (LIVE.get(video) == live && live.prepared && video.isPlaying()) live.report(PLAYING, "");
                });
                mp.setOnInfoListener(new MediaPlayer.OnInfoListener() {
                    @Override
                    public boolean onInfo(MediaPlayer p, int what, int extra) {
                        if (what == MediaPlayer.MEDIA_INFO_BUFFERING_START) {
                            live.report(LOADING, "");
                        } else if (what == MediaPlayer.MEDIA_INFO_BUFFERING_END) {
                            live.report(p.isPlaying() ? PLAYING : PAUSED, "");
                        }
                        return false;
                    }
                });
                live.report(mp.isPlaying() ? PLAYING : PAUSED, "");
            }
        });
        video.setOnCompletionListener(new MediaPlayer.OnCompletionListener() {
            @Override
            public void onCompletion(MediaPlayer mp) {
                if (!live.looping) {
                    live.report(ENDED, "");
                }
            }
        });
        video.setOnErrorListener(new MediaPlayer.OnErrorListener() {
            @Override
            public boolean onError(MediaPlayer mp, int what, int extra) {
                live.report(ERROR, errorText(what, extra));
                return true; // handled: no system "Can't play this video" dialog
            }
        });
        if (url != null && !url.isEmpty()) {
            // Uri.parse handles file paths and http(s)/content URIs (setVideoPath is the same call).
            live.report(LOADING, "");
            video.setVideoURI(Uri.parse(url));
            if (autoplay) {
                video.start();
            }
        }
        return video;
    }

    /** Build (or rebuild) the sound-only MediaPlayer for `url`. Asynchronous: a stream prepares
     *  off the UI thread and reports its state as it goes. */
    private static void loadAudio(final Live live, String url, boolean playWhenReady) {
        releasePlayer(live);
        final MediaPlayer mp = new MediaPlayer();
        live.player = mp;
        live.prepared = false;
        live.bufferedPercent = 0;
        live.seeking = false;
        live.playWhenReady = playWhenReady;
        mp.setWakeMode(DayBridge.ctx.getApplicationContext(), PowerManager.PARTIAL_WAKE_LOCK);
        mp.setOnSeekCompleteListener(p -> { if (live.player == p) live.seeking = false; });
        mp.setOnBufferingUpdateListener((p, percent) -> { if (live.player == p) live.bufferedPercent = percent; });
        mp.setAudioAttributes(new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build());
        mp.setLooping(live.looping);
        live.applyVolume(mp);
        mp.setOnPreparedListener(new MediaPlayer.OnPreparedListener() {
            @Override
            public void onPrepared(MediaPlayer p) {
                if (live.player != p) {
                    return; // superseded by a later load
                }
                live.prepared = true;
                applyRate(live, p);
                if (live.playWhenReady && live.acquireFocus()) {
                    p.start();
                    live.report(PLAYING, "");
                } else {
                    live.report(PAUSED, "");
                }
            }
        });
        mp.setOnInfoListener(new MediaPlayer.OnInfoListener() {
            @Override
            public boolean onInfo(MediaPlayer p, int what, int extra) {
                if (live.player != p) {
                    return false;
                }
                if (what == MediaPlayer.MEDIA_INFO_BUFFERING_START) {
                    live.report(LOADING, "");
                } else if (what == MediaPlayer.MEDIA_INFO_BUFFERING_END) {
                    live.report(p.isPlaying() ? PLAYING : PAUSED, "");
                }
                return false;
            }
        });
        mp.setOnCompletionListener(new MediaPlayer.OnCompletionListener() {
            @Override
            public void onCompletion(MediaPlayer p) {
                if (live.player == p && !live.looping) {
                    live.report(ENDED, "");
                }
            }
        });
        mp.setOnErrorListener(new MediaPlayer.OnErrorListener() {
            @Override
            public boolean onError(MediaPlayer p, int what, int extra) {
                if (live.player == p) {
                    live.report(ERROR, errorText(what, extra));
                }
                return true;
            }
        });
        live.report(LOADING, "");
        final Uri uri = Uri.parse(url);
        IO.execute(new Runnable() {
            @Override
            public void run() {
                try {
                    mp.setDataSource(DayBridge.ctx, uri);
                    mp.prepareAsync();
                } catch (IOException | IllegalArgumentException | IllegalStateException
                         | SecurityException e) {
                    final String text =
                            e.getMessage() == null ? "could not open the stream" : e.getMessage();
                    MAIN.post(new Runnable() {
                        @Override
                        public void run() {
                            if (live.player == mp) {
                                live.report(ERROR, text);
                            }
                        }
                    });
                }
            }
        });
    }

    private static void releasePlayer(Live live) {
        final MediaPlayer old = live.player;
        live.active = false;
        live.abandonFocus();
        updateBackgroundService();
        live.player = null;
        live.prepared = false;
        if (old != null) {
            IO.execute(new Runnable() {
                @Override
                public void run() {
                    try {
                        old.reset();
                    } catch (IllegalStateException ignored) {
                    }
                    old.release();
                }
            });
        }
    }

    // setPlaybackParams can start a paused MediaPlayer. Preserve transport intent explicitly.
    private static void applyRate(Live live, MediaPlayer player) {
        if (android.os.Build.VERSION.SDK_INT < 23) {
            if (live.rate != 1f) live.report(8, "rate:unsupported");
            return;
        }
        try {
            boolean playing = player.isPlaying();
            player.setPlaybackParams(new PlaybackParams().setSpeed(live.rate).setPitch(1f));
            live.appliedRate = player.getPlaybackParams().getSpeed();
            if (!playing) player.pause();
        } catch (IllegalArgumentException | IllegalStateException error) {
            live.report(8, "rate:unsupported");
        }
    }
    private static void poll(Live live) {
        MediaPlayer mp = live.audioOnly ? live.player : live.videoPlayer;
        try {
            JSONObject value = new JSONObject();
            JSONObject capabilities = new JSONObject();
            capabilities.put("volume", true);
            capabilities.put("rate", android.os.Build.VERSION.SDK_INT >= 23 && live.prepared);
            int duration = mp != null && live.prepared ? mp.getDuration() : -1;
            capabilities.put("seek", duration > 0);
            value.put("position", mp != null && live.prepared ? Math.max(0, mp.getCurrentPosition()) / 1000.0 : 0);
            value.put("duration", duration > 0 ? duration / 1000.0 : JSONObject.NULL);
            // Android reports speed zero while paused. Expose the last accepted nonzero speed.
            float actualRate = mp != null && live.prepared && android.os.Build.VERSION.SDK_INT >= 23 ? mp.getPlaybackParams().getSpeed() : 1f;
            value.put("rate", actualRate > 0 ? actualRate : live.appliedRate);
            value.put("seeking", live.seeking);
            JSONArray ranges = new JSONArray();
            if (duration > 0) ranges.put(new JSONObject().put("start", 0).put("end", duration / 1000.0));
            value.put("seekable", ranges);
            JSONArray buffered = new JSONArray();
            if (duration > 0 && live.bufferedPercent > 0) buffered.put(new JSONObject().put("start", 0).put("end", duration / 1000.0 * live.bufferedPercent / 100.0));
            value.put("buffered", buffered);
            value.put("capabilities", capabilities);
            live.report(7, value.toString());
        } catch (Exception error) { live.report(8, "poll:" + error.getClass().getSimpleName()); }
    }

    public static String sessionReports(View view) {
        Live live = LIVE.get(view);
        if (live == null) return "[]";
        poll(live);
        String result = live.sessionReports.toString();
        while (live.sessionReports.length() > 0) live.sessionReports.remove(live.sessionReports.length() - 1);
        return result;
    }

    // --- The system's now-playing surface (docs/media.md): one framework MediaSession per
    // process, named "day.media", owned by whichever player published last. The lock screen,
    // the media keys and the MediaStyle notification (DayMediaService) all read it, and their
    // transport commands come back as REMOTE reports on that owner: into a session player's
    // report queue, or through nativeOnEvent for a view-backed one. The owner is held weakly
    // so a released player is never pinned; its release clears the surface.
    private static MediaSession session;
    private static WeakReference<Live> nowPlayingOwner = new WeakReference<>(null);
    private static JSONObject nowPlaying;
    /** The artwork source the current info names (`file:<path>` or the URL), and its bitmap once
     *  decoded; decoding and fetching happen on IO, keyed into a small cache. */
    private static String artworkKey;
    private static Bitmap artwork;
    private static final LruCache<String, Bitmap> ARTWORK = new LruCache<>(4);
    private static final int ARTWORK_SIDE = 512;
    private static final int ARTWORK_BYTES = 16 << 20;

    static MediaSession.Token sessionToken() { return session == null ? null : session.getSessionToken(); }
    static JSONObject nowPlaying() { return nowPlaying; }
    static Bitmap artwork() { return artwork; }
    /** A string field, with JSON `null` (serde's `None`) read as empty rather than "null". */
    static String text(JSONObject info, String name) { return info.isNull(name) ? "" : info.optString(name, ""); }

    /** Deliver a transport command to the player that owns the now-playing surface. */
    static void remote(String text) {
        Live owner = nowPlayingOwner.get();
        if (owner != null) owner.report(REMOTE, text);
    }

    @SuppressWarnings("deprecation")
    private static MediaSession ensureSession() {
        if (session != null) return session;
        Context context = DayBridge.ctx.getApplicationContext();
        MediaSession s = new MediaSession(context, "day.media");
        // Implied from API 26 on; the flags are what routes media buttons here below it.
        if (android.os.Build.VERSION.SDK_INT < 26) s.setFlags(MediaSession.FLAG_HANDLES_MEDIA_BUTTONS | MediaSession.FLAG_HANDLES_TRANSPORT_CONTROLS);
        // Media buttons (headset, KEYCODE_MEDIA_*) reach these through the callback's default
        // onMediaButtonEvent, which picks onPlay/onPause for PLAY_PAUSE from the published state.
        s.setCallback(new MediaSession.Callback() {
            @Override public void onPlay() { remote("play"); }
            @Override public void onPause() { remote("pause"); }
            @Override public void onStop() { remote("stop"); }
            @Override public void onSkipToNext() { remote("next"); }
            @Override public void onSkipToPrevious() { remote("previous"); }
            @Override public void onSeekTo(long pos) { remote("seek:" + Math.max(0L, pos) / 1000.0); }
            @Override public void onFastForward() { remote("forward:30"); }
            @Override public void onRewind() { remote("backward:15"); }
        }, MAIN);
        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        if (launch != null) s.setSessionActivity(PendingIntent.getActivity(context, 0, launch,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        session = s;
        return s;
    }

    /** Command 9: `json` describes what is on (NowPlayingInfo), or is empty to clear. */
    private static void setNowPlaying(Live live, String json) {
        if (json == null || json.isEmpty()) {
            Live owner = nowPlayingOwner.get();
            if (owner == null || owner == live) clearNowPlaying();
            return;
        }
        JSONObject info;
        try { info = new JSONObject(json); } catch (Exception error) { live.report(8, "nowplaying:invalid"); return; }
        nowPlayingOwner = new WeakReference<>(live);
        nowPlaying = info;
        applyNowPlaying(ensureSession(), info);
    }

    private static void applyNowPlaying(MediaSession s, JSONObject info) {
        MediaMetadata.Builder metadata = new MediaMetadata.Builder()
            .putString(MediaMetadata.METADATA_KEY_TITLE, text(info, "title"))
            .putString(MediaMetadata.METADATA_KEY_ARTIST, text(info, "artist"))
            .putString(MediaMetadata.METADATA_KEY_ALBUM, text(info, "album"));
        double duration = info.optDouble("duration", Double.NaN);
        if (!Double.isNaN(duration) && duration > 0) metadata.putLong(MediaMetadata.METADATA_KEY_DURATION, (long) (duration * 1000));
        String path = text(info, "artwork_path");
        String url = text(info, "artwork_url");
        String key = !path.isEmpty() ? "file:" + path : !url.isEmpty() ? url : null;
        if (key == null) { artworkKey = null; artwork = null; }
        else if (!key.equals(artworkKey)) {
            artworkKey = key;
            artwork = ARTWORK.get(key);
            if (artwork == null) loadArtwork(key, path, url);
        }
        if (artwork != null) metadata.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, artwork);
        s.setMetadata(metadata.build());
        boolean playing = info.optBoolean("playing");
        long actions = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE | PlaybackState.ACTION_PLAY_PAUSE | PlaybackState.ACTION_STOP;
        if (info.optBoolean("has_next")) actions |= PlaybackState.ACTION_SKIP_TO_NEXT;
        if (info.optBoolean("has_previous")) actions |= PlaybackState.ACTION_SKIP_TO_PREVIOUS;
        if (!info.optBoolean("live")) actions |= PlaybackState.ACTION_SEEK_TO | PlaybackState.ACTION_FAST_FORWARD | PlaybackState.ACTION_REWIND;
        double rate = info.optDouble("rate", 1.0);
        long position = (long) (Math.max(0, info.optDouble("position", 0)) * 1000);
        // Speed zero while paused: the system advances the shown position by the speed.
        s.setPlaybackState(new PlaybackState.Builder()
            .setState(playing ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED, position, playing && rate > 0 ? (float) rate : 0f)
            .setActions(actions).build());
        s.setActive(true);
        DayMediaService.repost(DayBridge.ctx.getApplicationContext());
    }

    private static void clearNowPlaying() {
        nowPlaying = null;
        artworkKey = null;
        artwork = null;
        nowPlayingOwner = new WeakReference<>(null);
        if (session != null) {
            session.setPlaybackState(new PlaybackState.Builder().setState(PlaybackState.STATE_STOPPED, 0, 0f).build());
            session.setMetadata(null);
            // Inactive, so the media keys go to whichever app plays next.
            session.setActive(false);
        }
        DayMediaService.repost(DayBridge.ctx.getApplicationContext());
    }

    /** Decode a local file or fetch a URL on IO, then re-apply the metadata if it is still wanted. */
    private static void loadArtwork(final String key, final String path, final String url) {
        IO.execute(() -> {
            Bitmap bitmap = null;
            try {
                if (!path.isEmpty()) bitmap = decodeFile(path.startsWith("file://") ? Uri.parse(path).getPath() : path);
                else bitmap = decodeBytes(fetch(url));
            } catch (Exception | OutOfMemoryError ignored) {}
            final Bitmap result = bitmap;
            MAIN.post(() -> {
                if (result == null || !key.equals(artworkKey)) return;
                ARTWORK.put(key, result);
                artwork = result;
                if (session != null && nowPlaying != null) applyNowPlaying(session, nowPlaying);
            });
        });
    }
    private static byte[] fetch(String url) throws IOException {
        HttpURLConnection connection = (HttpURLConnection) new URL(url).openConnection();
        connection.setConnectTimeout(10_000);
        connection.setReadTimeout(10_000);
        connection.setInstanceFollowRedirects(true);
        try {
            if (connection.getResponseCode() / 100 != 2) throw new IOException("artwork: HTTP " + connection.getResponseCode());
            InputStream in = connection.getInputStream();
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] chunk = new byte[16384];
            int n;
            while ((n = in.read(chunk)) > 0) {
                if (out.size() + n > ARTWORK_BYTES) throw new IOException("artwork: too large");
                out.write(chunk, 0, n);
            }
            return out.toByteArray();
        } finally { connection.disconnect(); }
    }
    /** Two-pass decode: bounds first, then at a power-of-two sample that fits ARTWORK_SIDE. */
    private static Bitmap decodeFile(String path) {
        BitmapFactory.Options bounds = new BitmapFactory.Options();
        bounds.inJustDecodeBounds = true;
        BitmapFactory.decodeFile(path, bounds);
        return BitmapFactory.decodeFile(path, sampled(bounds));
    }
    private static Bitmap decodeBytes(byte[] bytes) {
        BitmapFactory.Options bounds = new BitmapFactory.Options();
        bounds.inJustDecodeBounds = true;
        BitmapFactory.decodeByteArray(bytes, 0, bytes.length, bounds);
        return BitmapFactory.decodeByteArray(bytes, 0, bytes.length, sampled(bounds));
    }
    private static BitmapFactory.Options sampled(BitmapFactory.Options bounds) {
        BitmapFactory.Options options = new BitmapFactory.Options();
        int side = Math.max(bounds.outWidth, bounds.outHeight);
        options.inSampleSize = 1;
        while (side / options.inSampleSize > ARTWORK_SIDE) options.inSampleSize *= 2;
        return options;
    }

    /** Imperative commands: 0=load (and play), 1=play, 2=pause, 3=stop, 4=volume (`value`),
     *  5=seek, 6=rate, 7=poll, 8=background (JSON in `url`), 9=now playing (JSON in `url`). */
    public static void mediaCommand(View view, int code, String url, double value) {
        Live live = LIVE.get(view);
        if (live == null) {
            return;
        }
        if (code == 9) { setNowPlaying(live, url); return; }
        if (code == 8) {
            try { live.background = new JSONObject(url); updateBackgroundService(); }
            catch (Exception error) { live.report(8, "background:invalid"); }
            return;
        }
        if (code == 2 || code == 3) { live.resumeAfterFocus = false; live.abandonFocus(); }
        if (code == 1 && live.audioOnly && live.prepared && !live.acquireFocus()) return;
        if (code == 7) { poll(live); return; }
        if (code == 6) {
            if (!Double.isFinite(value) || value < .25 || value > 4) { live.report(8, "rate:invalid"); return; }
            live.rate = (float) value;
            MediaPlayer mp = live.audioOnly ? live.player : live.videoPlayer;
            if (mp != null && live.prepared) applyRate(live, mp);
            return;
        }
        if (code == 5) {
            MediaPlayer mp = live.audioOnly ? live.player : live.videoPlayer;
            if (mp == null || !live.prepared || !Double.isFinite(value) || value < 0) { live.report(8, "seek:unsupported"); return; }
            try {
                if (mp.getDuration() <= 0) { live.report(8, "seek:unsupported"); return; }
                int target = (int)Math.min(value * 1000, mp.getDuration());
                live.seeking = true;
                if (android.os.Build.VERSION.SDK_INT >= 26) mp.seekTo(target, MediaPlayer.SEEK_CLOSEST);
                else mp.seekTo(target);
            } catch (IllegalStateException error) { live.seeking = false; live.report(8, "seek:failed"); }
            return;
        }
        if (view instanceof VideoView) {
            VideoView video = (VideoView) view;
            switch (code) {
                case 0:
                    if (url != null && !url.isEmpty()) {
                        live.prepared = false;
                        live.videoPlayer = null;
                        live.report(LOADING, "");
                        video.setVideoURI(Uri.parse(url));
                        video.start();
                    }
                    break;
                case 1:
                    video.start();
                    if (live.prepared) {
                        live.report(PLAYING, "");
                    }
                    break;
                case 2:
                    video.pause();
                    live.report(PAUSED, "");
                    break;
                case 3:
                    video.stopPlayback();
                    live.videoPlayer = null;
                    live.prepared = false;
                    live.report(IDLE, "");
                    break;
                case 4:
                    live.volume = (float) value;
                    if (live.videoPlayer != null && live.prepared) live.applyVolume(live.videoPlayer);
                    break;
                default:
                    break;
            }
            return;
        }
        switch (code) {
            case 0:
                if (url != null && !url.isEmpty()) {
                    loadAudio(live, url, true);
                }
                break;
            case 1:
                if (live.player != null && live.prepared) {
                    live.player.start();
                    live.report(PLAYING, "");
                } else {
                    live.playWhenReady = true;
                }
                break;
            case 2:
                live.playWhenReady = false;
                if (live.player != null && live.prepared) {
                    live.player.pause();
                    live.report(PAUSED, "");
                }
                break;
            case 3:
                // Dropping the player is what lets a live stream's connection go.
                releasePlayer(live);
                live.report(IDLE, "");
                break;
            case 4:
                live.volume = (float) value;
                if (live.player != null) {
                    live.applyVolume(live.player);
                }
                break;
            default:
                break;
        }
    }

    /** Whether the view is a sound-only player (which measures zero). */
    public static boolean isAudioOnly(View view) {
        Live live = LIVE.get(view);
        return live != null && live.audioOnly;
    }

    /** Release the sound-only player; a VideoView releases its own with its window. */
    public static void releaseMedia(View view) {
        Live live = LIVE.remove(view);
        if (live == null) {
            return;
        }
        if (nowPlayingOwner.get() == live) clearNowPlaying();
        live.active = false;
        live.abandonFocus();
        updateBackgroundService();
        if (view instanceof VideoView) {
            live.prepared = false;
            live.videoPlayer = null;
            ((VideoView) view).stopPlayback();
        } else {
            releasePlayer(live);
        }
    }
}
