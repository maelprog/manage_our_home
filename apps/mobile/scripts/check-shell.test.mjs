// What the Android shell promises and nothing else checks (#665): whom it
// trusts for TLS, which site each target opens, and that its name and
// User-Agent marker are apps/web's. `npm test` in apps/mobile; the CI job
// `mobile` runs it before building the APK. No Android SDK needed: it reads
// the sources, and loads capacitor.config.ts the way `npx cap sync` does
// (Node's type stripping).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
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

function rustConst(file, name) {
  const found = read(`../web/src/${file}`).match(new RegExp(`^pub const ${name}: &str = "([^"]*)";$`, 'm'));
  assert.ok(found, `${name} in apps/web/src/${file}`);
  return found[1];
}

test('the app trusts the system authorities only, and no plain http', () => {
  const nsc = read(`${res}/xml/network_security_config.xml`).replace(/<!--[\s\S]*?-->/g, '');
  const anchors = [...nsc.matchAll(/<certificates\s+src="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(anchors, ['system']);
  assert.match(nsc, /<base-config cleartextTrafficPermitted="false">/);
  // No exception for one host, no debug-only authority, no pinning that
  // could be left behind.
  assert.doesNotMatch(nsc, /domain-config|debug-overrides|pin-set|overridePins/);
  // No certificate shipped in the APK for a trust anchor to point at.
  assert.ok(!existsSync(`${mobile}/${res}/raw`), `${res}/raw holds no certificate`);
  for (const flavor of readdirSync(`${mobile}/android/app/src`)) {
    assert.ok(
      !existsSync(`${mobile}/android/app/src/${flavor}/res/xml/network_security_config.xml`) || flavor === 'main',
      `the ${flavor} flavour does not override the policy`,
    );
  }
});

test('the manifest applies that policy, keeps the session off backups, and asks for the camera', () => {
  const manifest = read('android/app/src/main/AndroidManifest.xml');
  assert.match(manifest, /android:networkSecurityConfig="@xml\/network_security_config"/);
  assert.match(manifest, /android:usesCleartextTraffic="false"/);
  assert.match(manifest, /android:allowBackup="false"/);
  assert.match(manifest, /<uses-permission android:name="android.permission.CAMERA" \/>/);
  assert.match(manifest, /<uses-feature android:name="android.hardware.camera" android:required="false" \/>/);
});

test('staging is the default target, and opens the staging server', () => {
  const { config, error } = loadConfig({});
  assert.equal(error, undefined);
  assert.equal(config.server.url, 'https://staging.watallo.fr');
  assert.equal(config.server.hostname, 'staging.watallo.fr');
  assert.equal(config.server.cleartext, false);
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
