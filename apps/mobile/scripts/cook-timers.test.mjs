// The cooking timers of the page handed to the app's native alarm (#1019):
// the part of apps/web/src/cook-watch.js that decides, from the timers the
// page Cuisiner keeps (`mhome.cuisine.<id>`), which alarms of the
// CookTimers plugin (#1017, CookTimersPlugin.java) to schedule and which
// to cancel, and whether the page's Web Audio beep rings. It runs here in a
// bare `vm` context, with no `window`: nothing starts on its own, and the
// plugin is a fake that records its calls. `npm test` in apps/mobile.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const SOURCE = readFileSync(new URL('../../web/src/cook-watch.js', import.meta.url), 'utf8');

function load() {
  const context = vm.createContext({});
  vm.runInContext(SOURCE, context, { filename: 'cook-watch.js' });
  const api = vm.runInContext('MHomeCookWatch', context);
  assert.ok(api, 'the script defines MHomeCookWatch');
  return api;
}

const W = load();
// Made in the script's own context: compared by value, out of it.
const plain = (value) => JSON.parse(JSON.stringify(value));

const A = '7b1c2d44-5a0e-4f1b-9c3a-0d6e8f21a977';
const B = 'ef5a0c8e-b6d9-4c62-9051-0ebbcd646b24';
const NOW = 1_800_000_000_000;
const MIN = 60_000;

// What the page Cuisiner keeps for a recipe, read as cook-watch reads it.
function entry(id, title, timers) {
  return W.entry(W.PREFIX + id, JSON.stringify({ at: NOW - MIN, step: 0, title, timers }), NOW);
}

const RUNNING = { id: '1800000000000', what: 'Cuisson des pâtes', end: NOW + 8 * MIN, left: 0, done: false };
const PAUSED = { id: '1800000000001', what: 'Repos', end: 0, left: 5 * MIN, done: false };
const DUE = { id: '1800000000002', what: 'Four', end: NOW - 1000, left: 0, done: false };
const DONE = { id: '1800000000003', what: 'Sauce', end: NOW - 2 * MIN, left: 0, done: true };

// A fake plugin: what it was asked, in order; `answers` for `schedule`.
function plugin(answer = { scheduled: true, exact: true, notifications: true }) {
  const calls = [];
  return {
    calls,
    schedule(options) {
      calls.push(['schedule', plain(options)]);
      return typeof answer === 'function' ? answer(options) : Promise.resolve(answer);
    },
    cancel(options) {
      calls.push(['cancel', plain(options)]);
      return Promise.resolve();
    },
    stop() {
      calls.push(['stop']);
      return Promise.resolve();
    },
  };
}

// -- the plugin --------------------------------------------------------------

test('the plugin is the one the Capacitor bridge puts in the page, and nothing else', () => {
  const timers = plugin();
  assert.equal(W.alarmPlugin({ Capacitor: { Plugins: { CookTimers: timers } } }), timers);
  // A browser, or an APK built before #1017: no plugin.
  for (const win of [{}, { Capacitor: {} }, { Capacitor: { Plugins: {} } }, null, undefined]) {
    assert.equal(W.alarmPlugin(win), null, JSON.stringify(win));
  }
});

// -- one timer, one alarm ----------------------------------------------------

test("an alarm's id is the recipe's and the timer's, as the plugin takes it", () => {
  const id = W.nativeId(A, RUNNING.id);
  assert.equal(id, `${A}_${RUNNING.id}`);
  assert.match(id, /^[A-Za-z0-9_-]{1,64}$/);
});

test('a timer whose id the plugin would refuse has no alarm', () => {
  for (const timer of ['', 'a b', 'é', 'x'.repeat(30), null, 42]) {
    assert.equal(W.nativeId(A, timer), null, String(timer));
  }
  assert.equal(W.nativeId('../x', '1'), null);
});

test('an alarm for each timer that runs or rang, none for one in pause', () => {
  const alarms = plain(W.alarms([entry(A, 'Pâtes à la carbonara', [RUNNING, PAUSED, DUE, DONE])]));
  assert.deepEqual(alarms, {
    [`${A}_${RUNNING.id}`]: {
      id: `${A}_${RUNNING.id}`,
      endAt: RUNNING.end,
      title: 'Pâtes à la carbonara',
      text: 'Cuisson des pâtes',
      url: `/recipes/${A}/cook`,
    },
    // Due, it rings: cancelling it would silence it.
    [`${A}_${DUE.id}`]: {
      id: `${A}_${DUE.id}`,
      endAt: DUE.end,
      title: 'Pâtes à la carbonara',
      text: 'Four',
      url: `/recipes/${A}/cook`,
    },
    // Over and not yet stopped: it goes on ringing until « Arrêter ».
    [`${A}_${DONE.id}`]: {
      id: `${A}_${DONE.id}`,
      endAt: DONE.end,
      title: 'Pâtes à la carbonara',
      text: 'Sauce',
      url: `/recipes/${A}/cook`,
    },
  });
});

test('the alarms of every recipe kept, none for what is not kept', () => {
  const alarms = W.alarms([entry(A, 'Pâtes', [RUNNING]), entry(B, 'Gâteau', [{ ...RUNNING, id: '7' }]), null]);
  assert.deepEqual(Object.keys(alarms).sort(), [`${A}_${RUNNING.id}`, `${B}_7`].sort());
  assert.deepEqual(plain(W.alarms([])), {});
});

// -- what to schedule, what to cancel ----------------------------------------

test('a timer started is scheduled; one already scheduled at its end is left alone', () => {
  const alarms = W.alarms([entry(A, 'Pâtes', [RUNNING])]);
  const id = `${A}_${RUNNING.id}`;
  assert.deepEqual(plain(W.alarmPlan(alarms, {})), { schedule: [plain(alarms[id])], cancel: [] });
  assert.deepEqual(plain(W.alarmPlan(alarms, { [id]: RUNNING.end })), { schedule: [], cancel: [] });
});

test('a timer paused, stopped or forgotten is cancelled', () => {
  const id = `${A}_${RUNNING.id}`;
  const kept = { [id]: RUNNING.end };
  // Paused.
  const paused = W.alarms([entry(A, 'Pâtes', [{ ...RUNNING, end: 0, left: 3 * MIN }])]);
  assert.deepEqual(plain(W.alarmPlan(paused, kept)), { schedule: [], cancel: [id] });
  // Stopped (« Arrêter »), or the recipe finished (« Terminer »).
  assert.deepEqual(plain(W.alarmPlan(W.alarms([entry(A, 'Pâtes', [])]), kept)), { schedule: [], cancel: [id] });
  assert.deepEqual(plain(W.alarmPlan(W.alarms([]), kept)), { schedule: [], cancel: [id] });
});

test('a timer resumed is scheduled again, at its new end', () => {
  const id = `${A}_${RUNNING.id}`;
  const resumed = W.alarms([entry(A, 'Pâtes', [{ ...RUNNING, end: NOW + 3 * MIN }])]);
  assert.deepEqual(plain(W.alarmPlan(resumed, { [id]: RUNNING.end })), { schedule: [plain(resumed[id])], cancel: [] });
});

// -- one synchronisation -----------------------------------------------------

test('a synchronisation calls the plugin by the plan and keeps what it scheduled', async () => {
  const timers = plugin();
  const gone = `${B}_9`;
  const record = await W.syncAlarms(timers, W.alarms([entry(A, 'Pâtes', [RUNNING])]), { ids: { [gone]: NOW } });
  const id = `${A}_${RUNNING.id}`;
  assert.deepEqual(timers.calls, [
    ['cancel', { id: gone }],
    ['schedule', { id, endAt: RUNNING.end, title: 'Pâtes', text: 'Cuisson des pâtes', url: `/recipes/${A}/cook` }],
  ]);
  assert.deepEqual(plain(record), { ids: { [id]: RUNNING.end }, notifications: true });
  // Nothing changed: nothing called.
  const again = plugin();
  assert.deepEqual(plain(await W.syncAlarms(again, W.alarms([entry(A, 'Pâtes', [RUNNING])]), record)), plain(record));
  assert.deepEqual(again.calls, []);
});

test('notifications refused to the app are kept: the native alarm then rings nothing', async () => {
  const timers = plugin({ scheduled: true, exact: true, notifications: false });
  const record = await W.syncAlarms(timers, W.alarms([entry(A, 'Pâtes', [RUNNING])]), {});
  assert.equal(record.notifications, false);
});

test('a refusal of the plugin is kept as scheduled: not asked again every half second', async () => {
  const timers = plugin(() => Promise.reject(new Error('id')));
  const record = await W.syncAlarms(timers, W.alarms([entry(A, 'Pâtes', [RUNNING])]), {});
  assert.deepEqual(plain(record.ids), { [`${A}_${RUNNING.id}`]: RUNNING.end });
});

test('what was kept unreadable is read as nothing scheduled', async () => {
  for (const kept of [null, undefined, 'x', 42, { ids: 'x' }]) {
    const timers = plugin();
    const record = await W.syncAlarms(timers, W.alarms([entry(A, 'Pâtes', [RUNNING])]), kept);
    assert.equal(timers.calls.length, 1, JSON.stringify(kept));
    assert.equal(timers.calls[0][0], 'schedule');
    assert.equal(Object.keys(record.ids).length, 1);
  }
});

// -- one ringing at a time ---------------------------------------------------

function win(timers, kept) {
  const store = kept === undefined ? {} : { [W.NATIVE_KEY]: JSON.stringify(kept) };
  return {
    Capacitor: timers ? { Plugins: { CookTimers: timers } } : undefined,
    localStorage: { getItem: (key) => (key in store ? store[key] : null) },
  };
}

test('without the plugin, the page beeps as before', () => {
  assert.equal(W.ringsNatively(win(null)), false);
  assert.equal(W.ringsNatively({}), false);
  assert.equal(W.ringsNatively(undefined), false);
});

test('with the plugin, the alarm rings and the page does not beep', () => {
  assert.equal(W.ringsNatively(win(plugin())), true);
  assert.equal(W.ringsNatively(win(plugin(), { ids: {}, notifications: true })), true);
});

test('notifications refused to the app: the native alarm rings nothing, the page beeps', () => {
  assert.equal(W.ringsNatively(win(plugin(), { ids: {}, notifications: false })), false);
});

test('storage that throws does not keep the alarm from ringing', () => {
  const broken = { Capacitor: { Plugins: { CookTimers: plugin() } }, get localStorage() { throw new Error('denied'); } };
  assert.equal(W.ringsNatively(broken), true);
});

// -- the permission, on a button ---------------------------------------------

test("the app's « Prévenir à la fin »: a button until Android answers, then nothing", () => {
  assert.equal(W.appNotifyState('prompt'), 'ask');
  assert.equal(W.appNotifyState('prompt-with-rationale'), 'ask');
  assert.equal(W.appNotifyState('granted'), 'none');
  // Refused, Android no longer asks: the button would do nothing.
  assert.equal(W.appNotifyState('denied'), 'none');
  assert.equal(W.appNotifyState(undefined), 'none');
});
