// The version a signed APK gets from its tag (#666), and the tag a release
// started by hand gets (#742): see release-version.mjs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { nextTag, releaseVersion } from './release-version.mjs';

test('a mobile-v tag gives the versionName and a versionCode read off its three numbers', () => {
  assert.deepEqual(releaseVersion('mobile-v1.0.0', []), { versionName: '1.0.0', versionCode: 1_000_000 });
  assert.deepEqual(releaseVersion('mobile-v0.1.0', []), { versionName: '0.1.0', versionCode: 1_000 });
  assert.deepEqual(releaseVersion('mobile-v2.13.7', []), { versionName: '2.13.7', versionCode: 2_013_007 });
  assert.deepEqual(releaseVersion('mobile-v2100.0.0', []), { versionName: '2100.0.0', versionCode: 2_100_000_000 });
});

test('the versionCode grows with the SemVer order', () => {
  const ordered = ['0.0.1', '0.1.0', '0.1.999', '0.2.0', '0.999.999', '1.0.0', '1.0.1', '1.10.0', '10.0.0'];
  const codes = ordered.map((v) => releaseVersion(`mobile-v${v}`, []).versionCode);
  assert.deepEqual(codes, [...codes].sort((a, b) => a - b));
  assert.equal(new Set(codes).size, codes.length);
});

test('anything but mobile-vX.Y.Z is refused', () => {
  for (const tag of [
    'v1.0.0',
    'mobile-1.0.0',
    'mobile-v1.0',
    'mobile-v1.0.0.0',
    'mobile-v1.0.0-rc.1',
    'mobile-v1.0.0+build',
    'mobile-v01.0.0',
    'mobile-v1.00.0',
    'mobile-v 1.0.0',
    'mobile-v1.0.0 ',
    '',
  ]) {
    assert.throws(() => releaseVersion(tag, []), /expected mobile-vX\.Y\.Z/, tag);
  }
});

test('a version Android cannot number is refused', () => {
  // versionCode 0 is not a version Android accepts.
  assert.throws(() => releaseVersion('mobile-v0.0.0', []), /0\.0\.0/);
  // Minor and patch take three digits each, the major what is left under
  // Android's ceiling of 2 100 000 000.
  assert.throws(() => releaseVersion('mobile-v1.1000.0', []), /999/);
  assert.throws(() => releaseVersion('mobile-v1.0.1000', []), /999/);
  assert.throws(() => releaseVersion('mobile-v2100.0.1', []), /2100000000/);
  assert.throws(() => releaseVersion('mobile-v99999999999999999999.0.0', []), /2100000000/);
});

test('the versionCode is above every other mobile-v tag', () => {
  const existing = ['mobile-v1.0.0', 'mobile-v1.2.0', 'v3.0.0', 'mobile-v1.1.5'];
  assert.equal(releaseVersion('mobile-v1.2.1', existing).versionCode, 1_002_001);
  assert.equal(releaseVersion('mobile-v2.0.0', existing).versionCode, 2_000_000);
  // The tag being released is among the repository's tags: it is not a
  // previous release.
  assert.equal(releaseVersion('mobile-v1.2.0', existing).versionCode, 1_002_000);
  // A version below or equal to a released one would not install over it.
  assert.throws(() => releaseVersion('mobile-v1.1.9', existing), /mobile-v1\.2\.0/);
  assert.throws(() => releaseVersion('mobile-v0.9.0', existing), /mobile-v1\.2\.0/);
  // Tags that are not mobile-vX.Y.Z never made a release, and do not count.
  assert.equal(
    releaseVersion('mobile-v1.0.1', ['mobile-v9.0.0-rc.1', 'mobile-v1.0.0', 'v9.0.0', 'mobile-v1.1000.0']).versionCode,
    1_000_001,
  );
});

test('the first release started by hand is mobile-v0.1.0', () => {
  assert.equal(nextTag([]), 'mobile-v0.1.0');
  // Tags that are not mobile-vX.Y.Z never made a release.
  assert.equal(nextTag(['v1.0.0', 'mobile-v1.0.0-rc.1', 'mobile-v01.0.0', 'mobile-v0.0.0']), 'mobile-v0.1.0');
});

test('the next tag is the patch after the highest mobile-v tag, in SemVer order', () => {
  assert.equal(nextTag(['mobile-v0.1.0']), 'mobile-v0.1.1');
  // 0.1.10 is above 0.1.9, although it sorts below it as text.
  assert.equal(nextTag(['mobile-v0.1.9', 'mobile-v0.1.10', 'mobile-v0.1.2']), 'mobile-v0.1.11');
  assert.equal(nextTag(['mobile-v1.2.0', 'mobile-v0.9.999', 'mobile-v1.10.0', 'mobile-v1.9.5']), 'mobile-v1.10.1');
  assert.equal(nextTag(['mobile-v10.0.0', 'mobile-v9.99.99', 'mobile-v2.0.0']), 'mobile-v10.0.1');
  // Invalid tags are left out, even when they would sort above.
  assert.equal(nextTag(['mobile-v0.1.3', 'mobile-v9.0.0-rc.1', 'mobile-v1.1000.0', 'v9.0.0']), 'mobile-v0.1.4');
  // The tag it gives is one releaseVersion takes.
  const existing = ['mobile-v0.1.9', 'mobile-v0.1.10'];
  assert.equal(releaseVersion(nextTag(existing), existing).versionCode, 1_011);
});

test('past patch 999 the next tag has to be chosen by hand', () => {
  assert.throws(() => nextTag(['mobile-v0.1.999']), /mobile-v0\.1\.999.*999.*mobile-v0\.2\.0/s);
  assert.throws(() => nextTag(['mobile-v1.0.0', 'mobile-v2.3.999']), /mobile-v2\.4\.0/);
  // The minor is at its limit too: the hint is the next major.
  assert.throws(() => nextTag(['mobile-v2.999.999']), /mobile-v3\.0\.0/);
});
