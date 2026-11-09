// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
package dev.daybrite.day.piece.media;

import android.app.Service;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.drawable.Icon;
import android.media.session.MediaSession;
import android.os.IBinder;
import android.os.Build;
import org.json.JSONObject;

/** Process lifetime for explicitly opted-in audio. Player ownership stays in AudioSession. */
public final class DayMediaService extends Service {
    static final String CHANNEL = "day.media.playback";
    static final int NOTIFICATION = 0x444159;
    /** The extra naming a transport command a notification button sends back (a REMOTE wire form). */
    private static final String COMMAND = "command";
    private static boolean running;
    // All calls and lifecycle callbacks run on the main thread. A fast source replacement
    // can stop playback before Android delivers the foreground start request. Do not cancel
    // that service until it has fulfilled its startForeground obligation.
    private static int pendingStarts;
    static void requestStart(Context context, Intent intent) {
        pendingStarts++;
        try {
            if (Build.VERSION.SDK_INT >= 26) context.startForegroundService(intent);
            else context.startService(intent);
        } catch (RuntimeException error) {
            pendingStarts--;
            throw error;
        }
    }
    static void requestStop(Context context) {
        if (pendingStarts == 0) context.stopService(new Intent(context, DayMediaService.class));
    }
    private static String title = "", description = "";
    @Override public IBinder onBind(Intent intent) { return null; }
    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent == null) { stopSelf(); return START_NOT_STICKY; }
        // Notification commands do not originate from requestStart.
        String command = intent.getStringExtra(COMMAND);
        if (command == null && pendingStarts > 0) pendingStarts--;
        if (intent.hasExtra("title")) {
            title = intent.getStringExtra("title");
            description = intent.getStringExtra("description");
            String channel = intent.getStringExtra("channel");
            if (Build.VERSION.SDK_INT >= 26) getSystemService(NotificationManager.class).createNotificationChannel(
                new NotificationChannel(CHANNEL, channel, NotificationManager.IMPORTANCE_LOW));
        }
        running = true;
        // Promote even if the source stopped while this intent was in flight. Stopping first
        // can leave Android's foreground-start timer armed and crash the entire application.
        startForeground(NOTIFICATION, build(this));
        if (command != null) DayMedia.remote(command);
        if (!DayMedia.hasBackgroundPlayback() && pendingStarts == 0) {
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelfResult(startId);
        }
        return START_NOT_STICKY;
    }
    @Override public void onDestroy() { running = false; super.onDestroy(); }

    /** Post the notification again (same id) when the now-playing info changes under it. */
    static void repost(Context context) {
        if (!running) return;
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null) manager.notify(NOTIFICATION, build(context));
    }

    /** The plain opt-in notification, or a MediaStyle one bound to the session while the player
     *  that published now-playing info is the one in the background. */
    private static Notification build(Context context) {
        Notification.Builder builder = Build.VERSION.SDK_INT >= 26
            ? new Notification.Builder(context, CHANNEL) : new Notification.Builder(context);
        builder.setSmallIcon(android.R.drawable.ic_media_play).setOngoing(true).setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_TRANSPORT);
        JSONObject info = DayMedia.nowPlaying();
        MediaSession.Token token = DayMedia.sessionToken();
        if (info != null && token != null) {
            String itemTitle = DayMedia.text(info, "title");
            String artist = DayMedia.text(info, "artist");
            builder.setContentTitle(itemTitle.isEmpty() ? title : itemTitle)
                .setContentText(artist.isEmpty() ? description : artist);
            Bitmap artwork = DayMedia.artwork();
            if (artwork != null) builder.setLargeIcon(artwork);
            boolean playing = info.optBoolean("playing");
            int count = 0;
            if (info.optBoolean("has_previous")) {
                builder.addAction(action(context, android.R.drawable.ic_media_previous, "Previous", "previous"));
                count++;
            }
            builder.addAction(action(context, playing ? android.R.drawable.ic_media_pause : android.R.drawable.ic_media_play,
                playing ? "Pause" : "Play", "toggle"));
            count++;
            if (info.optBoolean("has_next")) {
                builder.addAction(action(context, android.R.drawable.ic_media_next, "Next", "next"));
                count++;
            }
            int[] compact = new int[count];
            for (int i = 0; i < count; i++) compact[i] = i;
            builder.setStyle(new Notification.MediaStyle().setMediaSession(token).setShowActionsInCompactView(compact));
        } else builder.setContentTitle(title).setContentText(description);
        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        if (launch != null) builder.setContentIntent(PendingIntent.getActivity(context, 0, launch,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        return builder.build();
    }

    /** A transport button: a service intent carrying the command, handled in onStartCommand. */
    private static Notification.Action action(Context context, int icon, String label, String command) {
        Intent intent = new Intent(context, DayMediaService.class)
            .setAction("dev.daybrite.day.piece.media.remote." + command).putExtra(COMMAND, command);
        PendingIntent pending = PendingIntent.getService(context, command.hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Action.Builder(Icon.createWithResource(context, icon), label, pending).build();
    }
}
