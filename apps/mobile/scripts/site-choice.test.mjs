// The first-launch choice of a site (#737): see www/site-choice.js.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { MANIFEST_PATH, judgeProbe, readChoice, standardLabel } from '../www/site-choice.js';

const MANIFEST = JSON.stringify({ name: 'MHome', short_name: 'MHome', id: '/', start_url: '/', lang: 'fr' });
const answer = (status, body = '', extra = {}) => ({ status, body, ...extra });
const ONLINE = { online: true, appName: 'MHome' };

test("the standard site is named after the flavour: staging, or the app's name", () => {
  assert.equal(standardLabel('fr.watallo.mhome.staging', 'MHome'), 'staging');
  assert.equal(standardLabel('fr.watallo.mhome', 'MHome'), 'MHome');
  // Only the suffix the staging flavour adds counts.
  assert.equal(standardLabel('fr.watallo.mhome.stagingx', 'MHome'), 'MHome');
  assert.equal(standardLabel('fr.watallo.staging.mhome', 'MHome'), 'MHome');
});

test('the standard site is stored as such, not as an address', () => {
  assert.deepEqual(readChoice({ kind: 'standard' }), { store: { standard: true } });
  // Whatever was typed in the field meanwhile.
  assert.deepEqual(readChoice({ kind: 'standard', address: 'https://maison.example.org' }), {
    store: { standard: true },
  });
});

test('a custom address is normalised, then probed, never stored at once', () => {
  assert.deepEqual(readChoice({ kind: 'custom', address: ' Maison.Example.org ' }), {
    probe: 'https://maison.example.org',
    url: `https://maison.example.org${MANIFEST_PATH}`,
  });
  assert.deepEqual(readChoice({ kind: 'custom', address: 'https://maison.example.org:8443/' }), {
    probe: 'https://maison.example.org:8443',
    url: 'https://maison.example.org:8443/manifest.webmanifest',
  });
  assert.equal(MANIFEST_PATH, '/manifest.webmanifest');
});

test("a custom address normalizeSiteAddress refuses gives its reason, in French", () => {
  for (const [address, reason] of [
    ['', /vide/],
    [undefined, /vide/],
    ['http://maison.example.org', /Seul https/],
    ['https://maison.example.org/app', /sans chemin/],
    ['192.168.1.10', /IP privée/],
  ]) {
    const decision = readChoice({ kind: 'custom', address });
    assert.deepEqual(Object.keys(decision), ['error'], String(address));
    assert.match(decision.error, reason, String(address));
  }
});

test('an unknown kind of choice is refused', () => {
  assert.deepEqual(Object.keys(readChoice({ kind: 'other' })), ['error']);
  assert.deepEqual(Object.keys(readChoice({})), ['error']);
});

test('a site that answers its manifest as MHome does is stored', () => {
  assert.deepEqual(judgeProbe('https://maison.example.org', answer(200, MANIFEST), ONLINE), {
    store: { origin: 'https://maison.example.org' },
  });
  // The app's name is the one compared, not a constant.
  assert.deepEqual(
    Object.keys(judgeProbe('https://maison.example.org', answer(200, MANIFEST), { online: true, appName: 'Autre' })),
    ['error'],
  );
});

test('the phone offline: nothing is stored, and the message says so', () => {
  for (const probe of [null, { failure: 'unknown-host' }, answer(200, MANIFEST)]) {
    const { error, store } = judgeProbe('https://maison.example.org', probe, { online: false, appName: 'MHome' });
    assert.equal(store, undefined);
    assert.match(error, /pas connecté à Internet/);
  }
});

test('each failure to reach the site has its own clear message, and nothing is stored', () => {
  for (const [failure, reason] of [
    ['certificate', /certificat/],
    ['unknown-host', /Aucun site ne porte ce nom/],
    ['timeout', /ne répond pas/],
    ['unreachable', /ne répond pas/],
    ['anything else', /ne répond pas/],
  ]) {
    const decision = judgeProbe('https://maison.example.org', { failure }, ONLINE);
    assert.deepEqual(Object.keys(decision), ['error'], failure);
    assert.match(decision.error, reason, failure);
    assert.match(decision.error, /Rien n'a été enregistré/, failure);
  }
  // The WebView may say it is online with no network at all (measured on
  // an Android 16 emulator, wifi and data off): a name that does not
  // resolve or a site that does not answer may be the phone's connection,
  // and the message says so.
  for (const failure of ['unknown-host', 'timeout', 'unreachable']) {
    assert.match(judgeProbe('https://maison.example.org', { failure }, ONLINE).error, /connecté à Internet/, failure);
  }
  assert.doesNotMatch(judgeProbe('https://maison.example.org', { failure: 'certificate' }, ONLINE).error, /Internet/);
});

test('a redirect is not followed: the message names where it leads, if an https site', () => {
  const to = (location) => judgeProbe('https://maison.example.org', answer(301, '', { location }), ONLINE);
  assert.match(to('https://www.maison.example.org/manifest.webmanifest').error, /www\.maison\.example\.org/);
  assert.match(to('/ailleurs').error, /renvoie vers une autre adresse/);
  assert.doesNotMatch(to('/ailleurs').error, /undefined/);
  // Within the same site, there is no other address to type.
  assert.doesNotMatch(to('/ailleurs').error, /tapez/);
  assert.doesNotMatch(to('https://maison.example.org/ailleurs').error, /tapez/);
  assert.doesNotMatch(to('http://www.maison.example.org/').error, /www\.maison/);
  assert.match(to(undefined).error, /renvoie vers une autre adresse/);
  assert.match(judgeProbe('https://maison.example.org', answer(308, ''), ONLINE).error, /renvoie/);
});

test('an answer that is not an MHome manifest is foreign', () => {
  const foreign = [
    answer(404, '<html>Not found</html>'),
    answer(500, MANIFEST),
    answer(204, ''),
    answer(200, '<!doctype html><html></html>'),
    answer(200, ''),
    answer(200, 'null'),
    answer(200, '"MHome"'),
    answer(200, JSON.stringify({ name: 'Jellyfin', start_url: '/' })),
    answer(200, JSON.stringify({ name: 'MHome', start_url: '/web/' })),
    answer(200, JSON.stringify({ short_name: 'MHome', start_url: '/' })),
    {},
  ];
  for (const probe of foreign) {
    const decision = judgeProbe('https://maison.example.org', probe, ONLINE);
    assert.deepEqual(Object.keys(decision), ['error'], JSON.stringify(probe));
    assert.match(decision.error, /pas un site MHome/, JSON.stringify(probe));
  }
  assert.match(judgeProbe('https://maison.example.org', answer(404, ''), ONLINE).error, /404/);
});
