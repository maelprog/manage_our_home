package fr.watallo.mhome;

import androidx.core.app.NotificationManagerCompat;
import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;

/**
 * The cooking timers' native alarm (#1017), for the page:
 * {@code window.Capacitor.Plugins.CookTimers}. Registered by
 * {@link MainActivity} on every page.
 *
 * <ul>
 *   <li>{@code schedule({ id, endAt, title, text, url })}: timer {@code id}
 *       (1 to 64 of {@code A-Z a-z 0-9 _ -}) rings at {@code endAt} (ms
 *       since the epoch), in place of an earlier one of the same id;
 *       {@code title} is the recipe's name, {@code text} an optional label
 *       after « Minuteur terminé », {@code url} the page a touch opens, a
 *       path or an address of the site open. Resolves
 *       {@code { scheduled, exact, notifications }}: {@code scheduled}
 *       false for a timer over by more than a minute (nothing rings, an
 *       earlier alarm of the id is cancelled), {@code exact} false when
 *       Android gives no exact alarm (a few minutes late at worst),
 *       {@code notifications} false when notifications are refused to the
 *       app (the timer then rings nothing: the ringing is its notification).
 *   <li>{@code cancel({ id })}: the timer neither rings later nor goes on
 *       ringing.
 *   <li>{@code stop()}: every timer that rings stops.
 * </ul>
 *
 * The scheduling and the ringing: {@link CookTimers}.
 */
@CapacitorPlugin(name = "CookTimers")
public class CookTimersPlugin extends Plugin {

    @PluginMethod
    public void schedule(PluginCall call) {
        String id = call.getString("id");
        if (!CookTimerPlan.validId(id)) {
            call.reject("id: 1 to 64 letters, digits, _ or -");
            return;
        }
        Long endAt = call.getLong("endAt");
        if (endAt == null) {
            call.reject("endAt: a time, in ms since the epoch");
            return;
        }
        String url = call.getString("url");
        String path = null;
        if (url != null && !url.isEmpty()) {
            path = CookTimerPlan.sitePath(url, CookTimers.origin(getBridge().getAppUrl()));
            if (path == null) {
                call.reject("url: a page of this site");
                return;
            }
        }
        long at = CookTimerPlan.triggerAt(endAt, System.currentTimeMillis());
        JSObject result = new JSObject();
        if (at == CookTimerPlan.NEVER) {
            CookTimers.cancel(getContext(), id);
            result.put("scheduled", false);
        } else {
            boolean exact = CookTimers.schedule(getContext(), id, at, call.getString("title"), call.getString("text"), path);
            result.put("scheduled", true);
            result.put("exact", exact);
        }
        result.put("notifications", NotificationManagerCompat.from(getContext()).areNotificationsEnabled());
        call.resolve(result);
    }

    @PluginMethod
    public void cancel(PluginCall call) {
        String id = call.getString("id");
        if (!CookTimerPlan.validId(id)) {
            call.reject("id: 1 to 64 letters, digits, _ or -");
            return;
        }
        CookTimers.cancel(getContext(), id);
        call.resolve();
    }

    @PluginMethod
    public void stop(PluginCall call) {
        CookTimers.stopAll(getContext());
        call.resolve();
    }
}
