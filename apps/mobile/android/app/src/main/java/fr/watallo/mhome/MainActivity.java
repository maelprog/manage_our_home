package fr.watallo.mhome;

import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebView;
import androidx.activity.OnBackPressedCallback;
import androidx.core.content.pm.ShortcutInfoCompat;
import androidx.core.content.pm.ShortcutManagerCompat;
import androidx.core.graphics.drawable.IconCompat;
import androidx.core.splashscreen.SplashScreen;
import com.getcapacitor.BridgeActivity;
import com.getcapacitor.BridgeWebViewClient;
import com.getcapacitor.CapConfig;
import com.getcapacitor.WebViewListener;
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
 * <p>What it changes in Capacitor's behaviour, six times.
 *
 * <p>At the first launch (#737), while no choice is stored, it opens no
 * site but the APK's own {@code www/choix-du-site.html}, served by
 * Capacitor's local server at {@code https://localhost}, and registers
 * {@link SitePlugin} for that page alone: the flavour's standard site
 * ({@link #SITE_STANDARD}, stored as a choice and not as an address, so
 * that a later APK with another standard site moves the app along) or an
 * address of the member's own ({@link #SITE_ORIGIN}). Storing either
 * restarts the activity on it.
 *
 * <p>The same page comes back to change the site afterwards (#738), with
 * the current one to keep: on the intent {@link #ACTION_CHANGE_SITE},
 * which the launcher's shortcut « Changer de site » sends (long press on
 * the icon), and on {@link #CHANGE_SITE_URL}, which the offline page opens
 * and the WebView never loads. Neither changes anything by itself: the
 * site changes when the member chooses another one on that page, which
 * then clears the old one's session ({@link SitePlugin#choose}). Another
 * app can send the intent too, the activity being the launcher's, and a
 * site open the URL: either only shows that page.
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
 *
 * <p>The launch screen (#779, res/values/styles.xml) stays up until the
 * first page shows ({@code onPageCommitVisible}: the site's, the choice
 * page, or the offline page when the site does not answer), and
 * {@link #LAUNCH_SCREEN_MAX_MS} at most: past that, the WebView shows
 * whatever it has, and the offline page still comes if the site never
 * answers. Only when the activity starts afresh: a restart on another
 * site ({@link #changeSite}, {@link #openSite}) keeps nothing up.
 */
public class MainActivity extends BridgeActivity {

    /** The private preferences file that holds the chosen site. */
    static final String SITE_PREFERENCES = "site";
    /** Its key: an origin, {@code https://host[:port]}. */
    static final String SITE_ORIGIN = "origin";
    /** Its other key: true when the flavour's standard site was chosen. */
    static final String SITE_STANDARD = "standard";
    /** The first-launch page, out of {@code www/}. */
    static final String CHOICE_PAGE = "/choix-du-site.html";
    /** The intent that shows {@link #CHOICE_PAGE} over a stored choice. */
    static final String ACTION_CHANGE_SITE = "fr.watallo.mhome.CHANGE_SITE";
    /** What the offline page opens for it: taken by the WebView client, never loaded. */
    static final String CHANGE_SITE_URL = "mhome:changer-de-site";
    private static final String CHANGE_SITE_SHORTCUT = "changer-de-site";

    /**
     * The config of {@link #CHOICE_PAGE} when this launch shows it (no
     * choice is stored yet, or the site is being changed, and the config
     * could be rewritten), else null.
     */
    private CapConfig choice;

    private static final String TAG = "MHome";

    /** The longest the launch screen waits for the first page, in ms. */
    static final long LAUNCH_SCREEN_MAX_MS = 5_000;

    /** True while the launch screen waits for the first page. */
    private boolean launching;

    private final Handler launchTimeout = new Handler(Looper.getMainLooper());
    private final Runnable endLaunchScreen = this::endLaunchScreen;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Before super.onCreate, as androidx.core.splashscreen requires;
        // it then puts on the theme the launch theme names
        // (postSplashScreenTheme).
        SplashScreen launchScreen = SplashScreen.installSplashScreen(this);
        if (savedInstanceState == null) {
            launching = true;
            launchScreen.setKeepOnScreenCondition(() -> launching);
            launchTimeout.postDelayed(endLaunchScreen, LAUNCH_SCREEN_MAX_MS);
        }
        SharedPreferences site = getSharedPreferences(SITE_PREFERENCES, MODE_PRIVATE);
        boolean changing = ACTION_CHANGE_SITE.equals(getIntent().getAction());
        if (changing || (!site.contains(SITE_ORIGIN) && !site.getBoolean(SITE_STANDARD, false))) {
            choice = choiceConfig();
        }
        if (choice != null) {
            // Before super.onCreate, which builds the bridge; and only when
            // the page is shown: a standard site opened because the choice
            // page could not be must not get the plugin.
            registerPlugin(SitePlugin.class);
        }
        super.onCreate(savedInstanceState);
        addChangeSiteShortcut();
    }

    /** Lets the launch screen go, at the first page or at the time limit. */
    private void endLaunchScreen() {
        launching = false;
        launchTimeout.removeCallbacks(endLaunchScreen);
    }

    @Override
    public void onDestroy() {
        launchTimeout.removeCallbacks(endLaunchScreen);
        super.onDestroy();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if (intent != null && ACTION_CHANGE_SITE.equals(intent.getAction())) {
            changeSite();
        }
    }

    /** Restarts the activity on {@link #CHOICE_PAGE}, unless it is shown already. */
    void changeSite() {
        if (choice != null) {
            return;
        }
        setIntent(new Intent(this, MainActivity.class).setAction(ACTION_CHANGE_SITE));
        recreate();
    }

    /** Restarts the activity on the stored site, once chosen or kept. */
    void openSite() {
        setIntent(new Intent(this, MainActivity.class).setAction(Intent.ACTION_MAIN));
        recreate();
    }

    /**
     * The launcher's shortcut to {@link #ACTION_CHANGE_SITE}. A dynamic one:
     * a static shortcut names its target package, which the staging
     * flavour changes. None below Android 7.1, where ShortcutManagerCompat
     * does nothing; a failure leaves the app without it, and nothing else.
     */
    private void addChangeSiteShortcut() {
        try {
            ShortcutManagerCompat.pushDynamicShortcut(
                this,
                new ShortcutInfoCompat.Builder(this, CHANGE_SITE_SHORTCUT)
                    .setShortLabel(getString(R.string.change_site_short))
                    .setLongLabel(getString(R.string.change_site_long))
                    .setIcon(IconCompat.createWithResource(this, R.mipmap.ic_launcher))
                    .setIntent(new Intent(ACTION_CHANGE_SITE).setClass(this, MainActivity.class))
                    .build()
            );
        } catch (RuntimeException e) {
            Log.w(TAG, "cannot add the shortcut to change the site", e);
        }
    }

    @Override
    protected void load() {
        config = choice != null ? choice : storedSiteConfig();
        super.load();
        bridge.setWebViewClient(new SiteWebViewClient(bridge));
        bridge.addWebViewListener(
            new WebViewListener() {
                @Override
                public void onPageCommitVisible(WebView view, String url) {
                    endLaunchScreen();
                }
            }
        );
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
            JSONObject synced = new JSONObject(readAsset(this, "capacitor.config.json"));
            JSONObject server = synced.getJSONObject("server");
            server.put("url", stored);
            server.put("hostname", authority);
            return writtenConfig(synced);
        } catch (IOException | JSONException e) {
            Log.e(TAG, "cannot open the stored site " + stored + ": opening the standard site", e);
            return null;
        }
    }

    /**
     * The synced config without {@code server.url}, so that Capacitor
     * serves the APK's web directory, at {@code https://localhost} (not at
     * the standard site's host, whose cookies the WebView keeps), starting
     * at {@link #CHOICE_PAGE}; null if it cannot be rewritten, and the
     * stored site, or else the standard one, then opens.
     */
    private CapConfig choiceConfig() {
        try {
            JSONObject synced = new JSONObject(readAsset(this, "capacitor.config.json"));
            JSONObject server = synced.getJSONObject("server");
            server.remove("url");
            server.put("hostname", "localhost");
            server.put("appStartPath", CHOICE_PAGE);
            return writtenConfig(synced);
        } catch (IOException | JSONException e) {
            Log.e(TAG, "cannot show the choice of a site: opening the standard site", e);
            return null;
        }
    }

    /**
     * {@code config} as a CapConfig. CapConfig takes a config from a file
     * (loadFromFile), and then still reads what it reads from the app: a
     * debug build's WebView debugging and logs.
     */
    private CapConfig writtenConfig(JSONObject config) throws IOException {
        File dir = new File(getNoBackupFilesDir(), "site-config");
        if (!dir.isDirectory() && !dir.mkdirs()) {
            throw new IOException("cannot create " + dir);
        }
        try (OutputStream out = new FileOutputStream(new File(dir, "capacitor.config.json"))) {
            out.write(config.toString().getBytes(StandardCharsets.UTF_8));
        }
        return CapConfig.loadFromFile(this, dir.getPath() + "/");
    }

    /** The flavour's standard site, {@code server.url} of the synced config, or null. */
    static String standardOrigin(Context context) {
        try {
            return new JSONObject(readAsset(context, "capacitor.config.json")).getJSONObject("server").getString("url");
        } catch (IOException | JSONException e) {
            Log.e(TAG, "cannot read the synced config", e);
            return null;
        }
    }

    /**
     * The stored choice, as {@code www/site-choice.js} reads it:
     * {@code {standard: true}}, {@code {origin}}, or null when none is.
     */
    static JSONObject storedChoice(Context context) {
        SharedPreferences site = context.getSharedPreferences(SITE_PREFERENCES, MODE_PRIVATE);
        try {
            String origin = site.getString(SITE_ORIGIN, null);
            if (origin != null) {
                return new JSONObject().put("origin", origin);
            }
            return site.getBoolean(SITE_STANDARD, false) ? new JSONObject().put("standard", true) : null;
        } catch (JSONException e) {
            return null;
        }
    }

    /**
     * Stores the choice made on {@link #CHOICE_PAGE}: {@code origin}, or
     * the standard site when it is null. Written at once (commit), since
     * the activity restarts on it right after.
     */
    static boolean storeChoice(Context context, String origin) {
        SharedPreferences.Editor site = context.getSharedPreferences(SITE_PREFERENCES, MODE_PRIVATE).edit();
        if (origin == null) {
            site.remove(SITE_ORIGIN).putBoolean(SITE_STANDARD, true);
        } else {
            site.remove(SITE_STANDARD).putString(SITE_ORIGIN, origin);
        }
        return site.commit();
    }

    private static String readAsset(Context context, String name) throws IOException {
        try (InputStream in = context.getAssets().open(name)) {
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

        private final com.getcapacitor.Bridge bridge;

        SiteWebViewClient(com.getcapacitor.Bridge bridge) {
            super(bridge);
            this.bridge = bridge;
        }

        /** {@link #CHANGE_SITE_URL} is the app's, and goes nowhere else. */
        @Override
        public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
            if (CHANGE_SITE_URL.equals(request.getUrl().toString())) {
                if (bridge.getActivity() instanceof MainActivity) {
                    ((MainActivity) bridge.getActivity()).changeSite();
                }
                return true;
            }
            return super.shouldOverrideUrlLoading(view, request);
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
