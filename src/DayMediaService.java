// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
package dev.daybrite.day.piece.media;

import android.app.Service;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Intent;
import android.os.IBinder;
import android.os.Build;

/** Process lifetime for explicitly opted-in audio. Player ownership stays in AudioSession. */
public final class DayMediaService extends Service {
    static final String CHANNEL = "day.media.playback";
    static final int NOTIFICATION = 0x444159;
    @Override public IBinder onBind(Intent intent) { return null; }
    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent == null || !DayMedia.hasBackgroundPlayback()) { stopSelf(); return START_NOT_STICKY; }
        String title = intent.getStringExtra("title");
        String description = intent.getStringExtra("description");
        String channel = intent.getStringExtra("channel");
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (Build.VERSION.SDK_INT >= 26) manager.createNotificationChannel(
            new NotificationChannel(CHANNEL, channel, NotificationManager.IMPORTANCE_LOW));
        Notification.Builder builder = Build.VERSION.SDK_INT >= 26
            ? new Notification.Builder(this, CHANNEL) : new Notification.Builder(this);
        builder.setSmallIcon(android.R.drawable.ic_media_play).setContentTitle(title)
            .setContentText(description).setOngoing(true).setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_TRANSPORT);
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (launch != null) builder.setContentIntent(PendingIntent.getActivity(this, 0, launch,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        startForeground(NOTIFICATION, builder.build());
        return START_NOT_STICKY;
    }
}
