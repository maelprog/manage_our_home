package fr.watallo.mhome;

import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebView;
import androidx.activity.OnBackPressedCallback;
import com.getcapacitor.BridgeActivity;
import com.getcapacitor.BridgeWebViewClient;

/**
 * The app's one screen: Capacitor's WebView on the site's origin
 * (capacitor.config.ts, {@code server.url}).
 *
 * <p>What it changes in Capacitor's behaviour, twice.
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

    @Override
    protected void load() {
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
