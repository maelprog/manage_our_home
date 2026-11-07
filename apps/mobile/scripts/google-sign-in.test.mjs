// The native Google sign-in of the app (#670): apps/web/src/google-sign-in.js,
// the script apps/web serves on /login and /register to the app's pages
// only. It runs in the WebView, against the Social Login plugin; here it
// runs in a bare `vm` context, with the plugin and `fetch` replaced, so that
// what it asks and what it answers can be read back. `npm test` in
// apps/mobile.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const SOURCE = readFileSync(new URL('../../web/src/google-sign-in.js', import.meta.url), 'utf8');

// The script's API, as its file defines it, in a context with no `window`:
// nothing starts on its own.
function load() {
  const context = vm.createContext({});
  vm.runInContext(SOURCE, context, { filename: 'google-sign-in.js' });
  const api = vm.runInContext('MHomeGoogleSignIn', context);
  assert.ok(api, 'the script defines MHomeGoogleSignIn');
  return api;
}

const G = load();

const CLIENT_ID = '1234-abcd.apps.googleusercontent.com';
const ID_TOKEN = 'header.payload.signature';

function response(status, body) {
  return {
    status,
    ok: status >= 200 && status < 300,
    json: () => (body === undefined ? Promise.reject(new SyntaxError('no body')) : Promise.resolve(body)),
  };
}

// A run of `signIn` against apps/api answering `nonce` and `idToken` (a
// response, or a function of the request), and a plugin whose `login`
// does `login` (resolves with what it returns, or throws). Every call is
// recorded in `calls`.
function harness({
  nonce = () => response(200, { nonce: 'n0nce', client_id: CLIENT_ID }),
  idToken = () => response(200, { redirect: '/' }),
  login = () => ({ provider: 'google', result: { responseType: 'online', idToken: ID_TOKEN } }),
  initialize = () => undefined,
  logout = () => undefined,
} = {}) {
  const calls = [];
  const plugin = {
    initialize: async (options) => {
      calls.push(['initialize', options]);
      return initialize(options);
    },
    login: async (options) => {
      calls.push(['login', options]);
      return login(options);
    },
    logout: async (options) => {
      calls.push(['logout', options]);
      return logout(options);
    },
  };
  const fetch = async (url, init) => {
    calls.push(['fetch', url, init]);
    if (url === '/api/auth/google/nonce') return nonce(init);
    if (url === '/api/auth/google/id-token') return idToken(init);
    throw new Error(`unexpected ${url}`);
  };
  return { calls, env: { plugin, fetch, api: '/api' } };
}

// Plain objects out of the `vm` context, for `deepEqual`.
const plain = (value) => JSON.parse(JSON.stringify(value));

// -- where the page goes -------------------------------------------------------

test('the page follows a path of the site, and nothing else', () => {
  assert.equal(G.redirectTarget({ redirect: '/' }), '/');
  assert.equal(G.redirectTarget({ redirect: '/login?google=email_not_ascii' }), '/login?google=email_not_ascii');
  for (const redirect of [
    'https://evil.example/',
    '//evil.example/',
    '/\\evil.example/',
    'javascript:alert(1)',
    '',
    'login',
    42,
    null,
    undefined,
  ]) {
    assert.equal(G.redirectTarget({ redirect }), null, String(redirect));
  }
  assert.equal(G.redirectTarget(null), null);
  assert.equal(G.redirectTarget('/'), null);
});

test('only the plugin\'s own code is a cancellation', () => {
  assert.equal(G.isCancelled({ code: 'USER_CANCELLED', message: 'Google Sign-In cancelled by user' }), true);
  for (const error of [
    { message: 'Google Sign-In cancelled by user' },
    { code: 'UNAVAILABLE' },
    new Error('Google Sign-In failed: no credentials'),
    null,
    undefined,
  ]) {
    assert.equal(G.isCancelled(error), false, JSON.stringify(error));
  }
});

// -- one sign-in ----------------------------------------------------------------

test('signIn: a nonce, the account picker with it, the token posted, then where the API says', async () => {
  const { calls, env } = harness();
  const outcome = await G.signIn(env);
  assert.deepEqual(plain(outcome), { state: 'redirect', to: '/' });

  const [nonceCall, init, login, logout, post] = calls;
  // The nonce first, with its cookie kept.
  assert.equal(nonceCall[0], 'fetch');
  assert.equal(nonceCall[1], '/api/auth/google/nonce');
  assert.equal(nonceCall[2].method, 'POST');
  assert.equal(nonceCall[2].credentials, 'include');
  // The plugin, for the web client the API checks the token's audience
  // against, online, and with no Google API scope to authorise.
  assert.deepEqual(plain(init), [
    'initialize',
    { google: { webClientId: CLIENT_ID, mode: 'online', skipAuthOnlyAuthorizationAfterSignIn: true } },
  ]);
  // The picker, with the nonce, and nothing else.
  assert.deepEqual(plain(login), ['login', { provider: 'google', options: { nonce: 'n0nce' } }]);
  // The plugin forgets the token it keeps on the phone.
  assert.deepEqual(plain(logout), ['logout', { provider: 'google' }]);
  // The token, to the API, with the nonce's cookie.
  assert.equal(post[1], '/api/auth/google/id-token');
  assert.equal(post[2].method, 'POST');
  assert.equal(post[2].credentials, 'include');
  assert.equal(post[2].headers['Content-Type'], 'application/json');
  assert.deepEqual(JSON.parse(post[2].body), { id_token: ID_TOKEN });
  assert.equal(calls.length, 5);
});

test('signIn: a refusal goes where the API sends it, the login page that says why', async () => {
  const { env } = harness({
    idToken: () => response(200, { redirect: '/login?google=email_not_ascii', refusal: 'email_not_ascii' }),
  });
  assert.deepEqual(plain(await G.signIn(env)), { state: 'redirect', to: '/login?google=email_not_ascii' });
});

test('signIn: the picker closed by the member is a silent return', async () => {
  const { calls, env } = harness({
    login: () => {
      throw Object.assign(new Error('Google Sign-In cancelled by user'), { code: 'USER_CANCELLED' });
    },
  });
  assert.deepEqual(plain(await G.signIn(env)), { state: 'cancelled' });
  // Nothing is posted.
  assert.ok(!calls.some((c) => c[1] === '/api/auth/google/id-token'));
});

test('signIn: the plugin failing is a failure, nothing posted', async () => {
  for (const options of [
    { login: () => { throw new Error('Google Sign-In failed: no credentials available'); } },
    { initialize: () => { throw new Error('google.clientId is null or empty'); } },
    { login: () => ({ provider: 'google', result: { responseType: 'offline', serverAuthCode: 'x' } }) },
    { login: () => ({ provider: 'google', result: { idToken: '' } }) },
    { login: () => undefined },
  ]) {
    const { calls, env } = harness(options);
    assert.deepEqual(plain(await G.signIn(env)), { state: 'failed' });
    assert.ok(!calls.some((c) => c[1] === '/api/auth/google/id-token'));
  }
});

test('signIn: a nonce the API does not give is a failure, the picker never opens', async () => {
  for (const nonce of [
    () => response(500, { error: 'internal' }),
    () => response(200, { nonce: 'n0nce' }),
    () => response(200, { client_id: CLIENT_ID }),
    () => response(200, { nonce: '', client_id: CLIENT_ID }),
    () => response(200),
    () => { throw new TypeError('Failed to fetch'); },
  ]) {
    const { calls, env } = harness({ nonce });
    assert.deepEqual(plain(await G.signIn(env)), { state: 'failed' });
    assert.ok(!calls.some((c) => c[0] === 'login'));
  }
});

test('signIn: too many attempts from the address is said as such', async () => {
  const tooMany = () => response(429, { error: 'too_many_attempts' });
  for (const options of [{ nonce: tooMany }, { idToken: tooMany }]) {
    const { env } = harness(options);
    assert.deepEqual(plain(await G.signIn(env)), { state: 'throttled' });
  }
});

test('signIn: a token the API refuses, or an answer it cannot follow, is a failure', async () => {
  for (const idToken of [
    () => response(401, { error: 'unauthorized' }),
    () => response(500, { error: 'internal' }),
    () => response(200, { redirect: 'https://evil.example/' }),
    () => response(200),
    () => { throw new TypeError('Failed to fetch'); },
  ]) {
    const { env } = harness({ idToken });
    assert.deepEqual(plain(await G.signIn(env)), { state: 'failed' });
  }
});

test('signIn: the plugin failing to forget the token does not stop the sign-in', async () => {
  const { env } = harness({
    logout: () => {
      throw new Error('Failed to clear credential state');
    },
  });
  assert.deepEqual(plain(await G.signIn(env)), { state: 'redirect', to: '/' });
});

test('what the page says of each outcome', () => {
  assert.equal(G.message({ state: 'redirect', to: '/' }), null);
  assert.equal(G.message({ state: 'cancelled' }), null);
  assert.match(G.message({ state: 'throttled' }), /Trop de tentatives/);
  assert.match(G.message({ state: 'failed' }), /Google/);
});
