package fr.watallo.mhome;

import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;
import com.google.firebase.FirebaseApp;

/**
 * Whether this APK can receive the server's pushes (#1004): built with a
 * google-services.json, Firebase is initialised at launch; built without
 * one (app/build.gradle), it is not, and the Push Notifications plugin's
 * {@code register} and {@code unregister} would throw out of the bridge
 * and close the app. The page asks {@code status} first
 * (apps/web/src/local-reminders.js, www/site-choice.js). Registered by
 * {@link MainActivity} on every page.
 */
@CapacitorPlugin(name = "PushSupport")
public class PushSupportPlugin extends Plugin {

    @PluginMethod
    public void status(PluginCall call) {
        JSObject result = new JSObject();
        result.put("available", !FirebaseApp.getApps(getContext()).isEmpty());
        call.resolve(result);
    }
}
