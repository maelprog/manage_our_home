// The notifications the server pushes to the app by Firebase Cloud
// Messaging (#1004): the part of apps/web/src/local-reminders.js that
// registers the phone's token with the site, forgets it on logout, and
// follows a touch on a notification. The notification itself is shown by
// the app (android/…/PushMessagingService.java), whose texts
// check-shell.test.mjs holds to apps/web/src/sw.js. `npm test` in
// apps/mobile.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const SOURCE = readFileSync(new URL('../../web/src/local-reminders.js', import.meta.url), 'utf8');

function load() {
  const context = vm.createContext({});
  vm.runInContext(SOURCE, context, { filename: 'local-reminders.js' });
  return vm.runInContext('MHomeLocalReminders', context);
}

const R = load();
// Made in the script's own context: compared by value, out of it.
const plain = (value) => JSON.parse(JSON.stringify(value));

const A = '0f8fad5b-d9cb-469f-a165-70867728950e';
const B = '7c9e6679-7425-40de-944b-e07fc1f90ae7';

// -- the page a touch opens --------------------------------------------------

test("a touch opens the kind's page, as the service worker does", () => {
  assert.equal(R.pushPage('agenda_change'), '/agenda');
  assert.equal(R.pushPage('agenda_reminder'), '/agenda');
  assert.equal(R.pushPage('budget_alert'), '/budget');
  assert.equal(R.pushPage('stock_expiry'), '/stocks');
});

test("the pages are the service worker's, kind for kind", () => {
  const context = vm.createContext({ self: { addEventListener() {} } });
  vm.runInContext(readFileSync(new URL('../../web/src/sw.js', import.meta.url), 'utf8'), context);
  const { texts, neutral } = JSON.parse(vm.runInContext('JSON.stringify({ texts: TEXTS, neutral: NEUTRAL })', context));
  for (const [kind, { url }] of Object.entries(texts)) assert.equal(R.pushPage(kind), url, kind);
  assert.equal(R.pushPage('unknown'), neutral.url);
});

test('an unknown kind opens the home page', () => {
  for (const kind of ['', 'unknown', 'toString', '__proto__', undefined, null, 7]) {
    assert.equal(R.pushPage(kind), '/', String(kind));
  }
});

test("the push's entries: the ids it names, separated by commas", () => {
  assert.deepEqual(plain(R.pushedEntries(`${A},${B}`)), [A, B]);
  assert.deepEqual(plain(R.pushedEntries(A)), [A]);
  // What is not an id is left out, the rest kept.
  assert.deepEqual(plain(R.pushedEntries(`${A},nope,${A.toUpperCase()}, ${B}`)), [A]);
});

test('no entry, or more than the API ever sends, marks nothing', () => {
  for (const text of [undefined, null, '', 7, [A]]) {
    assert.deepEqual(plain(R.pushedEntries(text)), [], String(text));
  }
  const many = Array.from({ length: 21 }, () => A).join(',');
  assert.deepEqual(plain(R.pushedEntries(many)), []);
  const twenty = Array.from({ length: 20 }, () => A).join(',');
  assert.equal(R.pushedEntries(twenty).length, 20);
});

test("a touch: the kind's page, and the entries to mark read", () => {
  const action = { actionId: 'tap', notification: { id: 'm1', data: { kind: 'budget_alert', entries: `${A},${B}` } } };
  assert.deepEqual(plain(R.pushTouch(action)), { url: '/budget', entries: [A, B] });
  // Without entries, the page alone.
  const bare = { notification: { data: { kind: 'agenda_change' } } };
  assert.deepEqual(plain(R.pushTouch(bare)), { url: '/agenda', entries: [] });
});

test('a touch on something else than a push opens nothing', () => {
  for (const action of [null, undefined, {}, { notification: {} }, { notification: { data: {} } }, { notification: { data: { kind: 7 } } }]) {
    assert.equal(R.pushTouch(action), null, JSON.stringify(action));
  }
});

// -- registering the phone ---------------------------------------------------

// The Push Notifications plugin and the app's PushSupport plugin, recording
// their calls in one list.
function fakePlugins({ receive = 'granted', available = true, failing = [] } = {}) {
  const calls = [];
  const call = (name, result) => async () => {
    calls.push(name);
    if (failing.includes(name)) throw new Error(`${name} failed`);
    return result;
  };
  return {
    calls,
    push: {
      checkPermissions: call('checkPermissions', { receive }),
      requestPermissions: call('requestPermissions', { receive: 'granted' }),
      register: call('register'),
      unregister: call('unregister'),
    },
    support: { status: call('status', { available }) },
  };
}

test('permission granted: the phone registers with Firebase', async () => {
  const p = fakePlugins();
  const state = await R.pushSync({ push: p.push, support: p.support, signedOut: false });
  assert.equal(state, 'registered');
  assert.deepEqual(p.calls, ['status', 'checkPermissions', 'register']);
});

test('without the permission, nothing is registered and no prompt opens', async () => {
  for (const receive of ['prompt', 'prompt-with-rationale', 'denied']) {
    const p = fakePlugins({ receive });
    const state = await R.pushSync({ push: p.push, support: p.support, signedOut: false });
    assert.equal(state, 'not-allowed', receive);
    assert.deepEqual(p.calls, ['status', 'checkPermissions'], receive);
  }
});

// The token the page last handed to the site, as `start` keeps it on the
// phone.
function memory(kept = null) {
  const m = {
    kept,
    get: () => m.kept,
    set: (token) => { m.kept = token; },
    clear: () => { m.kept = null; },
  };
  return m;
}

test('the permission withdrawn: the site forgets the token, then Firebase does', async () => {
  for (const receive of ['prompt', 'prompt-with-rationale', 'denied']) {
    const p = fakePlugins({ receive });
    const r = recorder();
    const order = [];
    const fetch = async (url, init) => (order.push('site'), r.fetch(url, init));
    const unregister = p.push.unregister;
    p.push.unregister = async () => (order.push('unregister'), unregister());
    const m = memory('tok');
    const state = await R.pushSync({ push: p.push, support: p.support, signedOut: false, fetch, memory: m });
    assert.equal(state, 'withdrawn', receive);
    assert.deepEqual(order, ['site', 'unregister'], receive);
    assert.deepEqual(p.calls, ['status', 'checkPermissions', 'unregister'], receive);
    assert.equal(r.requests.length, 1, receive);
    const [req] = r.requests;
    assert.equal(req.url, R.PUSH_TOKEN_URL);
    assert.equal(req.method, 'DELETE');
    assert.equal(req.credentials, 'same-origin');
    assert.deepEqual(req.body, { token: 'tok' });
    // Done once: the next page has nothing left to withdraw.
    assert.equal(m.kept, null, receive);
  }
});

test('the permission withdrawn, the site failing: Firebase withdraws it all the same', async () => {
  const offline = async () => {
    throw new TypeError('Failed to fetch');
  };
  for (const fetch of [offline, recorder(500).fetch]) {
    const p = fakePlugins({ receive: 'denied' });
    const m = memory('tok');
    const state = await R.pushSync({ push: p.push, support: p.support, signedOut: false, fetch, memory: m });
    assert.equal(state, 'withdrawn');
    assert.deepEqual(p.calls, ['status', 'checkPermissions', 'unregister']);
    assert.equal(m.kept, null);
  }
});

test('the permission withdrawn, Firebase failing: tried again on the next page', async () => {
  const p = fakePlugins({ receive: 'denied', failing: ['unregister'] });
  const m = memory('tok');
  const state = await R.pushSync({ push: p.push, support: p.support, signedOut: false, fetch: recorder().fetch, memory: m });
  assert.equal(state, 'failed');
  assert.equal(m.kept, 'tok');
});

test('never registered: no permission is nothing to withdraw', async () => {
  for (const kept of [null, undefined, '']) {
    const p = fakePlugins({ receive: 'denied' });
    const r = recorder();
    const state = await R.pushSync({ push: p.push, support: p.support, signedOut: false, fetch: r.fetch, memory: memory(kept) });
    assert.equal(state, 'not-allowed');
    assert.deepEqual(p.calls, ['status', 'checkPermissions']);
    assert.equal(r.requests.length, 0);
  }
});

test('permission granted: the kept token stays kept', async () => {
  const p = fakePlugins();
  const r = recorder();
  const m = memory('tok');
  const state = await R.pushSync({ push: p.push, support: p.support, signedOut: false, fetch: r.fetch, memory: m });
  assert.equal(state, 'registered');
  assert.equal(r.requests.length, 0);
  assert.equal(m.kept, 'tok');
});

test('logged out: the kept token goes with Firebase\'s', async () => {
  const p = fakePlugins();
  const m = memory('tok');
  assert.equal(await R.pushSync({ push: p.push, support: p.support, signedOut: true, memory: m }), 'forgotten');
  assert.equal(m.kept, null);
});

test('the logout drops the kept token once Firebase withdrew it', async () => {
  const m = memory('tok');
  await R.forgetToken({ fetch: recorder().fetch, push: fakePlugins().push, memory: m }, 'tok');
  assert.equal(m.kept, null);
  const failing = memory('tok');
  await R.forgetToken({ fetch: recorder().fetch, push: fakePlugins({ failing: ['unregister'] }).push, memory: failing }, 'tok');
  assert.equal(failing.kept, 'tok');
});

test('logged out: the token is withdrawn from Firebase, nothing registered', async () => {
  const p = fakePlugins();
  const state = await R.pushSync({ push: p.push, support: p.support, signedOut: true });
  assert.equal(state, 'forgotten');
  assert.deepEqual(p.calls, ['status', 'unregister']);
});

test('an APK built without Firebase never calls the plugin', async () => {
  // register() would throw out of the plugin, and the app would close.
  for (const signedOut of [false, true]) {
    const p = fakePlugins({ available: false });
    assert.equal(await R.pushSync({ push: p.push, support: p.support, signedOut }), 'unavailable');
    assert.deepEqual(p.calls, ['status']);
  }
});

test('an APK built before #1004 has neither plugin: nothing happens', async () => {
  const p = fakePlugins();
  assert.equal(await R.pushSync({ push: undefined, support: p.support, signedOut: false }), 'unsupported');
  assert.equal(await R.pushSync({ push: p.push, support: undefined, signedOut: false }), 'unsupported');
  assert.deepEqual(p.calls, []);
});

test('a plugin that fails is a failure, not a crash', async () => {
  for (const failing of [['status'], ['checkPermissions'], ['register'], ['unregister']]) {
    const p = fakePlugins({ failing });
    const state = await R.pushSync({ push: p.push, support: p.support, signedOut: failing[0] === 'unregister' });
    assert.equal(state, 'failed', failing[0]);
  }
});

// -- the token, on the site --------------------------------------------------

function recorder(status = 204) {
  const requests = [];
  const fetch = async (url, init) => {
    requests.push({ url, ...init, body: init.body === undefined ? undefined : JSON.parse(init.body) });
    return { status, ok: status >= 200 && status < 300 };
  };
  return { requests, fetch };
}

test('the token goes to the site, which relays it to the API', async () => {
  const r = recorder();
  assert.equal(await R.saveToken({ fetch: r.fetch }, 'tok:en_1'), true);
  assert.equal(r.requests.length, 1);
  const [req] = r.requests;
  assert.equal(req.url, R.PUSH_TOKEN_URL);
  assert.equal(req.url, '/account/notifications/fcm-token');
  assert.equal(req.method, 'POST');
  assert.equal(req.credentials, 'same-origin');
  assert.equal(req.headers['Content-Type'], 'application/json');
  assert.deepEqual(req.body, { token: 'tok:en_1' });
});

test('a token the site refuses, or no answer, is no crash', async () => {
  assert.equal(await R.saveToken(recorder(409), 'tok'), false);
  const offline = async () => {
    throw new TypeError('Failed to fetch');
  };
  assert.equal(await R.saveToken({ fetch: offline }, 'tok'), false);
  // No token, nothing sent.
  const r = recorder();
  assert.equal(await R.saveToken({ fetch: r.fetch }, ''), false);
  assert.equal(await R.saveToken({ fetch: r.fetch }, undefined), false);
  assert.equal(r.requests.length, 0);
});

test('logging out: the site forgets the token, then Firebase does', async () => {
  const r = recorder();
  const p = fakePlugins();
  const order = [];
  const fetch = async (url, init) => (order.push('site'), r.fetch(url, init));
  p.push.unregister = async () => order.push('unregister');
  await R.forgetToken({ fetch, push: p.push }, 'tok');
  assert.deepEqual(order, ['site', 'unregister']);
  const [req] = r.requests;
  assert.equal(req.url, R.PUSH_TOKEN_URL);
  assert.equal(req.method, 'DELETE');
  assert.equal(req.credentials, 'same-origin');
  assert.equal(req.headers['Content-Type'], 'application/json');
  assert.deepEqual(req.body, { token: 'tok' });
});

test('logging out before the token came: Firebase forgets it all the same', async () => {
  const r = recorder();
  const p = fakePlugins();
  await R.forgetToken({ fetch: r.fetch, push: p.push }, null);
  assert.equal(r.requests.length, 0);
  assert.deepEqual(p.calls, ['unregister']);
});

test('logging out goes on whatever fails on the way', async () => {
  const offline = async () => {
    throw new TypeError('Failed to fetch');
  };
  const p = fakePlugins({ failing: ['unregister'] });
  // Resolves: the logout form is sent next.
  await R.forgetToken({ fetch: offline, push: p.push }, 'tok');
  await R.forgetToken({ fetch: offline, push: undefined }, 'tok');
  assert.deepEqual(p.calls, ['unregister']);
});

test('only the logout form is held back for the token', () => {
  const form = (action, method = 'post') => ({ getAttribute: (name) => ({ action, method })[name] });
  assert.equal(R.isLogoutForm(form('/logout')), true);
  assert.equal(R.isLogoutForm(form('/logout', 'POST')), true);
  for (const other of [form('/login'), form('/logout/x'), form('/account/sessions/revoke-all'), form('/logout', 'get')]) {
    assert.equal(R.isLogoutForm(other), false);
  }
  assert.equal(R.isLogoutForm(null), false);
  assert.equal(R.isLogoutForm({}), false);
});
