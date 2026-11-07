// The Capacitor shell of MHome (#665), model A of docs/front-stack-study.md
// § 2: the WebView loads the real https origin, apps/web runs as it is, and
// the app adds only what a browser cannot give.
//
// Read by `npx cap sync` (and `npx cap copy`), which writes the result to
// android/app/src/main/assets/capacitor.config.json. That file is what the
// APK carries; it is not versioned (android/.gitignore), so the target is
// whatever the last sync chose. The Gradle build checks that it matches the
// flavour being built (android/app/build.gradle, `checkSyncedTarget`).
//
// The target is chosen by MHOME_TARGET:
// - `staging` (the default): https://staging.watallo.fr;
// - `prod`: the URL in MHOME_PROD_URL, required, https, which the production
//   domain is not fixed yet to fill in here.
//
// That target is the flavour's standard site. A site stored in the app's
// preferences (#736, an instance of its own) takes its place at launch:
// MainActivity then rewrites `server.url` and `server.hostname` below, and
// keeps the rest.
//
// Loaded by Node's own type stripping (Capacitor 8 with TypeScript 7): no
// enum, no namespace, no parameter property.
import { readFileSync } from 'node:fs';
import type { CapacitorConfig } from '@capacitor/cli';

const STAGING_URL = 'https://staging.watallo.fr';

// A `pub const NAME: &str = "…";` of a Rust file of apps/web, so the app
// and the site cannot disagree: the app's name is the manifest's
// (apps/web/src/brand.rs), and the User-Agent marker is the one
// apps/web/src/mobile_app.rs looks for.
function rustConst(file: string, name: string, shape: RegExp): string {
  const source = readFileSync(new URL(`../web/src/${file}`, import.meta.url), 'utf8');
  const found = source.match(new RegExp(`^pub const ${name}: &str = "([^"\\\\]*)";$`, 'm'));
  if (!found) {
    throw new Error(`apps/web/src/${file}: no \`pub const ${name}: &str = "…";\` line`);
  }
  if (!shape.test(found[1])) {
    throw new Error(`apps/web/src/${file}: ${name} = "${found[1]}" does not match ${shape}`);
  }
  return found[1];
}

function serverUrl(): string {
  const target = process.env.MHOME_TARGET ?? 'staging';
  if (target === 'staging') {
    return STAGING_URL;
  }
  if (target !== 'prod') {
    throw new Error(`MHOME_TARGET=${target}: expected staging or prod`);
  }
  const raw = process.env.MHOME_PROD_URL;
  if (!raw) {
    throw new Error('MHOME_TARGET=prod needs MHOME_PROD_URL, e.g. https://example.org');
  }
  const url = new URL(raw);
  if (url.protocol !== 'https:' || url.pathname !== '/' || url.search || url.hash) {
    throw new Error(`MHOME_PROD_URL=${raw}: expected an https origin, nothing after the host`);
  }
  return url.origin;
}

const url = serverUrl();

const config: CapacitorConfig = {
  appId: 'fr.watallo.mhome',
  appName: rustConst('brand.rs', 'NAME', /^[^<>&]+$/),
  // Nothing of apps/web is bundled (model A); the directory holds only the
  // page shown when the site cannot be loaded.
  webDir: 'www',
  appendUserAgent: rustConst('mobile_app.rs', 'USER_AGENT_MARKER', /^[A-Za-z0-9]+$/),
  server: {
    url,
    // The host of the error page: with it, the page is served (out of the
    // APK, by Capacitor's local server) at the site's own origin, so its
    // "Réessayer" goes back to `/` there. Left at its default, `localhost`,
    // `/` would mean the APK's assets.
    hostname: new URL(url).host,
    androidScheme: 'https',
    errorPath: 'hors-ligne.html',
    cleartext: false,
  },
  android: {
    // No http:// content inside the https page, whatever the site serves.
    allowMixedContent: false,
  },
  plugins: {
    // The back button is MainActivity's: back in the history, then out of
    // the app. The plugin's own handler would stay put at the start of the
    // history unless a page listens for the button.
    App: { disableBackButtonHandler: true },
    // The native Google sign-in (#670), driven by apps/web/src/
    // google-sign-in.js. Google only: the plugin's hook, run by
    // `npx cap sync`, writes these switches into its gradle.properties
    // (`socialLogin.<provider>.include`), and the APK is then built without
    // the Facebook SDK (com.facebook.android:facebook-login). Apple's and
    // Twitter's sign-ins bring no SDK on Android to leave out. The web
    // client ID it signs in for is
    // not here: apps/api hands it out with each nonce, so that one APK
    // serves whatever `GOOGLE_CLIENT_ID` its server checks.
    SocialLogin: {
      providers: { google: true, facebook: false, apple: false, twitter: false },
    },
  },
};

export default config;
