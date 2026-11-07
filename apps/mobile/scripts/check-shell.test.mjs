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
  const found = read(`../web/src/${file}`).match(new RegExp(`^pub const ${name}: &str = "([^"]*)";$`, 'm'));
  assert.ok(found, `${name} in apps/web/src/${file}`);
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

test("the app's name and User-Agent marker are apps/web's", () => {
  const { config } = loadConfig({});
  assert.equal(config.appName, rustConst('brand.rs', 'NAME'));
  assert.equal(config.appendUserAgent, rustConst('mobile_app.rs', 'USER_AGENT_MARKER'));
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
});
