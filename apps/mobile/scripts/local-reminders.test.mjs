// The local reminders of the app (#668): apps/web/src/local-reminders.js,
// the script apps/web serves to the app's pages only. It runs in the
// WebView, against the Local Notifications plugin; here it runs in a bare
// `vm` context, with the plugin, `fetch` and the clock replaced, so that
// what it schedules can be read back. `npm test` in apps/mobile.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const SOURCE = readFileSync(new URL('../../web/src/local-reminders.js', import.meta.url), 'utf8');

// The script's API, as its file defines it, in a context with no `window`:
// nothing starts on its own.
function load() {
  const context = vm.createContext({});
  vm.runInContext(SOURCE, context, { filename: 'local-reminders.js' });
  const api = vm.runInContext('MHomeLocalReminders', context);
  assert.ok(api, 'the script defines MHomeLocalReminders');
  return api;
}

const R = load();

const ENTRY = {
  id: '0f8fad5b-d9cb-469f-a165-70867728950e',
  event_id: '7c9e6679-7425-40de-944b-e07fc1f90ae7',
  group_id: '16fd2706-8baf-433b-82eb-8c7fada847da',
  title: 'Dentiste',
  occurrence_at: '2026-11-10T13:30:00Z',
  // The day before.
  fire_at: '2026-11-09T13:30:00Z',
  all_day: false,
};

const NOW = Date.parse('2026-11-07T09:00:00Z');
const PARIS = { timeZone: 'Europe/Paris', now: NOW, exact: true };

// -- the identifier ----------------------------------------------------------

test('the identifier is a 32-bit integer, the same for the same UUID', () => {
  const id = R.notificationId(ENTRY.id);
  assert.equal(typeof id, 'number');
  assert.ok(Number.isInteger(id));
  assert.ok(id >= -2147483648 && id <= 2147483647, String(id));
  assert.equal(R.notificationId(ENTRY.id), id);
  // Case and the dashes do not count: it is the UUID's value.
  assert.equal(R.notificationId(ENTRY.id.toUpperCase()), id);
});

test('the identifier depends on every part of the UUID', () => {
  const base = R.notificationId('00000000-0000-4000-8000-000000000000');
  for (const other of [
    '00000001-0000-4000-8000-000000000000',
    '00000000-0001-4000-8000-000000000000',
    '00000000-0000-4000-8001-000000000000',
    '00000000-0000-4000-8000-000000000001',
  ]) {
    assert.notEqual(R.notificationId(other), base, other);
  }
});

test('the identifier is never the one the plugin reads as "no notification"', () => {
  // The plugin tells an activity opened by a notification from one that is
  // not by the id it carries: Int.MIN_VALUE means none.
  const words = '80000000-0000-0000-0000-000000000000';
  assert.notEqual(R.notificationId(words), -2147483648);
});

test('what is not a UUID has no identifier', () => {
  for (const text of ['', 'abc', '0f8fad5b-d9cb-469f-a165-70867728950', 42, null, undefined]) {
    assert.equal(R.notificationId(text), null, String(text));
  }
});

// -- one notification --------------------------------------------------------

test("a timed occurrence: the event's title, its time in the phone's zone", () => {
  const n = R.toNotification(ENTRY, PARIS);
  assert.equal(n.id, R.notificationId(ENTRY.id));
  assert.equal(n.title, 'Dentiste');
  // 13:30 UTC is 14:30 in Paris in November.
  assert.equal(n.body, 'À 14:30');
  assert.equal(n.schedule.at.getTime(), Date.parse(ENTRY.fire_at));
  assert.equal(n.schedule.allowWhileIdle, true);
  assert.equal(n.isExactNotification, true);
});

const NEW_YORK = { ...PARIS, timeZone: 'America/New_York' };

test('the time is the one of the zone the phone is in', () => {
  assert.equal(R.toNotification(ENTRY, NEW_YORK).body, 'À 08:30');
});

test('the text is the time alone, whatever day the reminder fires', () => {
  // Title and time only (choice of 2026-11-01, confirmed 2026-11-07): no
  // date, neither on the day of the occurrence nor before it.
  for (const fire_at of ['2026-11-10T12:30:00Z', '2026-11-09T13:30:00Z', '2026-11-03T13:30:00Z']) {
    assert.equal(R.toNotification({ ...ENTRY, fire_at }, PARIS).body, 'À 14:30', fire_at);
  }
});

test('the time is read in the zone of the phone, not in UTC', () => {
  // 23:30 UTC on the 9th is 00:30 on the 10th in Paris.
  const n = R.toNotification(
    { ...ENTRY, occurrence_at: '2026-11-09T23:30:00Z', fire_at: '2026-11-09T22:00:00Z' },
    PARIS,
  );
  assert.equal(n.body, 'À 00:30');
});

test('a late occurrence in New York: its New York time, no date', () => {
  // 03:00 UTC on the 10th is 22:00 on the 9th in New York, 04:00 on the
  // 10th in Paris.
  const late = { ...ENTRY, occurrence_at: '2026-11-10T03:00:00Z' };
  for (const fire_at of ['2026-11-08T15:00:00Z', '2026-11-09T20:00:00Z']) {
    assert.equal(R.toNotification({ ...late, fire_at }, NEW_YORK).body, 'À 22:00', fire_at);
  }
});

test('an all-day occurrence says « toute la journée », and no date', () => {
  // An all-day occurrence starts at the Paris midnight opening its day
  // (apps/api, recurrence::all_day_anchor): 23:00 UTC on the 9th is the 10th.
  const allDay = { ...ENTRY, all_day: true, occurrence_at: '2026-11-09T23:00:00Z' };
  for (const fire_at of ['2026-11-08T17:00:00Z', '2026-11-10T02:00:00Z', '2026-11-10T07:00:00Z']) {
    for (const zone of [PARIS, NEW_YORK]) {
      assert.equal(
        R.toNotification({ ...allDay, fire_at }, zone).body,
        'Toute la journée',
        `${fire_at} ${zone.timeZone}`,
      );
    }
  }
});

test('an entry it cannot read gives no notification', () => {
  for (const [what, entry] of [
    ['id', { ...ENTRY, id: 'nope' }],
    ['fire_at', { ...ENTRY, fire_at: 'demain' }],
    ['occurrence_at', { ...ENTRY, occurrence_at: null }],
    ['title', { ...ENTRY, title: 7 }],
    ['title', { ...ENTRY, title: undefined }],
    ['event_id', { ...ENTRY, event_id: 'x' }],
    ['group_id', { ...ENTRY, group_id: undefined }],
  ]) {
    assert.equal(R.toNotification(entry, PARIS), null, what);
  }
  assert.ok(R.toNotification(ENTRY, PARIS));
});

test('a touch opens the event in the agenda, in its family, on its occurrence', () => {
  const n = R.toNotification(ENTRY, PARIS);
  assert.equal(
    n.extra.url,
    `/agenda/${ENTRY.event_id}?famille=${ENTRY.group_id}&occ=2026-11-10T13:30:00Z`,
  );
});

test('exact alarms refused: the notification asks for none', () => {
  // With exact wanted and not allowed, the plugin opens Android's settings
  // on each schedule: it must not be asked.
  assert.equal(R.toNotification(ENTRY, { ...PARIS, exact: false }).isExactNotification, false);
});

// -- the whole list ----------------------------------------------------------

test('the plan keeps what is yet to fire, in order, once per identifier', () => {
  const later = { ...ENTRY, id: '1f8fad5b-d9cb-469f-a165-70867728950e', fire_at: '2026-11-11T08:00:00Z' };
  const past = { ...ENTRY, id: '2f8fad5b-d9cb-469f-a165-70867728950e', fire_at: '2026-11-07T08:59:00Z' };
  const now = { ...ENTRY, id: '3f8fad5b-d9cb-469f-a165-70867728950e', fire_at: '2026-11-07T09:00:00Z' };
  const twice = { ...ENTRY, title: 'Doublon' };
  const plan = R.plan([later, ENTRY, past, now, twice], PARIS);
  assert.deepEqual(
    plan.map((n) => n.title),
    ['Dentiste', 'Dentiste'],
  );
  assert.deepEqual(
    plan.map((n) => n.id),
    [R.notificationId(ENTRY.id), R.notificationId(later.id)],
  );
});

test('the plan drops what it cannot read', () => {
  const plan = R.plan(
    [
      { ...ENTRY, id: 'nope' },
      { ...ENTRY, fire_at: 'demain' },
      { ...ENTRY, occurrence_at: null },
      { ...ENTRY, title: 7 },
      null,
      ENTRY,
    ],
    PARIS,
  );
  assert.equal(plan.length, 1);
  assert.equal(R.plan(null, PARIS).length, 0);
  assert.equal(R.plan({}, PARIS).length, 0);
});

test('the number scheduled, in words', () => {
  assert.equal(R.countText(0), 'Aucun rappel programmé sur ce téléphone.');
  assert.equal(R.countText(1), '1 rappel programmé sur ce téléphone.');
  assert.equal(R.countText(12), '12 rappels programmés sur ce téléphone.');
});

test('only a link into the agenda is followed on a touch', () => {
  const url = `/agenda/${ENTRY.event_id}?famille=${ENTRY.group_id}`;
  assert.equal(R.touchTarget({ notification: { extra: { url } } }), url);
  for (const bad of ['https://evil.example/agenda/x', '//evil.example/agenda/', '/account', 'javascript:alert(1)', 7]) {
    assert.equal(R.touchTarget({ notification: { extra: { url: bad } } }), null, String(bad));
  }
  assert.equal(R.touchTarget({}), null);
  assert.equal(R.touchTarget(null), null);
});

// -- the synchronisation -----------------------------------------------------

// A Local Notifications plugin that records its calls.
function fakePlugin({ display = 'granted', exact = 'granted' } = {}) {
  const calls = [];
  return {
    calls,
    checkPermissions: async () => (calls.push(['checkPermissions']), { display }),
    checkExactNotificationSetting: async () => (calls.push(['checkExact']), { exact_alarm: exact }),
    cancelAll: async () => calls.push(['cancelAll']),
    schedule: async (options) => (calls.push(['schedule', options]), { notifications: [] }),
    requestPermissions: async () => (calls.push(['requestPermissions']), { display }),
  };
}

function answer(status, body) {
  return async (url, init) => {
    assert.equal(url, R.UPCOMING_URL);
    assert.equal(init.credentials, 'same-origin');
    return { status, ok: status >= 200 && status < 300, json: async () => body };
  };
}

const names = (calls) => calls.map(([name]) => name);
// What `sync` resolves to, made in the script's own context: compared by
// value, out of that context.
const plain = (value) => JSON.parse(JSON.stringify(value));

test('sync: cancels what the app scheduled, then schedules the list', async () => {
  const plugin = fakePlugin();
  const result = await R.sync({ plugin, fetch: answer(200, [ENTRY]), ...PARIS });
  assert.deepEqual(names(plugin.calls), ['checkPermissions', 'checkExact', 'cancelAll', 'schedule']);
  const [, options] = plugin.calls[3];
  assert.deepEqual(options.notifications.map((n) => n.id), [R.notificationId(ENTRY.id)]);
  assert.deepEqual(plain(result), { state: 'scheduled', count: 1, exact: true });
});

test('sync: a member on email gets an empty list, and nothing is scheduled', async () => {
  const plugin = fakePlugin();
  const result = await R.sync({ plugin, fetch: answer(200, []), ...PARIS });
  assert.deepEqual(names(plugin.calls), ['checkPermissions', 'checkExact', 'cancelAll']);
  assert.equal(result.count, 0);
});

test('sync: logged out, or the account gone, cancels every reminder', async () => {
  const plugin = fakePlugin();
  const result = await R.sync({ plugin, fetch: answer(401, { error: 'unauthorized' }), ...PARIS });
  assert.deepEqual(names(plugin.calls), ['cancelAll']);
  assert.deepEqual(plain(result), { state: 'signed-out', count: 0 });
});

test('sync: offline or a server error leaves what is scheduled as it is', async () => {
  for (const fetch of [
    answer(502, null),
    answer(500, null),
    async () => {
      throw new TypeError('Failed to fetch');
    },
    answer(200, { not: 'a list' }),
  ]) {
    const plugin = fakePlugin();
    const result = await R.sync({ plugin, fetch, ...PARIS });
    assert.deepEqual(names(plugin.calls), []);
    assert.equal(result.state, 'unreachable');
  }
});

test('sync: without the permission, nothing is scheduled and no prompt opens', async () => {
  for (const display of ['prompt', 'prompt-with-rationale', 'denied']) {
    const plugin = fakePlugin({ display });
    const result = await R.sync({ plugin, fetch: answer(200, [ENTRY]), ...PARIS });
    // schedule() would ask for the permission itself: never called.
    assert.deepEqual(names(plugin.calls), ['checkPermissions', 'cancelAll'], display);
    assert.deepEqual(plain(result), { state: 'not-allowed', count: 0, display });
  }
});

test('sync: exact alarms refused, the reminders are scheduled inexact', async () => {
  const plugin = fakePlugin({ exact: 'denied' });
  const result = await R.sync({ plugin, fetch: answer(200, [ENTRY]), ...PARIS });
  const [, options] = plugin.calls.find(([name]) => name === 'schedule');
  assert.equal(options.notifications[0].isExactNotification, false);
  assert.equal(result.exact, false);
});

test('sync: a plugin that fails is a failure, not a crash', async () => {
  const plugin = fakePlugin();
  plugin.schedule = async () => {
    throw new Error('boom');
  };
  const result = await R.sync({ plugin, fetch: answer(200, [ENTRY]), ...PARIS });
  assert.equal(result.state, 'failed');
});
