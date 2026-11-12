// What the Android shell promises and nothing else checks (#665): whom it
// trusts for TLS, which site each target opens, and that its name and
// User-Agent marker are apps/web's. `npm test` in apps/mobile; the CI job
// `mobile` runs it before building the APK. No Android SDK needed: it reads
// the sources, and loads capacitor.config.ts the way `npx cap sync` does
// (Node's type stripping).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const mobile = fileURLToPath(new URL('..', import.meta.url));
const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const res = 'android/app/src/main/res';

// The config as `npx cap sync` would compute it under `env`: the parsed
// object, or the error it throws.
function loadConfig(env) {
  const run = spawnSync(
    process.execPath,
    [
      '--input-type=module',
      '-e',
      "import c from './capacitor.config.ts'; process.stdout.write(JSON.stringify(c));",
    ],
    { cwd: mobile, env: { PATH: process.env.PATH, ...env }, encoding: 'utf8' },
  );
  return run.status === 0 ? { config: JSON.parse(run.stdout) } : { error: run.stderr };
}

const uncommented = (xml) => xml.replace(/<!--[\s\S]*?-->/g, '');

// Each <tag …> of `xml`, comments aside, as its attributes (name → value),
// whatever the spacing and the quotes around them.
function elements(xml, tag) {
  return [...uncommented(xml).matchAll(new RegExp(`<${tag}(?=[\\s/>])([^>]*)>`, 'g'))].map(([, body]) =>
    Object.fromEntries(
      [...body.matchAll(/([\w:.-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g)].map(([, name, dq, sq]) => [name, dq ?? sq]),
    ),
  );
}

// The `src` of each <certificates> of a network security config.
const anchors = (nsc) => elements(nsc, 'certificates').map((attrs) => attrs.src);

// What the manifest of `main` sets and no other source set may touch: a
// flavour or build type manifest is merged over main's, and can set,
// replace (tools:replace) or remove (tools:remove) any of them. Naming the
// attribute is what policyOverrides looks for.
const POLICY_ATTRIBUTES = /\b(networkSecurityConfig|usesCleartextTraffic|allowBackup|fullBackupContent|dataExtractionRules)\b/;
const POLICY_RESOURCES = ['res/xml/network_security_config.xml', 'res/xml/data_extraction_rules.xml'];

// Every domain a backup or transfer rule can name (Android's
// data-extraction-rules).
const STORAGE_DOMAINS = [
  'root', 'file', 'database', 'sharedpref', 'external',
  'device_root', 'device_file', 'device_database', 'device_sharedpref',
];

// The source sets under `src`, other than main, whose manifest names one of
// those attributes, or that ship one of those files at res/xml/ exactly.
// Not caught, left to review: a qualified directory (res/xml-v24/, in main
// too), and a manifest that replaces <application> whole (tools:node).
function policyOverrides(src) {
  return readdirSync(src).filter((set) => {
    if (set === 'main') return false;
    const manifest = join(src, set, 'AndroidManifest.xml');
    return (
      (existsSync(manifest) && POLICY_ATTRIBUTES.test(uncommented(readFileSync(manifest, 'utf8')))) ||
      POLICY_RESOURCES.some((res) => existsSync(join(src, set, res)))
    );
  });
}

test('anchors are read whatever the spacing and quoting, comments aside', () => {
  assert.deepEqual(anchors('<certificates src="system"/>'), ['system']);
  assert.deepEqual(anchors('<certificates src = "user"/>'), ['user']);
  assert.deepEqual(anchors("<certificates\n  src='user' />"), ['user']);
  assert.deepEqual(anchors('<!-- <certificates src="user"/> -->'), []);
});

test('a source set that names a policy attribute or ships a policy file at res/xml/ is caught', () => {
  const src = mkdtempSync(join(tmpdir(), 'check-shell-'));
  try {
    const put = (path, text) => {
      mkdirSync(join(src, path, '..'), { recursive: true });
      writeFileSync(join(src, path), text);
    };
    put('main/AndroidManifest.xml', '<application android:networkSecurityConfig="@xml/network_security_config"/>');
    put('debug/AndroidManifest.xml', '<uses-permission android:name="android.permission.CAMERA" />');
    assert.deepEqual(policyOverrides(src), []);
    put(
      'staging/AndroidManifest.xml',
      '<manifest xmlns:tools="http://schemas.android.com/tools"><application' +
        ' android:networkSecurityConfig="@xml/lax" tools:replace="android:networkSecurityConfig"/></manifest>',
    );
    put('prodDebug/AndroidManifest.xml', '<application tools:remove="android:usesCleartextTraffic"/>');
    put('prod/res/xml/network_security_config.xml', '<network-security-config/>');
    put('stagingDebug/res/xml/data_extraction_rules.xml', '<data-extraction-rules/>');
    assert.deepEqual(policyOverrides(src).sort(), ['prod', 'prodDebug', 'staging', 'stagingDebug']);
  } finally {
    rmSync(src, { recursive: true, force: true });
  }
});

function rustConst(file, name) {
  const found = read(`../${file}`).match(new RegExp(`^pub const ${name}: &str = "([^"]*)";$`, 'm'));
  assert.ok(found, `${name} in apps/${file}`);
  return found[1];
}

test('the app trusts the system authorities only, and no plain http', () => {
  const nsc = uncommented(read(`${res}/xml/network_security_config.xml`));
  assert.deepEqual(anchors(nsc), ['system']);
  assert.deepEqual(elements(nsc, 'base-config'), [{ cleartextTrafficPermitted: 'false' }]);
  // No exception for one host, no debug-only authority, no pinning that
  // could be left behind.
  assert.doesNotMatch(nsc, /domain-config|debug-overrides|pin-set|overridePins/);
  // No certificate shipped in the APK for a trust anchor to point at.
  assert.ok(!existsSync(`${mobile}/${res}/raw`), `${res}/raw holds no certificate`);
  assert.deepEqual(policyOverrides(`${mobile}/android/app/src`), [], 'no other source set names the policy or ships its files at res/xml/');
});

test('the manifest applies that policy, keeps the session off backups, and asks for the camera', () => {
  const manifest = read('android/app/src/main/AndroidManifest.xml');
  const [app, ...others] = elements(manifest, 'application');
  assert.equal(others.length, 0);
  assert.equal(app['android:networkSecurityConfig'], '@xml/network_security_config');
  assert.equal(app['android:usesCleartextTraffic'], 'false');
  assert.equal(app['android:allowBackup'], 'false');
  // On Android 12 and later, allowBackup false stops the cloud backup but
  // not the transfer to a new phone: data_extraction_rules.xml excludes
  // every storage domain from both.
  assert.equal(app['android:dataExtractionRules'], '@xml/data_extraction_rules');
  const rules = uncommented(read(`${res}/xml/data_extraction_rules.xml`));
  assert.doesNotMatch(rules, /<include\b/);
  for (const section of ['cloud-backup', 'device-transfer']) {
    const bodies = [...rules.matchAll(new RegExp(`<${section}\\b[^>]*>([\\s\\S]*?)</${section}>`, 'g'))];
    assert.equal(bodies.length, 1, section);
    assert.deepEqual(
      elements(bodies[0][1], 'exclude').map((e) => `${e.domain}:${e.path}`).sort(),
      STORAGE_DOMAINS.map((domain) => `${domain}:.`).sort(),
      section,
    );
  }
  assert.ok(elements(manifest, 'uses-permission').some((p) => p['android:name'] === 'android.permission.CAMERA'));
  // The local reminders (#668): the permission to show them (asked at run
  // time from Android 13), and exact alarms, which the member may refuse
  // (the reminders then go on inexact). Not USE_EXACT_ALARM, which Android
  // would grant without asking.
  const permissions = elements(manifest, 'uses-permission').map((p) => p['android:name']);
  assert.ok(permissions.includes('android.permission.POST_NOTIFICATIONS'));
  assert.ok(permissions.includes('android.permission.SCHEDULE_EXACT_ALARM'));
  assert.ok(!permissions.includes('android.permission.USE_EXACT_ALARM'));
  // The Social Login plugin's manifest (#670) declares USE_CREDENTIALS, an
  // AccountManager permission Android stopped using at API 23, below this
  // app's minSdk: removed from the merged manifest rather than declared
  // for nothing.
  assert.deepEqual(
    elements(manifest, 'uses-permission').filter((p) => p['android:name'] === 'android.permission.USE_CREDENTIALS'),
    [{ 'android:name': 'android.permission.USE_CREDENTIALS', 'tools:node': 'remove' }],
  );
  assert.equal(elements(manifest, 'manifest')[0]['xmlns:tools'], 'http://schemas.android.com/tools');
  assert.deepEqual(
    elements(manifest, 'uses-feature').filter((f) => f['android:name'] === 'android.hardware.camera'),
    [{ 'android:name': 'android.hardware.camera', 'android:required': 'false' }],
  );
});

test('staging is the default target, and opens the staging server', () => {
  const { config, error } = loadConfig({});
  assert.equal(error, undefined);
  assert.equal(config.server.url, 'https://staging.watallo.fr');
  assert.equal(config.server.hostname, 'staging.watallo.fr');
  assert.equal(config.server.cleartext, false);
  // The bridge, and the plugins, on the site's origin only: a site stored
  // on the phone (#736) replaces server.url in MainActivity, it needs no
  // other origin allowed.
  assert.equal(config.server.allowNavigation, undefined);
  // MainActivity handles the back button; two handlers would race.
  assert.equal(config.plugins.App.disableBackButtonHandler, true);
  assert.deepEqual(loadConfig({ MHOME_TARGET: 'staging' }).config, config);
});

test('prod opens the https origin it is given, and refuses anything else', () => {
  const ok = loadConfig({ MHOME_TARGET: 'prod', MHOME_PROD_URL: 'https://example.org' });
  assert.equal(ok.config?.server.url, 'https://example.org');
  assert.equal(ok.config.server.hostname, 'example.org');
  for (const [env, why] of [
    [{ MHOME_TARGET: 'prod' }, /needs MHOME_PROD_URL/],
    [{ MHOME_TARGET: 'prod', MHOME_PROD_URL: 'http://example.org' }, /expected an https origin/],
    [{ MHOME_TARGET: 'prod', MHOME_PROD_URL: 'https://example.org/app' }, /expected an https origin/],
    [{ MHOME_TARGET: 'dev' }, /expected staging or prod/],
  ]) {
    const { config, error } = loadConfig(env);
    assert.equal(config, undefined, JSON.stringify(env));
    assert.match(error, why);
  }
});

test('the Social Login plugin brings Google alone, and no client ID of its own', () => {
  // #670: the plugin's sync hook reads `providers` and leaves the SDKs of
  // the others out of the APK; the web client ID comes from apps/api with
  // each nonce.
  const { config } = loadConfig({});
  assert.deepEqual(config.plugins.SocialLogin, {
    providers: { google: true, facebook: false, apple: false, twitter: false },
  });
});

test("the app's name and User-Agent marker are the site's", () => {
  const { config } = loadConfig({});
  assert.equal(config.appName, rustConst('web/src/brand.rs', 'NAME'));
  assert.equal(config.appendUserAgent, rustConst('shared/src/mobile_app.rs', 'USER_AGENT_MARKER'));
});

test('the offline page is in the web directory, and loads nothing from elsewhere', () => {
  const { config } = loadConfig({});
  const page = read(`${config.webDir}/${config.server.errorPath}`);
  assert.match(page, /<html lang="fr">/);
  assert.match(page, /Réessayer/);
  // Capacitor serves this one file out of the APK; any other URL would be
  // fetched from the network the page is there to say is missing.
  assert.doesNotMatch(page, /\b(src|href)=|url\(|@import/);
  // DESIGN.md: no var(--x, #hex) fallback.
  assert.doesNotMatch(page, /var\(--[\w-]+\s*,/);
  // "Changer de site" (#738) opens the URL MainActivity takes for itself:
  // the two cannot diverge, and it is no address the WebView would fetch.
  const activity = read('android/app/src/main/java/fr/watallo/mhome/MainActivity.java');
  const change = activity.match(/static final String CHANGE_SITE_URL = "([^"]+)";/);
  assert.ok(change, 'MainActivity.CHANGE_SITE_URL');
  assert.doesNotMatch(change[1], /^(https?|file|data|blob|javascript):/i);
  assert.match(page, />Changer de site</);
  assert.ok(page.includes(`location.assign("${change[1]}")`), `the offline page opens ${change[1]}`);
});

// What a module imports: each `import … from '…'`, `export … from '…'` and
// bare `import '…'`, at the start of a line or after a `;`.
const moduleRefs = (js) =>
  [...js.matchAll(/(?:^|;)\s*(?:import|export)\b(?:[^;'"]*?\bfrom)?\s*['"]([^'"]+)['"]/gm)].map((m) => m[1]);

// The files of the web directory a page or a module of it loads: each
// `src`/`href` of the page and what its inline scripts import, what each
// module imports, followed from file to file. Throws on anything that is
// not a relative path to a file of the directory: the first-launch page
// has nothing else (#737).
function localClosure(webDir, page) {
  const seen = new Set();
  const visit = (file, text) => {
    const refs = file.endsWith('.html')
      ? [
          ...[...uncommented(text).matchAll(/\b(?:src|href)\s*=\s*["']([^"']*)["']/g)].map((m) => m[1]),
          ...[...uncommented(text).matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)].flatMap((m) => moduleRefs(m[1])),
        ]
      : moduleRefs(text);
    for (const ref of refs) {
      assert.match(ref, /^(\.\/)?[\w-]+\.(js|css)$/, `${file}: ${ref} is not a file of ${webDir}`);
      const target = ref.replace(/^\.\//, '');
      assert.ok(existsSync(`${mobile}/${webDir}/${target}`), `${file}: ${target} is not in ${webDir}`);
      if (!seen.has(target)) {
        seen.add(target);
        visit(target, read(`${webDir}/${target}`));
      }
    }
    // No other way out: no dynamic import, no request of its own (the probe
    // is MainActivity's Site plugin), no stylesheet or font from elsewhere.
    assert.doesNotMatch(text, /\bimport\s*\(|\bfetch\s*\(|XMLHttpRequest|WebSocket|url\(|@import/, `${file} loads something of its own`);
  };
  visit(page, read(`${webDir}/${page}`));
  return [...seen].sort();
}

test('the first-launch page is in the web directory, and loads nothing from elsewhere', () => {
  const { config } = loadConfig({});
  // MainActivity opens it at the first launch (#737), out of the APK.
  const activity = read('android/app/src/main/java/fr/watallo/mhome/MainActivity.java');
  const path = activity.match(/static final String CHOICE_PAGE = "\/([\w-]+\.html)";/);
  assert.ok(path, 'MainActivity.CHOICE_PAGE');
  const page = read(`${config.webDir}/${path[1]}`);
  assert.match(page, /<html lang="fr">/);
  assert.match(page, /Site standard/);
  assert.match(page, /Site personnalisé/);
  assert.match(page, />Continuer</);
  assert.deepEqual(localClosure(config.webDir, path[1]), ['choix-du-site.js', 'site-address.js', 'site-choice.js']);
  // DESIGN.md: no var(--x, #hex) fallback.
  assert.doesNotMatch(page, /var\(--[\w-]+\s*,/);
  // The field is labelled, and the error is read out and tied to it.
  assert.match(page, /<label for="address">/);
  assert.match(page, /<input id="address"[^>]*aria-describedby="[^"]*\baddress-error\b/);
  assert.match(page, /<p id="address-error"[^>]*role="alert"/);
  // The current site to keep (#738): hidden until the page knows there is
  // one, so that the first launch does not offer it.
  assert.match(page, /<section [^>]*id="current"[^>]*\bhidden\b/);
  assert.match(page, /<button [^>]*id="keep"/);
});

test("the choice page keeps the current site as it is, and leaves another one's reminders first", () => {
  // choix-du-site.js drives the DOM and the plugins, which node does not
  // have: its `store` is read, not run. The decisions are site-choice.js's,
  // tested in site-choice.test.mjs; this is how the page follows them.
  const js = read('www/choix-du-site.js');
  const store = js.match(/\nasync function store\(choice, onError\) \{\n([\s\S]*?)\n\}\n/);
  assert.ok(store, 'choix-du-site.js: async function store(choice, onError)');
  const body = store[1].replace(/\s+/g, ' ');
  // The same site: reopened, and nothing else.
  assert.match(body, /^ const kind = changeKind\(current, choice\); if \(kind === 'keep'\) \{ await reopen\(onError\); return; \}/);
  // Another site: its reminders gone first, or no change at all.
  const leave = body.indexOf(
    "if (kind === 'change') { const forgot = await forgetReminders(window.Capacitor?.Plugins?.LocalNotifications); if (forgot.error) { onError(forgot.error); return; } }",
  );
  const choose = body.indexOf('await site.choose(choice);');
  assert.ok(leave !== -1, 'store leaves the old site through forgetReminders, and stops on its error');
  assert.ok(choose > leave, 'store chooses after the reminders are gone');
  assert.equal(body.split('site.choose').length, 2, 'site.choose is called once, after them');
});

test('the closure of a page refuses what is not a file of the web directory', () => {
  const dir = mkdtempSync(join(mobile, 'www-check-'));
  const webDir = dir.slice(mobile.length).replace(/^\//, '');
  try {
    writeFileSync(join(dir, 'a.js'), "import { x } from './b.js';\n");
    writeFileSync(join(dir, 'b.js'), 'export const x = 1;\n');
    for (const [html, js, why] of [
      ['<script src="https://cdn.example/x.js"></script>', '', /is not a file/],
      ['<link href="//cdn.example/x.css">', '', /is not a file/],
      ['<script src="missing.js"></script>', '', /is not in/],
      ['<script src="c.js"></script>', "import x from 'https://cdn.example/x.js';", /is not a file/],
      ['<script src="c.js"></script>', "import 'https://cdn.example/x.js';", /is not a file/],
      ['<script type="module">import x from "https://cdn.example/x.js";</script>', '', /is not a file/],
      ['<script type="module">\n  import "https://cdn.example/x.js";\n</script>', '', /is not a file/],
      ['<script src="c.js"></script>', "fetch('https://example.org');", /c\.js loads something/],
      ['<style>body { background: url(x.png) }</style>', '', /p\.html loads something/],
    ]) {
      writeFileSync(join(dir, 'p.html'), html);
      writeFileSync(join(dir, 'c.js'), js);
      assert.throws(() => localClosure(webDir, 'p.html'), why, html + js);
    }
    writeFileSync(join(dir, 'p.html'), '<script type="module" src="a.js"></script>');
    assert.deepEqual(localClosure(webDir, 'p.html'), ['a.js', 'b.js']);
    writeFileSync(join(dir, 'p.html'), '<script type="module">import { x } from "./a.js";</script>');
    assert.deepEqual(localClosure(webDir, 'p.html'), ['a.js', 'b.js']);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// The custom properties a page declares in `:root`, light and dark.
function pageTokens(page) {
  const block = (css) => Object.fromEntries([...css.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map(([, k, v]) => [k, v.trim()]));
  const light = page.match(/<style>\s*:root \{([^}]*)\}/);
  const dark = page.match(/@media \(prefers-color-scheme: dark\) \{\s*:root \{([^}]*)\}/);
  assert.ok(light && dark, 'a light and a dark :root');
  return { light: block(light[1]), dark: block(dark[1]) };
}

// The WCAG 2.x contrast ratio of two #RRGGBB.
function contrast(a, b) {
  const luminance = (hex) => {
    const linear = (c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
    const [r, g, bl] = [1, 3, 5].map((i) => linear(parseInt(hex.slice(i, i + 2), 16) / 255));
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
  };
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

test("the shell's two pages share DESIGN.md's tokens, and their text meets AA in both themes", () => {
  assert.equal(contrast('#000000', '#FFFFFF').toFixed(2), '21.00');
  assert.equal(contrast('#777777', '#FFFFFF').toFixed(2), '4.48');
  const offline = pageTokens(read('www/hors-ligne.html'));
  const choice = pageTokens(read('www/choix-du-site.html'));
  assert.deepEqual(Object.keys(choice.dark).sort(), Object.keys(choice.light).sort(), 'each token redeclared in dark');
  for (const theme of ['light', 'dark']) {
    for (const [name, value] of Object.entries(offline[theme])) {
      assert.equal(choice[theme][name], value, `${theme} ${name}`);
    }
    const t = choice[theme];
    // Each text colour of the first-launch page on each ground it is
    // painted on: the card, the page around it, a button, a hovered
    // secondary button, the error notice.
    for (const [fg, bg] of [
      ['--fg', '--surface'], ['--fg', '--bg'], ['--muted', '--surface'], ['--muted', '--bg'],
      ['--accent-fg', '--accent'], ['--accent-fg', '--accent-hover'],
      ['--fg', '--hover'], ['--error', '--error-soft'], ['--error', '--surface'],
    ]) {
      const ratio = contrast(t[fg], t[bg]);
      assert.ok(ratio >= 4.5, `${theme}: ${fg} on ${bg} is ${ratio.toFixed(2)}:1`);
    }
  }
});

// The launch screen (#779). A values file's <color>s, and a <style>'s
// parent and <item>s, as name → value.
const colours = (xml) =>
  Object.fromEntries(
    [...uncommented(xml).matchAll(/<color\s+name="([\w.]+)"\s*>([^<]*)<\/color>/g)].map(([, k, v]) => [k, v.trim()]),
  );
function style(xml, name) {
  const found = uncommented(xml).match(
    new RegExp(`<style\\s+name="${name.replace(/\./g, '\\.')}"([^>]*)>([\\s\\S]*?)</style>`),
  );
  assert.ok(found, `a style ${name}`);
  return {
    parent: (found[1].match(/parent="([^"]*)"/) ?? [])[1],
    items: Object.fromEntries(
      [...found[2].matchAll(/<item\s+name="([\w:]+)"\s*>([^<]*)<\/item>/g)].map(([, k, v]) => [k, v.trim()]),
    ),
  };
}
const brandColour = (name) =>
  read('../web/src/brand.rs').match(new RegExp(`^pub const ${name}: &str = "(#[0-9A-Fa-f]{6})";$`, 'm'))[1];

test('the launch screen follows the system theme, on the grounds and in the ink of the offline page', () => {
  const tokens = pageTokens(read('www/hors-ligne.html'));
  const light = colours(read(`${res}/values/colors.xml`));
  const dark = colours(read(`${res}/values-night/colors.xml`));
  // The light ground is the manifest's background_color, which
  // app/build.gradle turns into @color/brand_background.
  assert.equal(light.splash_background, '@color/brand_background');
  assert.equal(brandColour('BACKGROUND_COLOR').toUpperCase(), tokens.light['--bg'].toUpperCase());
  assert.equal(dark.splash_background?.toUpperCase(), tokens.dark['--bg'].toUpperCase());
  assert.equal(light.splash_name?.toUpperCase(), tokens.light['--fg'].toUpperCase());
  assert.equal(dark.splash_name?.toUpperCase(), tokens.dark['--fg'].toUpperCase());
  for (const theme of ['light', 'dark']) {
    const ratio = contrast(tokens[theme]['--fg'], tokens[theme]['--bg']);
    assert.ok(ratio >= 4.5, `${theme}: the name is ${ratio.toFixed(2)}:1 on its ground`);
  }
  // One style for both themes and every API level, but for the name,
  // which Android draws from 12 (API 31) only.
  const launch = style(read(`${res}/values/styles.xml`), 'AppTheme.NoActionBarLaunch');
  assert.equal(launch.parent, 'Theme.SplashScreen');
  assert.deepEqual(launch.items, {
    windowSplashScreenBackground: '@color/splash_background',
    windowSplashScreenAnimatedIcon: '@mipmap/ic_launcher',
    postSplashScreenTheme: '@style/AppTheme.NoActionBar',
  });
  const launch31 = style(read(`${res}/values-v31/styles.xml`), 'AppTheme.NoActionBarLaunch');
  assert.equal(launch31.parent, launch.parent);
  assert.deepEqual(launch31.items, {
    ...launch.items,
    'android:windowSplashScreenBrandingImage': '@drawable/splash_name',
  });
  const manifest = read('android/app/src/main/AndroidManifest.xml');
  assert.equal(elements(manifest, 'activity')[0]['android:theme'], '@style/AppTheme.NoActionBarLaunch');
});

test("the name on the launch screen is the logo lockup's word, in the theme's ink", () => {
  const word = [...read('../web/src/brand/logo-mono.svg').matchAll(/<path fill="currentColor" d="([^"]+)"/g)];
  assert.equal(word.length, 1, 'one filled path in the lockup: the word');
  const paths = elements(read(`${res}/drawable/splash_name.xml`), 'path');
  assert.deepEqual(paths, [{ 'android:fillColor': '@color/splash_name', 'android:pathData': word[0][1] }]);
});

test('the launch screen stays until a page shows, the offline page included, and 5 s at most', () => {
  const activity = read('android/app/src/main/java/fr/watallo/mhome/MainActivity.java');
  const onCreate = activity.slice(activity.indexOf('protected void onCreate('), activity.indexOf('private void endLaunchScreen('));
  const install = onCreate.indexOf('SplashScreen.installSplashScreen(this)');
  assert.ok(install !== -1 && install < onCreate.indexOf('super.onCreate('), 'installed before super.onCreate');
  const max = activity.match(/static final long LAUNCH_SCREEN_MAX_MS = ([\d_]+);/);
  assert.ok(max, 'a LAUNCH_SCREEN_MAX_MS');
  const ms = Number(max[1].replace(/_/g, ''));
  assert.ok(ms > 0 && ms <= 5000, `${ms} ms`);
  // On a fresh start only, the screen is held by `launching` alone, and the
  // time limit is set, not cancelled before it runs.
  assert.match(
    onCreate.slice(0, onCreate.indexOf('super.onCreate(')),
    /if \(savedInstanceState == null\) \{\s*launching = true;\s*\w+\.setKeepOnScreenCondition\(\(\) -> launching\);\s*\w+\.postDelayed\(endLaunchScreen, LAUNCH_SCREEN_MAX_MS\);\s*\}/,
  );
  assert.doesNotMatch(onCreate, /removeCallbacks\(|endLaunchScreen\(\)/, 'nothing ends the wait in onCreate');
  assert.match(activity, /Runnable endLaunchScreen = this::endLaunchScreen;/);
  assert.match(activity, /private void endLaunchScreen\(\) \{\s*launching = false;/);
  // A page shows: the site's, the choice page, or the offline page that
  // BridgeWebViewClient loads when the main frame gets no answer. Not the
  // WebView's own error page, which commits first at the address that
  // failed ("Webpage not available", seen on the emulator).
  const notFailed = String.raw`(?:!(\w+)\.equals\(failedPage\)|!Objects\.equals\((\w+), failedPage\)|!Objects\.equals\(failedPage, (\w+)\))`;
  const listener = activity.match(
    new RegExp(
      String.raw`bridge\.addWebViewListener\(\s*new WebViewListener\(\) \{\s*@Override\s*public void onPageCommitVisible\(WebView \w+, String (\w+)\) \{\s*if \(${notFailed}\) \{\s*endLaunchScreen\(\);`,
    ),
  );
  assert.ok(listener, 'a listener on the bridge ends the wait at the first page but the failed one');
  assert.equal(listener[2] ?? listener[3] ?? listener[4], listener[1], 'compares the committed page');
  assert.match(
    activity,
    /public void onReceivedError\(WebView \w+, WebResourceRequest (\w+), WebResourceError \w+\) \{\s*if \(\1\.isForMainFrame\(\)[^{]*\{\s*[^;]*failedPage = \1\.getUrl\(\)\.toString\(\);/,
  );
});

test('the photo of a barcode opens the camera, and a refused camera says so (#802)', () => {
  // Capacitor's BridgeWebChromeClient opens the camera for an input with
  // accept="image/*" and capture (apps/web/src/routes/stocks/new.rs) only
  // when `new Intent(MediaStore.ACTION_IMAGE_CAPTURE).resolveActivity()`
  // finds a camera app; it falls back to the file picker otherwise. From
  // Android 11 on (targetSdk 30 and up), that lookup sees no other app
  // unless the manifest declares the intent in <queries>.
  const manifest = uncommented(read('android/app/src/main/AndroidManifest.xml'));
  const queries = [...manifest.matchAll(/<queries>([\s\S]*?)<\/queries>/g)].map(([, body]) => body);
  assert.equal(queries.length, 1, 'one <queries> block');
  const intents = [...queries[0].matchAll(/<intent>([\s\S]*?)<\/intent>/g)].map(([, body]) => elements(body, 'action'));
  assert.ok(
    intents.some((actions) => actions.some((a) => a['android:name'] === 'android.media.action.IMAGE_CAPTURE')),
    'the camera intent is visible to the app',
  );
  // The camera refused, Capacitor hands the page no file and only logs a
  // warning: the activity puts in its place a client that tells the member.
  const activity = read('android/app/src/main/java/fr/watallo/mhome/MainActivity.java');
  const load = activity.slice(activity.indexOf('protected void load('));
  assert.match(load, /bridge\.getWebView\(\)\.setWebChromeClient\(new CameraChromeClient\(bridge\)\);/);
  const start = activity.indexOf('static final class CameraChromeClient extends BridgeWebChromeClient');
  assert.ok(start !== -1, 'a CameraChromeClient');
  const client = activity.slice(start);
  assert.match(client, /public boolean onShowFileChooser\(/);
  // Wrapped only for a field Capacitor asks the camera for (capture and
  // image/*, as in BridgeWebChromeClient), and only while it is not granted.
  assert.match(
    client,
    /if \(!asksForPhoto\((\w+)\) \|\| cameraGranted\(\)\) \{\s*return super\.onShowFileChooser\(\w+, \w+, \1\);/,
  );
  assert.match(
    client,
    /boolean asksForPhoto\(FileChooserParams (\w+)\) \{\s*return \1\.isCaptureEnabled\(\) && Arrays\.asList\(\1\.getAcceptTypes\(\)\)\.contains\("image\/\*"\);\s*\}/,
  );
  assert.match(client, /Manifest\.permission\.CAMERA/);
  // Told only when nothing came back and the camera is still refused: a
  // photo cancelled with the camera granted says nothing.
  assert.match(client, /if \(\w+ == null && !cameraGranted\(\)\) \{\s*Toast\.makeText\([^;]*R\.string\.camera_refused/);
  const strings = elements(read(`${res}/values/strings.xml`), 'string').map((s) => s.name);
  assert.ok(strings.includes('camera_refused'), 'the message is a string resource');
});
