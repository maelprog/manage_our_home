// The version of a signed APK, read off the tag that releases it (#666,
// .github/workflows/mobile-release.yml).
//
// A tag mobile-vX.Y.Z gives versionName X.Y.Z and versionCode
// X·1 000 000 + Y·1 000 + Z: the code grows with the SemVer order, which is
// what Android needs to install an update over the previous APK. Minor and
// patch take three digits each; Android caps the code at 2 100 000 000.
// No pre-release suffix: 1.0.0-rc.1 would need a code below 1.0.0's, and
// the next one above it.
//
// The code also has to be above every APK already released, or the update
// is refused on the phones: the tags the repository already has are
// checked, the one being released aside.
//
// From the workflow: `node scripts/release-version.mjs <tag> [<tag>…]`, the
// tag to release then the repository's mobile-v tags; prints
// `version_name=…` and `version_code=…` for $GITHUB_OUTPUT, or the reason
// on stderr with a non-zero exit.
import { pathToFileURL } from 'node:url';

const TAG = /^mobile-v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const MAX_VERSION_CODE = 2_100_000_000;

// The versionName and versionCode of one tag, or why it cannot have any.
function versionOf(tag) {
  const found = TAG.exec(tag);
  if (!found) {
    throw new Error(`tag ${JSON.stringify(tag)}: expected mobile-vX.Y.Z (no pre-release, no leading zero)`);
  }
  const [major, minor, patch] = found.slice(1).map(Number);
  if (minor > 999 || patch > 999) {
    throw new Error(`tag ${tag}: minor and patch go up to 999`);
  }
  const versionCode = major * 1_000_000 + minor * 1_000 + patch;
  if (versionCode > MAX_VERSION_CODE) {
    throw new Error(`tag ${tag}: versionCode ${versionCode} is above Android's ${MAX_VERSION_CODE}`);
  }
  if (versionCode === 0) {
    throw new Error(`tag ${tag}: 0.0.0 gives versionCode 0, which Android refuses`);
  }
  return { versionName: found.slice(1).join('.'), versionCode };
}

export function releaseVersion(tag, existingTags) {
  const version = versionOf(tag);
  // A tag that cannot have a version never made a release: it does not count.
  const previous = existingTags
    .filter((other) => other !== tag)
    .flatMap((other) => {
      try {
        return [{ tag: other, ...versionOf(other) }];
      } catch {
        return [];
      }
    });
  const latest = previous.reduce((a, b) => (b.versionCode > a.versionCode ? b : a), { versionCode: 0 });
  if (latest.versionCode >= version.versionCode) {
    throw new Error(
      `tag ${tag}: versionCode ${version.versionCode} is not above ${latest.versionCode}, of ${latest.tag}; ` +
        'a phone that has that release would refuse this one',
    );
  }
  return version;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [tag, ...existingTags] = process.argv.slice(2);
  try {
    const { versionName, versionCode } = releaseVersion(tag ?? '', existingTags);
    process.stdout.write(`version_name=${versionName}\nversion_code=${versionCode}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
