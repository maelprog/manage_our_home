package fr.watallo.mhome;

import androidx.annotation.NonNull;
import com.capacitorjs.plugins.pushnotifications.MessagingService;
import com.google.firebase.messaging.RemoteMessage;

/**
 * Where Firebase Cloud Messaging delivers the server's pushes (#1004), in
 * place of the Push Notifications plugin's own service (removed from the
 * merged manifest), which it extends: the page still hears of a new token
 * and of a message received with the app open.
 *
 * <p>The plugin shows nothing of a data message, and nothing at all with
 * the app closed, when no page is there to: the server sends data only
 * ({@code kind}, {@code entries}), so this service shows it itself, open
 * or closed ({@link PushNotices}). A reminder never: the app schedules
 * them on the phone (#668), the API sends none by FCM, and one that came
 * anyway would show twice.
 */
public class PushMessagingService extends MessagingService {

    @Override
    public void onMessageReceived(@NonNull RemoteMessage message) {
        super.onMessageReceived(message);
        String kind = message.getData().get("kind");
        if (PushNotices.REMINDER.equals(kind)) {
            return;
        }
        PushNotices.show(this, message.getMessageId(), kind, message.getData().get("entries"));
    }
}
