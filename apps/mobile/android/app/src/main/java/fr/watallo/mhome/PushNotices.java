package fr.watallo.mhome;

import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import androidx.core.app.NotificationCompat;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * The notification a push of the server shows (#1004), app closed or open.
 *
 * <p>Its texts are fixed, the ones apps/web/src/sw.js shows in a browser
 * for the same kind ({@link #TEXTS}, kept equal to sw.js's by
 * scripts/check-shell.test.mjs), the neutral pair for a kind it does not
 * know: nothing of the message is shown, the message carries no text
 * anyway (apps/api, {@code notifications::fcm::message_body}). Never an
 * event's title, an author, an amount or a category on the lock screen.
 *
 * <p>On a channel of its own, {@link #CHANNEL}, apart from the reminders
 * the Local Notifications plugin shows on its {@code default} one: the
 * member can silence one and keep the other in Android's settings.
 *
 * <p>A touch opens {@link MainActivity} with the push's kind and entries
 * as extras, and {@code google.message_id}: the Push Notifications plugin
 * hands such an intent to the page ({@code pushNotificationActionPerformed}),
 * at a cold start too, which marks the entries read and opens the kind's
 * page (apps/web/src/local-reminders.js).
 */
final class PushNotices {

    /** The channel of the pushes. Not {@code default}, the reminders'. */
    static final String CHANNEL = "pushes";
    /** The reminders' kind: scheduled on the phone (#668), never shown from a push. */
    static final String REMINDER = "agenda_reminder";
    /** The extras of the intent a touch opens, as the page reads them. */
    static final String KIND = "kind";
    static final String ENTRIES = "entries";
    /** The action of the intent a touch opens. */
    static final String ACTION_TOUCH = "fr.watallo.mhome.PUSH_TOUCHED";
    /** The notifications of this class, apart from the reminders' (no tag). */
    private static final String TAG = "push";

    /** Kind, title, text: apps/web/src/sw.js's TEXTS, the same words. */
    static final String[][] TEXTS = {
        { "agenda_reminder", "Rappel d'un événement à venir", "Ouvrez l'agenda pour le voir." },
        { "budget_alert", "Alerte de budget", "Les dépenses du mois ont franchi un seuil du budget. Ouvrez le budget pour le voir." },
        { "stock_expiry", "Produits bientôt périmés", "Des produits du stock arrivent à leur date. Ouvrez les stocks pour les voir." },
        { "agenda_change", "Agenda de la famille modifié", "Un événement a été ajouté, modifié ou supprimé. Ouvrez l'agenda pour le voir." },
    };

    /** sw.js's NEUTRAL: a kind this APK does not know. */
    static final String[] NEUTRAL = { "Nouvelle notification", "Ouvrez l'application pour la voir." };

    private static final AtomicInteger NEXT_ID = new AtomicInteger((int) (System.currentTimeMillis() & 0xffffff));

    private PushNotices() {}

    /** The title and the text of a push of {@code kind}. */
    static String[] textsFor(String kind) {
        for (String[] row : TEXTS) {
            if (row[0].equals(kind)) {
                return new String[] { row[1], row[2] };
            }
        }
        return NEUTRAL;
    }

    /**
     * Shows the push of {@code kind} naming {@code entries} (ids separated
     * by commas, or null). Each push its own notification, as in a
     * browser: three changes, three notifications.
     */
    static void show(Context context, String messageId, String kind, String entries) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationChannel channel = new NotificationChannel(
                CHANNEL,
                context.getString(R.string.push_channel_name),
                NotificationManager.IMPORTANCE_DEFAULT
            );
            channel.setDescription(context.getString(R.string.push_channel_description));
            // Created once; later calls only update its name and description.
            manager.createNotificationChannel(channel);
        }
        int id = NEXT_ID.incrementAndGet();
        // An action of its own: neither the launcher's nor « Changer de
        // site » (MainActivity.ACTION_CHANGE_SITE), and no data for
        // Capacitor to read as an address to open.
        Intent open = new Intent(context, MainActivity.class)
            .setAction(ACTION_TOUCH)
            .putExtra("google.message_id", messageId != null ? messageId : "mhome-" + id)
            .putExtra(KIND, kind != null ? kind : "");
        if (entries != null) {
            open.putExtra(ENTRIES, entries);
        }
        PendingIntent touch = PendingIntent.getActivity(
            context,
            id,
            open,
            PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE
        );
        String[] text = textsFor(kind);
        manager.notify(
            TAG,
            id,
            new NotificationCompat.Builder(context, CHANNEL)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle(text[0])
                .setContentText(text[1])
                .setStyle(new NotificationCompat.BigTextStyle().bigText(text[1]))
                .setContentIntent(touch)
                .setAutoCancel(true)
                .build()
        );
    }
}
