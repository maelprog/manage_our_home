package fr.watallo.mhome;

import android.content.Context;
import android.webkit.CookieManager;
import android.webkit.WebStorage;
import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.MalformedURLException;
import java.net.SocketTimeoutException;
import java.net.URL;
import java.net.UnknownHostException;
import java.nio.charset.StandardCharsets;
import javax.net.ssl.SSLException;
import org.json.JSONObject;

/**
 * What the page of the choice of a site ({@code www/choix-du-site.html},
 * #737, #738) asks of the app: which standard site this APK has and which
 * site is stored, a probe of a typed address, storing the choice, and
 * keeping the current one. Registered by {@link MainActivity} only while
 * that page is shown, so the sites the app opens never see it: none of
 * them can change the site the app opens.
 *
 * <p>What a probe answer means is decided by the page
 * ({@code www/site-choice.js}); this class only reports it.
 */
@CapacitorPlugin(name = "Site")
public class SitePlugin extends Plugin {

    /** Enough for the web manifest, which is under a kilobyte. */
    private static final int MAX_BODY = 64 * 1024;
    private static final int TIMEOUT_MS = 10_000;

    /**
     * The application id, the app's name, the flavour's standard site, and
     * the stored choice ({@code current}, absent at the first launch).
     */
    @PluginMethod
    public void info(PluginCall call) {
        Context context = getContext();
        String standard = MainActivity.standardOrigin(context);
        if (standard == null) {
            call.reject("no standard site in the synced config");
            return;
        }
        JSObject info = new JSObject();
        info.put("applicationId", context.getPackageName());
        info.put("appName", context.getApplicationInfo().loadLabel(context.getPackageManager()).toString());
        info.put("standardOrigin", standard);
        JSONObject current = MainActivity.storedChoice(context);
        if (current != null) {
            info.put("current", current);
        }
        call.resolve(info);
    }

    /**
     * A GET of {@code url}, an https URL, through the app's own network
     * policy (the system authorities only, res/xml/network_security_config.xml),
     * redirects not followed: {@code {status, location, body}}, or
     * {@code {failure}} when no answer came: {@code certificate},
     * {@code unknown-host}, {@code timeout} or {@code unreachable}. Run on
     * Capacitor's plugin thread, not the UI's.
     */
    @PluginMethod
    public void probe(PluginCall call) {
        URL url;
        try {
            url = new URL(call.getString("url", ""));
        } catch (MalformedURLException e) {
            call.reject("not a URL");
            return;
        }
        if (!"https".equals(url.getProtocol())) {
            call.reject("not an https URL");
            return;
        }
        JSObject result = new JSObject();
        HttpURLConnection connection = null;
        try {
            connection = (HttpURLConnection) url.openConnection();
            connection.setInstanceFollowRedirects(false);
            connection.setConnectTimeout(TIMEOUT_MS);
            connection.setReadTimeout(TIMEOUT_MS);
            connection.setUseCaches(false);
            connection.setRequestProperty("Accept", "application/manifest+json, application/json");
            int status = connection.getResponseCode();
            result.put("status", status);
            String location = connection.getHeaderField("Location");
            if (location != null) {
                result.put("location", location);
            }
            InputStream in = status >= 400 ? connection.getErrorStream() : connection.getInputStream();
            result.put("body", in == null ? "" : readCapped(in));
        } catch (SSLException e) {
            result.put("failure", "certificate");
        } catch (UnknownHostException e) {
            result.put("failure", "unknown-host");
        } catch (SocketTimeoutException e) {
            result.put("failure", "timeout");
        } catch (IOException e) {
            result.put("failure", "unreachable");
        } finally {
            if (connection != null) {
                connection.disconnect();
            }
        }
        call.resolve(result);
    }

    /**
     * Stores {@code {standard: true}} or {@code {origin}}, an origin as
     * {@code normalizeSiteAddress} gives them back, clears what the WebView
     * kept of the sites opened before (#738: their cookies, the session
     * among them, their web storage and the HTTP cache), then restarts the
     * activity, which opens that site. The page has removed the old site's
     * reminders first; it does not call this to keep the current site
     * ({@link #keep}).
     */
    @PluginMethod
    public void choose(PluginCall call) {
        boolean standard = Boolean.TRUE.equals(call.getBoolean("standard", false));
        String origin = call.getString("origin");
        if (!standard && (origin == null || MainActivity.httpsOriginAuthority(origin) == null)) {
            call.reject("neither the standard site nor an https origin");
            return;
        }
        if (!MainActivity.storeChoice(getContext(), standard ? null : origin)) {
            call.reject("the choice could not be stored");
            return;
        }
        call.resolve();
        getActivity()
            .runOnUiThread(() -> {
                WebStorage.getInstance().deleteAllData();
                bridge.getWebView().clearCache(true);
                CookieManager cookies = CookieManager.getInstance();
                cookies.removeAllCookies(removed -> {
                    cookies.flush();
                    ((MainActivity) getActivity()).openSite();
                });
            });
    }

    /** Restarts the activity on the stored site, as it was left. */
    @PluginMethod
    public void keep(PluginCall call) {
        call.resolve();
        getActivity().runOnUiThread(() -> ((MainActivity) getActivity()).openSite());
    }

    private static String readCapped(InputStream in) throws IOException {
        try (InputStream body = in) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] chunk = new byte[8192];
            for (int n; out.size() < MAX_BODY && (n = body.read(chunk, 0, Math.min(chunk.length, MAX_BODY - out.size()))) != -1; ) {
                out.write(chunk, 0, n);
            }
            return new String(out.toByteArray(), StandardCharsets.UTF_8);
        }
    }
}
