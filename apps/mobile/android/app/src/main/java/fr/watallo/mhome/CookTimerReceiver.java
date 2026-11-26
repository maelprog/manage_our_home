package fr.watallo.mhome;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * Where a cooking timer's alarm ends (#1017), with no page and the app
 * killed: {@link CookTimers#ACTION_RING} rings it, « Arrêter »
 * ({@link CookTimers#ACTION_STOP}) stops it without opening the app. Not
 * exported: only the app's own pending intents reach it.
 */
public class CookTimerReceiver extends BroadcastReceiver {

    @Override
    public void onReceive(Context context, Intent intent) {
        String id = CookTimers.idOf(intent);
        if (!CookTimerPlan.validId(id)) {
            return;
        }
        if (CookTimers.ACTION_RING.equals(intent.getAction())) {
            CookTimers.ring(
                context,
                id,
                intent.getStringExtra(CookTimers.EXTRA_TITLE),
                intent.getStringExtra(CookTimers.EXTRA_TEXT),
                intent.getStringExtra(CookTimers.EXTRA_PATH)
            );
        } else if (CookTimers.ACTION_STOP.equals(intent.getAction())) {
            CookTimers.stop(context, id);
        }
    }
}
