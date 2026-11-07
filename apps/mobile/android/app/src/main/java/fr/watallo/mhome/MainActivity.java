package fr.watallo.mhome;

import android.util.Log;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebView;
import androidx.activity.OnBackPressedCallback;
import com.getcapacitor.BridgeActivity;
import com.getcapacitor.BridgeWebViewClient;
import com.getcapacitor.CapConfig;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.URI;
import java.net.URISyntaxException;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * The app's one screen: Capacitor's WebView on the site's origin.
 *
 * <p>What it changes in Capacitor's behaviour, three times.
 *
 * <p>The site it opens (#736) is the one stored in the app's private
 * preferences ({@link #SITE_PREFERENCES}, key {@link #SITE_ORIGIN}) when
 * there is one and it is an https origin as {@code www/site-address.js}
 * gives them back; otherwise the flavour's standard site, {@code server.url}
 * of the synced capacitor.config.json (capacitor.config.ts), which the build
 * checks against the flavour. A stored site replaces {@code server.url} and
 * {@code server.hostname} of that config and nothing else: Capacitor injects
 * its bridge, and with it the plugins, at {@code server.url}'s origin, and
 * serves the offline page ({@code server.errorPath}) at
 * {@code server.hostname}'s, so both follow the site; no
 * {@code allowNavigation} is needed. The preferences stay off backups and
 * transfers like the rest of the app's data (allowBackup false,
 * res/xml/data_extraction_rules.xml).
 *
 * <p>The back button goes back in the WebView's history and, at its start,
 * closes the app. {@code @capacitor/app} would go back but, at the start, do
 * nothing unless a page listens for the button; its handler is turned off
 * ({@code plugins.App.disableBackButtonHandler} in capacitor.config.ts) and
 * this one takes its place.
 *
 * <p>An HTTP error status on a page
 * (a 404, a 429, a 500) shows the page the site sent with it, not the offline
 * page. Capacitor's {@link BridgeWebViewClient} loads {@code server.errorPath}
 * on any error of the main frame, an HTTP status included; the offline page
 * would then say "Pas de connexion" over an answer the server did give. A
 * request that gets no answer at all ({@code onReceivedError}: no network, a
 * name that does not resolve, a server that does not answer) still leads
 * there.
 */
public class MainActivity extends BridgeActivity {

    /** The private preferences file that holds the chosen site. */
    static final String SITE_PREFERENCES = "site";
    /** Its key: an origin, {@code https://host[:port]}. */
    static final String SITE_ORIGIN = "origin";

    private static final String TAG = "MHome";

    @Override
    protected void load() {
        config = storedSiteConfig();
        super.load();
        bridge.setWebViewClient(new SiteWebViewClient(bridge));
        getOnBackPressedDispatcher()
            .addCallback(
                this,
                new OnBackPressedCallback(true) {
                    @Override
                    public void handleOnBackPressed() {
                        WebView webView = bridge.getWebView();
                        if (webView.canGoBack()) {
                            webView.goBack();
                        } else {
                            finish();
                        }
                    }
                }
            );
    }

    /**
     * The synced config with the stored site in place of the standard one,
     * or null (Capacitor then reads the synced config as it is) when no
     * site is stored, when the stored one is not a canonical https origin,
     * or when the config cannot be rewritten.
     */
    private CapConfig storedSiteConfig() {
        String stored = getSharedPreferences(SITE_PREFERENCES, MODE_PRIVATE).getString(SITE_ORIGIN, null);
        if (stored == null) {
            return null;
        }
        String authority = httpsOriginAuthority(stored);
        if (authority == null) {
            Log.w(TAG, "stored site " + stored + " is not an https origin: opening the standard site");
            return null;
        }
        try {
            JSONObject synced = new JSONObject(readAsset("capacitor.config.json"));
            JSONObject server = synced.getJSONObject("server");
            server.put("url", stored);
            server.put("hostname", authority);
            // CapConfig takes a config from a file (loadFromFile), and then
            // still reads what it reads from the app: a debug build's
            // WebView debugging and logs.
            File dir = new File(getNoBackupFilesDir(), "site-config");
            if (!dir.isDirectory() && !dir.mkdirs()) {
                throw new IOException("cannot create " + dir);
            }
            try (OutputStream out = new FileOutputStream(new File(dir, "capacitor.config.json"))) {
                out.write(synced.toString().getBytes(StandardCharsets.UTF_8));
            }
            return CapConfig.loadFromFile(this, dir.getPath() + "/");
        } catch (IOException | JSONException e) {
            Log.e(TAG, "cannot open the stored site " + stored + ": opening the standard site", e);
            return null;
        }
    }

    private String readAsset(String name) throws IOException {
        try (InputStream in = getAssets().open(name)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] chunk = new byte[8192];
            for (int n; (n = in.read(chunk)) != -1; ) {
                out.write(chunk, 0, n);
            }
            return out.toString("UTF-8");
        }
    }

    /**
     * The {@code host[:port]} of {@code origin} if it is written exactly as
     * {@code normalizeSiteAddress} of www/site-address.js gives an origin
     * back (https, lowercase host, no default port, no credentials, nothing
     * after the host), else null. Not a second normalisation: anything
     * else is refused, not repaired.
     */
    static String httpsOriginAuthority(String origin) {
        URI uri;
        try {
            uri = new URI(origin);
        } catch (URISyntaxException e) {
            return null;
        }
        String host = uri.getHost();
        int port = uri.getPort();
        if (
            !"https".equals(uri.getScheme()) ||
            host == null ||
            uri.getRawUserInfo() != null ||
            !uri.getRawPath().isEmpty() ||
            uri.getRawQuery() != null ||
            uri.getRawFragment() != null ||
            port == 443
        ) {
            return null;
        }
        String authority = host.toLowerCase(Locale.ROOT) + (port == -1 ? "" : ":" + port);
        return origin.equals("https://" + authority) ? authority : null;
    }

    static final class SiteWebViewClient extends BridgeWebViewClient {

        SiteWebViewClient(com.getcapacitor.Bridge bridge) {
            super(bridge);
        }

        /**
         * Skips {@code BridgeWebViewClient.onReceivedHttpError}, which would
         * load the error page. What else it does, telling the bridge's
         * {@code WebViewListener}s, reaches nobody here: neither Capacitor
         * 8.6 nor {@code @capacitor/app} registers a listener of that event.
         */
        @Override
        public void onReceivedHttpError(WebView view, WebResourceRequest request, WebResourceResponse errorResponse) {
            // The page the server sent is already on its way to the WebView.
        }
    }
}
