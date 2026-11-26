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
import android.os.PowerManager;
import android.provider.Settings;
import android.service.notification.StatusBarNotification;
import android.util.Log;
import androidx.core.app.AlarmManagerCompat;
import androidx.core.app.NotificationCompat;
import androidx.core.app.NotificationManagerCompat;
import androidx.core.content.ContextCompat;
import java.util.Arrays;

/**
 * The cooking timers' alarms (#1017), the page gone or the app killed:
 * scheduled by {@link CookTimersPlugin}, received by
 * {@link CookTimerReceiver}, rung by {@link CookTimerRinging}.
 *
 * <p>The ringing is a foreground service, {@link CookTimerRinging}: the
 * alarm ringtone of the phone in a loop on the <em>alarm</em> stream
 * ({@link #alarmAudio}: {@code USAGE_ALARM}, at the alarm volume, heard
 * when the media are muted) and a vibration, until « Arrêter »,
 * {@code stop()} or {@code cancel()} of the page; opening the notification
 * shade does not stop it. Its notification, on channel {@link #CHANNEL},
 * is silent: the service plays the sound.
 *
 * <p>From Android 12 on, Android starts such a service from the
 * background only for an exact alarm ({@link #schedule}: an alarm clock,
 * which needs « Alarmes et rappels » from Android 12 on). Refused, as for
 * an inexact alarm with the app in the background, the timer falls back
 * on a notification that rings itself ({@link #ringByNotification}), on
 * channel {@link #FALLBACK_CHANNEL}: the same sound and vibration,
 * repeated by {@link Notification#FLAG_INSISTENT}; but Android silences
 * it when the member opens the notification shade, the phone unlocked,
 * and, from Android 14 on, a swipe removes it.
 *
 * <p>The notification shows the recipe's name and « Minuteur terminé »,
 * on the lock screen too (decision of 2026-11-25), and opens full screen
 * when Android allows it ({@link CookTimerPlan#fullScreen}), a high-priority
 * notification to touch otherwise. Both open {@link MainActivity} on the
 * page the timer names ({@link #ACTION_OPEN}, {@link #EXTRA_PATH}), over
 * the lock screen while a timer rings (decision of 2026-11-26).
 *
 * <p>Nothing is stored: an alarm lives in {@link AlarmManager} alone, and a
 * restart of the phone, or a force stop of the app, loses it.
 */
final class CookTimers {

    private static final String TAG = "MHome";

    /** The ringing's channel, silent: {@link CookTimerRinging} plays the sound. */
    static final String CHANNEL = "minuteurs";
    /** The fallback's channel, whose notification rings itself. */
    static final String FALLBACK_CHANNEL = "minuteurs-repli";
    /** The broadcast an alarm sends {@link CookTimerReceiver} at the end. */
    static final String ACTION_RING = "fr.watallo.mhome.COOK_TIMER_RING";
    /** The broadcast « Arrêter » sends {@link CookTimerReceiver}. */
    static final String ACTION_STOP = "fr.watallo.mhome.COOK_TIMER_STOP";
    /** The broadcast a swipe of a ringing notification sends: it comes back. */
    static final String ACTION_SHOW = "fr.watallo.mhome.COOK_TIMER_SHOW";
    /** The intent of {@link MainActivity} a touch, or the full screen, opens. */
    static final String ACTION_OPEN = "fr.watallo.mhome.COOK_TIMER_OPEN";
    /** Its extra: the page to open, a path of the site ({@link CookTimerPlan#sitePath}). */
    static final String EXTRA_PATH = "path";
    static final String EXTRA_TITLE = "title";
    static final String EXTRA_TEXT = "text";
    /** The scheme of the data that tells one timer's intents from another's. */
    private static final String SCHEME = "mhome-cook-timer";
    /** A timer's notification: tag {@code TAG_PREFIX + id}, number {@link #NOTIFICATION_ID}. */
    static final String TAG_PREFIX = "cook-timer:";
    static final int NOTIFICATION_ID = 1;
    /** The vibration, repeated with the sound: 0.8 s on, 0.6 s off. */
    static final long[] VIBRATION = { 0, 800, 600, 800, 600 };
    /** How long the CPU stays awake for the ringing service to start, in ms. */
    private static final long WAKE_MS = 10_000;

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
     * status bar shows, and from which Android lets {@link CookTimerRinging}
     * start. Inexact otherwise: Android may then ring it late by up to
     * three quarters of the time left, an hour at most (Android 12 on), and
     * later still in deep sleep. Returns whether it is exact.
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

    /**
     * Rings the timer {@code alarm} names: {@link CookTimerRinging}, or,
     * when Android refuses to start it, {@link #ringByNotification}. Also
     * the latter when the ringing's notification cannot show (notifications
     * refused, or its channel turned off): a sound without its « Arrêter »
     * would not stop.
     */
    static void ring(Context context, String id, Intent alarm) {
        String title = alarm.getStringExtra(EXTRA_TITLE);
        String text = alarm.getStringExtra(EXTRA_TEXT);
        String path = alarm.getStringExtra(EXTRA_PATH);
        if (!canShow(context, CHANNEL)) {
            ringByNotification(context, id, title, text, path);
            return;
        }
        PowerManager power = context.getSystemService(PowerManager.class);
        if (power != null) {
            // The alarm keeps the CPU awake until onReceive returns only.
            PowerManager.WakeLock awake = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "mhome:cook-timer");
            awake.setReferenceCounted(false);
            awake.acquire(WAKE_MS);
        }
        Intent ringing = new Intent(context, CookTimerRinging.class)
            .setAction(ACTION_RING)
            .setData(timer(id))
            .putExtra(EXTRA_TITLE, title)
            .putExtra(EXTRA_TEXT, text)
            .putExtra(EXTRA_PATH, path);
        try {
            ContextCompat.startForegroundService(context, ringing);
        } catch (IllegalStateException | SecurityException e) {
            // ForegroundServiceStartNotAllowedException, from Android 12 on.
            Log.w(TAG, "no ringing service for cooking timer " + id + ": the notification rings", e);
            ringByNotification(context, id, title, text, path);
        }
    }

    /** Whether the app's notifications show, and those of {@code channel}. */
    private static boolean canShow(Context context, String channel) {
        NotificationManagerCompat notifications = NotificationManagerCompat.from(context);
        if (!notifications.areNotificationsEnabled()) {
            return false;
        }
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return true;
        }
        NotificationChannel shown = notifications.getNotificationChannel(channel);
        return shown == null || shown.getImportance() != NotificationManager.IMPORTANCE_NONE;
    }

    /**
     * The fallback ringing: timer {@code id}'s notification, on
     * {@link #FALLBACK_CHANNEL}, insistent: its sound and vibration repeat
     * until it goes, or until the member opens the notification shade.
     */
    static void ringByNotification(Context context, String id, String title, String text, String path) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        Notification notification = notification(context, FALLBACK_CHANNEL, id, title, text, path)
            // Below Android 8, which has no channel: the same sound and vibration.
            .setSound(Settings.System.DEFAULT_ALARM_ALERT_URI, AudioManager.STREAM_ALARM)
            .setVibrate(VIBRATION)
            .build();
        notification.flags |= Notification.FLAG_INSISTENT;
        manager.notify(TAG_PREFIX + id, NOTIFICATION_ID, notification);
    }

    /**
     * Timer {@code id}'s notification, on {@code channel}: the recipe's
     * name, « Minuteur terminé », « Arrêter », full screen when Android
     * allows it. Ongoing and not cancelled by a touch, which opens the
     * page and leaves the timer ringing.
     */
    static NotificationCompat.Builder notification(Context context, String channel, String id, String title, String text, String path) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        createChannels(context, manager);
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
        NotificationCompat.Builder builder = new NotificationCompat.Builder(context, channel)
            .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
            .setContentTitle(CookTimerPlan.title(title))
            .setContentText(CookTimerPlan.text(text))
            .setCategory(NotificationCompat.CATEGORY_ALARM)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setOngoing(true)
            .setAutoCancel(false)
            .setContentIntent(open)
            .addAction(0, context.getString(R.string.cook_timer_stop), stop);
        if (fullScreen) {
            builder.setFullScreenIntent(open, true);
        }
        return builder;
    }

    /** Stops timer {@code id}'s ringing, if it rings. */
    static void stop(Context context, String id) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null) {
            manager.cancel(TAG_PREFIX + id, NOTIFICATION_ID);
        }
        tellRinging(context, ACTION_STOP, id);
    }

    /** Stops every timer that rings. */
    static void stopAll(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null) {
            for (StatusBarNotification shown : manager.getActiveNotifications()) {
                if (isTimers(shown)) {
                    manager.cancel(shown.getTag(), shown.getId());
                }
            }
        }
        tellRinging(context, ACTION_STOP, null);
    }

    /** Whether a timer rings: the ringing service, or a fallback notification. */
    static boolean ringing(Context context) {
        if (CookTimerRinging.running) {
            return true;
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null) {
            for (StatusBarNotification shown : manager.getActiveNotifications()) {
                if (isTimers(shown)) {
                    return true;
                }
            }
        }
        return false;
    }

    private static boolean isTimers(StatusBarNotification shown) {
        return shown.getTag() != null && shown.getTag().startsWith(TAG_PREFIX);
    }

    /**
     * Sends {@code action} (for timer {@code id}, or all when null) to
     * {@link CookTimerRinging} if it runs. Android lets an app with a
     * foreground service start a service; one that just stopped may make
     * it refuse, and then there is nothing left to tell.
     */
    static void tellRinging(Context context, String action, String id) {
        if (!CookTimerRinging.running) {
            return;
        }
        Intent intent = new Intent(context, CookTimerRinging.class).setAction(action);
        if (id != null) {
            intent.setData(timer(id));
        }
        try {
            context.startService(intent);
        } catch (IllegalStateException e) {
            Log.w(TAG, "the cooking timers' ringing is gone", e);
        }
    }

    /** The timer an intent of {@link CookTimerReceiver} or {@link CookTimerRinging} names, or null. */
    static String idOf(Intent intent) {
        Uri data = intent.getData();
        return data != null && SCHEME.equals(data.getScheme()) ? data.getSchemeSpecificPart() : null;
    }

    /**
     * The timers' two channels, created once; later calls only update their
     * names and descriptions. Their sound and vibration are fixed at
     * creation, as Android keeps them, the member's to change.
     */
    static void createChannels(Context context, NotificationManager manager) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        NotificationChannel ringing = new NotificationChannel(
            CHANNEL,
            context.getString(R.string.cook_timer_channel_name),
            NotificationManager.IMPORTANCE_HIGH
        );
        ringing.setDescription(context.getString(R.string.cook_timer_channel_description));
        // Silent: CookTimerRinging plays the sound and the vibration.
        ringing.setSound(null, null);
        ringing.enableVibration(false);
        ringing.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
        NotificationChannel fallback = new NotificationChannel(
            FALLBACK_CHANNEL,
            context.getString(R.string.cook_timer_fallback_channel_name),
            NotificationManager.IMPORTANCE_HIGH
        );
        fallback.setDescription(context.getString(R.string.cook_timer_fallback_channel_description));
        fallback.setSound(Settings.System.DEFAULT_ALARM_ALERT_URI, alarmAudio());
        fallback.enableVibration(true);
        fallback.setVibrationPattern(VIBRATION);
        fallback.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
        manager.createNotificationChannels(Arrays.asList(ringing, fallback));
    }

    /** The data of timer {@code id}'s intents: alarms and stops of two timers differ. */
    static Uri timer(String id) {
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
