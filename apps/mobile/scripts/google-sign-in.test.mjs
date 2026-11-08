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
    // The URL parser drops a tab or a newline anywhere: each of these is
    // `//evil.example/`, another site, to it.
    '/\t/evil.example/',
    '/\n/evil.example/',
    '/\r/evil.example/',
    // No other control character, nor a backslash anywhere, in a path
    // apps/api writes.
    '/login\u0000',
    '/login\u007f',
    '/a\\b',
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
    const outcome = plain(await G.signIn(env));
    assert.equal(outcome.state, 'failed');
    assert.equal(outcome.step, 'picker');
    assert.ok(!calls.some((c) => c[1] === '/api/auth/google/id-token'));
  }
});

test('signIn: a failure names its step and what the plugin or the API said', async () => {
  const plugin = harness({ login: () => { throw Object.assign(new Error('no credentials available'), { code: 'NO_CREDENTIAL' }); } });
  assert.deepEqual(plain(await G.signIn(plugin.env)), { state: 'failed', step: 'picker', detail: 'NO_CREDENTIAL' });
  const nonce = harness({ nonce: () => response(500, { error: 'internal' }) });
  assert.deepEqual(plain(await G.signIn(nonce.env)), { state: 'failed', step: 'nonce', detail: 'HTTP 500' });
  const token = harness({ idToken: () => response(401, { error: 'unauthorized' }) });
  assert.deepEqual(plain(await G.signIn(token.env)), { state: 'failed', step: 'id-token', detail: 'HTTP 401' });
  const network = harness({ idToken: () => { throw new TypeError('Failed to fetch'); } });
  assert.deepEqual(plain(await G.signIn(network.env)), { state: 'failed', step: 'id-token', detail: 'TypeError' });
  const redirect = harness({ idToken: () => response(200, { redirect: 'https://evil.example/' }) });
  assert.deepEqual(plain(await G.signIn(redirect.env)), { state: 'failed', step: 'redirect', detail: 'redirect' });
});

// A stand-in for env.after: fires the timeout at once, so a call that never
// answers ends the sign-in.
const hang = () => new Promise(() => {});
const immediately = (ms, fn) => setImmediate(fn);

test('signIn: a call that never answers ends in a failure naming its step', async () => {
  for (const [options, step] of [
    [{ nonce: hang }, 'nonce'],
    [{ initialize: hang }, 'picker'],
    [{ login: hang }, 'picker'],
    [{ idToken: hang }, 'id-token'],
  ]) {
    const { env } = harness(options);
    env.after = immediately;
    assert.deepEqual(plain(await G.signIn(env)), { state: 'failed', step, detail: 'timeout' }, step);
  }
});

test('signIn: a logout that never answers does not stop the sign-in', async () => {
  const { env } = harness({ logout: hang });
  env.after = (ms, fn) => {
    // Only the logout's own short timeout fires; the others never do.
    if (ms <= 5000) setImmediate(fn);
  };
  assert.deepEqual(plain(await G.signIn(env)), { state: 'redirect', to: '/' });
});

test('signIn: every outcome is handed to env.log, never the token', async () => {
  const logged = [];
  const { env } = harness();
  env.log = (outcome) => logged.push(plain(outcome));
  await G.signIn(env);
  assert.deepEqual(logged, [{ state: 'redirect', to: '/' }]);
  const failing = harness({ idToken: () => response(401, {}) });
  failing.env.log = (outcome) => logged.push(plain(outcome));
  await G.signIn(failing.env);
  assert.equal(logged.length, 2);
  assert.ok(!JSON.stringify(logged).includes(ID_TOKEN));
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
    assert.equal(plain(await G.signIn(env)).state, 'failed');
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
    assert.equal(plain(await G.signIn(env)).state, 'failed');
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
  // A failure says where it stopped, for the member to read out.
  const located = G.message({ state: 'failed', step: 'id-token', detail: 'HTTP 401' });
  assert.match(located, /Google/);
  assert.match(located, /id-token/);
  assert.match(located, /HTTP 401/);
});

// -- the page -------------------------------------------------------------------

// The app's page as apps/web serves it (`routes::auth::google_actions`):
// the button and both notices hidden, `data-api` on the button; and the
// globals `start` reads, with the Social Login plugin present when
// `plugin`, and apps/api answering through `harness`.
function page({ plugin = false, button = true, ...answers } = {}) {
  const { calls, env } = harness(answers);
  const element = (attributes = {}) => ({
    hidden: true,
    disabled: false,
    textContent: '',
    listeners: {},
    getAttribute: (name) => (name in attributes ? attributes[name] : null),
    addEventListener(type, listener) {
      (this.listeners[type] ??= []).push(listener);
    },
  });
  const elements = {
    'google-native-unsupported': element(),
    'google-native-error': element(),
  };
  if (button) elements['google-native'] = element({ 'data-api': '/api' });
  const assigned = [];
  const win = {
    document: { getElementById: (id) => elements[id] ?? null },
    fetch: env.fetch,
    // The limits never fire here: `signIn`'s tests cover them.
    setTimeout: () => 0,
    location: { assign: (to) => assigned.push(to) },
  };
  if (plugin) win.Capacitor = { Plugins: { SocialLogin: env.plugin } };
  const click = () => elements['google-native'].listeners.click.forEach((listener) => listener());
  return { win, calls, elements, assigned, click };
}

// Lets a sign-in started by a touch run to its end.
async function settle() {
  for (let i = 0; i < 20; i += 1) await new Promise((resolve) => setImmediate(resolve));
}

test('start: without the plugin, the button stays hidden and the page says to update', () => {
  const { win, elements } = page();
  G.start(win);
  const button = elements['google-native'];
  assert.equal(button.hidden, true);
  assert.deepEqual(button.listeners, {});
  assert.equal(elements['google-native-unsupported'].hidden, false);
  assert.equal(elements['google-native-error'].hidden, true);
});

test('start: a page without the button is left as it is', () => {
  const { win, elements } = page({ button: false, plugin: true });
  G.start(win);
  assert.equal(elements['google-native-unsupported'].hidden, true);
  assert.equal(elements['google-native-error'].hidden, true);
});

test('start: with the plugin, the button shows, and a touch signs in then goes where the API says', async () => {
  const { win, calls, elements, assigned, click } = page({ plugin: true });
  G.start(win);
  const button = elements['google-native'];
  assert.equal(button.hidden, false);
  assert.equal(elements['google-native-unsupported'].hidden, true);

  click();
  // No second sign-in while this one runs.
  assert.equal(button.disabled, true);
  await settle();
  // apps/api at the button's `data-api`.
  assert.equal(calls[0][1], '/api/auth/google/nonce');
  assert.deepEqual(assigned, ['/']);
  // The page is leaving: the button stays off.
  assert.equal(button.disabled, true);
  assert.equal(elements['google-native-error'].hidden, true);
});

test('start: the picker closed puts the page back as it was, silently', async () => {
  const { win, elements, assigned, click } = page({
    plugin: true,
    login: () => {
      throw Object.assign(new Error('Google Sign-In cancelled by user'), { code: 'USER_CANCELLED' });
    },
  });
  G.start(win);
  click();
  await settle();
  assert.deepEqual(assigned, []);
  assert.equal(elements['google-native'].disabled, false);
  assert.equal(elements['google-native-error'].hidden, true);
  assert.equal(elements['google-native-error'].textContent, '');
});

test('start: a failure is said, the button comes back, and a new touch clears the message', async () => {
  const { win, elements, assigned, click } = page({
    plugin: true,
    idToken: () => response(401, { error: 'unauthorized' }),
  });
  G.start(win);
  const error = elements['google-native-error'];
  click();
  await settle();
  assert.deepEqual(assigned, []);
  assert.equal(elements['google-native'].disabled, false);
  assert.equal(error.hidden, false);
  assert.equal(error.textContent, G.message({ state: 'failed', step: 'id-token', detail: 'HTTP 401' }));

  click();
  assert.equal(error.hidden, true);
  assert.equal(elements['google-native'].disabled, true);
  await settle();
});

test('start: too many attempts is said as such', async () => {
  const { win, elements, click } = page({
    plugin: true,
    nonce: () => response(429, { error: 'too_many_attempts' }),
  });
  G.start(win);
  click();
  await settle();
  assert.equal(elements['google-native-error'].textContent, G.message({ state: 'throttled' }));
  assert.equal(elements['google-native-error'].hidden, false);
});

test('the script starts on its own in a page', () => {
  const { win, elements } = page({ plugin: true });
  const context = vm.createContext({ window: win });
  vm.runInContext(SOURCE, context, { filename: 'google-sign-in.js' });
  assert.equal(elements['google-native'].hidden, false);
});
