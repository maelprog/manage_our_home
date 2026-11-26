package fr.watallo.mhome;

import android.app.Notification;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.media.MediaPlayer;
import android.net.Uri;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.provider.Settings;
import android.util.Log;
import androidx.core.app.ServiceCompat;
import java.io.IOException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;

/**
 * The ringing of the cooking timers that are over (#1017): a foreground
 * service that plays the alarm ringtone of the phone in a loop, on the
 * alarm stream ({@link CookTimers#alarmAudio}), and vibrates, until the
 * last of them stops: « Arrêter » ({@link CookTimers#ACTION_STOP}), or
 * {@code stop()} or {@code cancel()} of the page. Opening the notification
 * shade, or touching the notification, leaves it ringing.
 *
 * <p>One sound for all: the oldest timer's notification is the service's
 * own ({@link #FOREGROUND_ID}), the others' are tagged
 * ({@link CookTimers#TAG_PREFIX}), all on the silent channel
 * {@link CookTimers#CHANNEL}. A swipe of one, which Android allows from
 * Android 14 on, brings it back ({@link CookTimers#ACTION_SHOW}): its
 * « Arrêter » is the way to stop it.
 *
 * <p>Started by {@link CookTimers#ring} only; type {@code mediaPlayback}
 * (AndroidManifest.xml). Not sticky: killed, it does not come back.
 */
public class CookTimerRinging extends Service {

    private static final String TAG = "MHome";
    /** The service's own notification, with no tag. */
    private static final int FOREGROUND_ID = 1017;

    /** True while the service runs; read by {@link CookTimers} on any thread. */
    static volatile boolean running;

    /** The timers that ring, by id, oldest first: the intent that started each. */
    private final LinkedHashMap<String, Intent> timers = new LinkedHashMap<>();
    private MediaPlayer player;
    private Vibrator vibrator;

    @Override
    public void onCreate() {
        super.onCreate();
        running = true;
    }

    @Override
    public void onDestroy() {
        running = false;
        silence();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? null : intent.getAction();
        String id = intent == null ? null : CookTimers.idOf(intent);
        if (CookTimers.ACTION_RING.equals(action) && CookTimerPlan.validId(id)) {
            timers.remove(id);
            timers.put(id, intent);
        } else if (CookTimers.ACTION_STOP.equals(action)) {
            if (id == null) {
                timers.clear();
            } else {
                timers.remove(id);
            }
        }
        if (timers.isEmpty()) {
            ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE);
            stopSelf();
            return START_NOT_STICKY;
        }
        show();
        sound();
        return START_NOT_STICKY;
    }

    /**
     * The oldest timer as the service's notification, the others tagged.
     * Each alerts (heads-up, full screen) when it first shows, and not when
     * shown again unchanged ({@code setOnlyAlertOnce}): a timer that starts
     * ringing is a new notification, one that stops leaves the others quiet.
     */
    private void show() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        List<String> ids = new ArrayList<>(timers.keySet());
        String oldest = ids.get(0);
        ServiceCompat.startForeground(this, FOREGROUND_ID, notification(oldest), ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        manager.cancel(CookTimers.TAG_PREFIX + oldest, CookTimers.NOTIFICATION_ID);
        for (String id : ids.subList(1, ids.size())) {
            manager.notify(CookTimers.TAG_PREFIX + id, CookTimers.NOTIFICATION_ID, notification(id));
        }
    }

    private Notification notification(String id) {
        Intent ring = timers.get(id);
        PendingIntent show = PendingIntent.getBroadcast(
            this,
            0,
            new Intent(this, CookTimerReceiver.class).setAction(CookTimers.ACTION_SHOW).setData(CookTimers.timer(id)),
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
        return CookTimers.notification(
            this,
            CookTimers.CHANNEL,
            id,
            ring.getStringExtra(CookTimers.EXTRA_TITLE),
            ring.getStringExtra(CookTimers.EXTRA_TEXT),
            ring.getStringExtra(CookTimers.EXTRA_PATH)
        )
            .setDeleteIntent(show)
            // Shown again at each change: it alerts once, the service rings.
            .setOnlyAlertOnce(true)
            .build();
    }

    /** Starts the sound and the vibration, unless they are on. */
    private void sound() {
        if (player == null) {
            player = play(Settings.System.DEFAULT_ALARM_ALERT_URI);
        }
        if (player == null) {
            // No alarm ringtone chosen on the phone: its notification sound.
            player = play(Settings.System.DEFAULT_NOTIFICATION_URI);
        }
        if (vibrator == null) {
            vibrator = getSystemService(Vibrator.class);
            if (vibrator != null && vibrator.hasVibrator()) {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    vibrator.vibrate(VibrationEffect.createWaveform(CookTimers.VIBRATION, 0), CookTimers.alarmAudio());
                } else {
                    vibrator.vibrate(CookTimers.VIBRATION, 0, CookTimers.alarmAudio());
                }
            }
        }
    }

    /** {@code sound} in a loop on the alarm stream, or null if it cannot play. */
    private MediaPlayer play(Uri sound) {
        MediaPlayer played = new MediaPlayer();
        try {
            played.setAudioAttributes(CookTimers.alarmAudio());
            played.setDataSource(this, sound);
            played.setLooping(true);
            played.setWakeMode(this, PowerManager.PARTIAL_WAKE_LOCK);
            played.prepare();
            played.start();
            return played;
        } catch (IOException | RuntimeException e) {
            Log.w(TAG, "cannot play " + sound + " for a cooking timer", e);
            played.release();
            return null;
        }
    }

    private void silence() {
        if (player != null) {
            player.stop();
            player.release();
            player = null;
        }
        if (vibrator != null) {
            vibrator.cancel();
            vibrator = null;
        }
    }
}
