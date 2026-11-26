package fr.watallo.mhome;

import android.app.AlarmManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.media.AudioAttributes;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.provider.Settings;
import android.service.notification.StatusBarNotification;
import androidx.core.app.AlarmManagerCompat;
import androidx.core.app.NotificationCompat;

/**
 * The cooking timers' alarms (#1017), the page gone or the app killed:
 * scheduled by {@link CookTimersPlugin}, rung by {@link CookTimerReceiver}.
 *
 * <p>The ringing is the notification itself, on a channel of its own,
 * {@link #CHANNEL}: importance high, the alarm ringtone of the phone on the
 * <em>alarm</em> stream ({@link #alarmAudio}: {@code USAGE_ALARM}, at the
 * alarm volume, heard when the media are muted), a vibration, and
 * {@link Notification#FLAG_INSISTENT}, which repeats the sound and the
 * vibration until the notification goes: « Arrêter », {@code stop()} or
 * {@code cancel()} of the page. Android plays it, not this process: it
 * rings with the app killed, and no foreground service is needed, which
 * Android would refuse to start from an inexact alarm. Android also stops
 * an insistent sound when the member opens the notification shade, the
 * phone unlocked; the notification, and its « Arrêter », stay.
 *
 * <p>The notification shows the recipe's name and « Minuteur terminé »,
 * on the lock screen too (decision of 2026-11-25), and opens full screen
 * when Android allows it ({@link CookTimerPlan#fullScreen}), a high-priority
 * notification to touch otherwise. Both open {@link MainActivity} on the
 * page the timer names ({@link #ACTION_OPEN}, {@link #EXTRA_PATH}). The
 * activity does not show over the lock screen: the site, and the session,
 * stay behind it.
 *
 * <p>Nothing is stored: an alarm lives in {@link AlarmManager} alone, and a
 * restart of the phone, or a force stop of the app, loses it.
 */
final class CookTimers {

    /** The timers' channel, apart from the reminders' and the pushes'. */
    static final String CHANNEL = "minuteurs";
    /** The broadcast an alarm sends {@link CookTimerReceiver} at the end. */
    static final String ACTION_RING = "fr.watallo.mhome.COOK_TIMER_RING";
    /** The broadcast « Arrêter » sends {@link CookTimerReceiver}. */
    static final String ACTION_STOP = "fr.watallo.mhome.COOK_TIMER_STOP";
    /** The intent of {@link MainActivity} a touch, or the full screen, opens. */
    static final String ACTION_OPEN = "fr.watallo.mhome.COOK_TIMER_OPEN";
    /** Its extra: the page to open, a path of the site ({@link CookTimerPlan#sitePath}). */
    static final String EXTRA_PATH = "path";
    static final String EXTRA_TITLE = "title";
    static final String EXTRA_TEXT = "text";
    /** The scheme of the data that tells one timer's intents from another's. */
    private static final String SCHEME = "mhome-cook-timer";
    /** A timer's notification: tag {@code TAG_PREFIX + id}, number {@link #NOTIFICATION_ID}. */
    private static final String TAG_PREFIX = "cook-timer:";
    private static final int NOTIFICATION_ID = 1;
    /** The vibration, repeated with the sound: 0.8 s on, 0.6 s off. */
    private static final long[] VIBRATION = { 0, 800, 600, 800, 600 };

    private CookTimers() {}

    /** The ringing's audio: an alarm, on the alarm stream. */
    static AudioAttributes alarmAudio() {
        return new AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_ALARM)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build();
    }

    /** The origin, {@code https://host[:port]}, of the address the app opens. */
    static String origin(String appUrl) {
        if (appUrl == null) {
            return null;
        }
        Uri uri = Uri.parse(appUrl);
        return uri.getScheme() == null || uri.getEncodedAuthority() == null ? null : uri.getScheme() + "://" + uri.getEncodedAuthority();
    }

    /**
     * Schedules timer {@code id} to ring at {@code at} (ms since the epoch),
     * in place of any earlier alarm of the same id. Exact when Android
     * allows it ({@link CookTimerPlan#exact}): an alarm clock, which the
     * status bar shows; inexact otherwise, a few minutes late at worst.
     * Returns whether it is exact.
     */
    static boolean schedule(Context context, String id, long at, String title, String text, String path) {
        AlarmManager alarms = context.getSystemService(AlarmManager.class);
        Intent ring = new Intent(context, CookTimerReceiver.class)
            .setAction(ACTION_RING)
            .setData(timer(id))
            .putExtra(EXTRA_TITLE, title)
            .putExtra(EXTRA_TEXT, text);
        if (path != null) {
            ring.putExtra(EXTRA_PATH, path);
        }
        PendingIntent operation = PendingIntent.getBroadcast(
            context,
            0,
            ring,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
        boolean exact = CookTimerPlan.exact(
            Build.VERSION.SDK_INT,
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && alarms.canScheduleExactAlarms()
        );
        if (exact) {
            AlarmManagerCompat.setAlarmClock(alarms, at, open(context, id, path), operation);
        } else {
            AlarmManagerCompat.setAndAllowWhileIdle(alarms, AlarmManager.RTC_WAKEUP, at, operation);
        }
        return exact;
    }

    /** Cancels timer {@code id}: its alarm if it is to come, its ringing if it rings. */
    static void cancel(Context context, String id) {
        PendingIntent operation = PendingIntent.getBroadcast(
            context,
            0,
            new Intent(context, CookTimerReceiver.class).setAction(ACTION_RING).setData(timer(id)),
            PendingIntent.FLAG_NO_CREATE | PendingIntent.FLAG_IMMUTABLE
        );
        if (operation != null) {
            context.getSystemService(AlarmManager.class).cancel(operation);
            operation.cancel();
        }
        stop(context, id);
    }

    /** Rings timer {@code id}: posts its insistent notification. */
    static void ring(Context context, String id, String title, String text, String path) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        createChannel(context, manager);
        boolean fullScreen = CookTimerPlan.fullScreen(
            Build.VERSION.SDK_INT,
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE && manager.canUseFullScreenIntent()
        );
        PendingIntent open = open(context, id, path);
        PendingIntent stop = PendingIntent.getBroadcast(
            context,
            0,
            new Intent(context, CookTimerReceiver.class).setAction(ACTION_STOP).setData(timer(id)),
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
        NotificationCompat.Builder builder = new NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setContentTitle(CookTimerPlan.title(title))
            .setContentText(CookTimerPlan.text(text))
            .setCategory(NotificationCompat.CATEGORY_ALARM)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            // Below Android 8, which has no channel: the same sound and vibration.
            .setSound(Settings.System.DEFAULT_ALARM_ALERT_URI, AudioManager.STREAM_ALARM)
            .setVibrate(VIBRATION)
            // Rings until « Arrêter »: neither a touch nor a swipe removes it.
            .setOngoing(true)
            .setAutoCancel(false)
            .setContentIntent(open)
            .addAction(0, context.getString(R.string.cook_timer_stop), stop);
        if (fullScreen) {
            builder.setFullScreenIntent(open, true);
        }
        Notification notification = builder.build();
        notification.flags |= Notification.FLAG_INSISTENT;
        manager.notify(TAG_PREFIX + id, NOTIFICATION_ID, notification);
    }

    /** Stops timer {@code id}'s ringing, if it rings. */
    static void stop(Context context, String id) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null) {
            manager.cancel(TAG_PREFIX + id, NOTIFICATION_ID);
        }
    }

    /** Stops every timer that rings. */
    static void stopAll(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        for (StatusBarNotification shown : manager.getActiveNotifications()) {
            if (shown.getTag() != null && shown.getTag().startsWith(TAG_PREFIX)) {
                manager.cancel(shown.getTag(), shown.getId());
            }
        }
    }

    /** The timer an intent of {@link CookTimerReceiver} names, or null. */
    static String idOf(Intent intent) {
        Uri data = intent.getData();
        return data != null && SCHEME.equals(data.getScheme()) ? data.getSchemeSpecificPart() : null;
    }

    /**
     * The timers' channel, created once; later calls only update its name
     * and description. Its sound and vibration are fixed at creation, as
     * Android keeps them, the member's to change.
     */
    static void createChannel(Context context, NotificationManager manager) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
            CHANNEL,
            context.getString(R.string.cook_timer_channel_name),
            NotificationManager.IMPORTANCE_HIGH
        );
        channel.setDescription(context.getString(R.string.cook_timer_channel_description));
        channel.setSound(Settings.System.DEFAULT_ALARM_ALERT_URI, alarmAudio());
        channel.enableVibration(true);
        channel.setVibrationPattern(VIBRATION);
        channel.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
        manager.createNotificationChannel(channel);
    }

    /** The data of timer {@code id}'s intents: alarms and stops of two timers differ. */
    private static Uri timer(String id) {
        return Uri.fromParts(SCHEME, id, null);
    }

    /**
     * What a touch, or the full screen, opens: {@link MainActivity} on
     * {@code path}, or on the page it shows when there is none. No data
     * (Capacitor would read it as an address to open): a category of the
     * timer's own tells two timers apart, as {@code id.hashCode()} would
     * not ("Aa" and "BB" share one), and an explicit intent ignores it.
     */
    private static PendingIntent open(Context context, String id, String path) {
        Intent open = new Intent(context, MainActivity.class).setAction(ACTION_OPEN).addCategory(TAG_PREFIX + id);
        if (path != null) {
            open.putExtra(EXTRA_PATH, path);
        }
        return PendingIntent.getActivity(
            context,
            0,
            open,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
    }
}
