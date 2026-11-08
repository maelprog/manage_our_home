// What the screen of the choice of a site (choix-du-site.html) decides, at
// the first launch (#737) and when the site is changed afterwards (#738):
// store the flavour's standard site, probe a typed address, or say why not;
// once probed, store it or say why not; on a change, keep the current site
// or leave it, its reminders first. No I/O here but the reminders, through
// the plugin handed in: the page asks MainActivity's Site plugin to probe
// and to store, and shows what this file returns. Loaded by that page and
// by `node --test` (scripts/site-choice.test.mjs) alike.
//
// The probe is a GET of MANIFEST_PATH, without following redirects: the
// web manifest apps/web serves to anyone (apps/web/src/brand.rs), whose
// `name` is the app's own (capacitor.config.ts reads both from brand.rs)
// and whose `start_url` is `/`. Not `/healthz`: an empty 200 any server
// may answer.
import { normalizeSiteAddress } from './site-address.js';

export const MANIFEST_PATH = '/manifest.webmanifest';

const NOTHING_STORED = "Rien n'a été enregistré.";

// The standard site's readable name: the staging flavour's application id
// carries `.staging` (android/app/build.gradle, applicationIdSuffix).
export function standardLabel(applicationId, appName) {
  return /\.staging$/.test(applicationId) ? 'staging' : appName;
}

// `{ kind: 'standard' }` or `{ kind: 'custom', address }`, as the page
// read its form. The standard site is stored as a choice, not as its
// address, so that a later APK with another standard site moves the app
// along. A typed address is only ever probed from here.
export function readChoice({ kind, address } = {}) {
  if (kind === 'standard') {
    return { store: { standard: true } };
  }
  if (kind !== 'custom') {
    return { error: 'Choisissez le site standard ou un site personnalisé.' };
  }
  let origin;
  try {
    origin = normalizeSiteAddress(address);
  } catch (e) {
    return { error: e.message };
  }
  return { probe: origin, url: `${origin}${MANIFEST_PATH}` };
}

// What the Site plugin's probe of `origin` gave back, `probe`: either
// `{ failure }` (certificate, unknown-host, timeout, unreachable) or the
// answer, `{ status, location?, body }`; null when the page did not probe,
// the phone being offline. `online` is navigator.onLine, which only an
// offline WebView that knows it makes false.
export function judgeProbe(origin, probe, { online, appName }) {
  if (online === false) {
    return {
      error: `Le téléphone n'est pas connecté à Internet. ${NOTHING_STORED} Connectez-le, puis réessayez.`,
    };
  }
  const host = new URL(origin).host;
  if (probe && typeof probe.failure === 'string') {
    if (probe.failure === 'certificate') {
      return {
        error: `Le certificat de ${host} est refusé : le téléphone ne lui fait pas confiance. ${NOTHING_STORED}`,
      };
    }
    // Without a network, the WebView may still say it is online: the
    // phone's connection is named along with the address.
    if (probe.failure === 'unknown-host') {
      return {
        error: `Aucun site ne porte ce nom, ou le téléphone n'est pas connecté à Internet : vérifiez l'adresse et la connexion. ${NOTHING_STORED}`,
      };
    }
    return {
      error: `${host} ne répond pas. Vérifiez l'adresse et que le téléphone est connecté à Internet, ou réessayez plus tard. ${NOTHING_STORED}`,
    };
  }
  const status = probe?.status;
  if (Number.isInteger(status) && status >= 300 && status < 400) {
    const target = redirectHost(probe.location, origin);
    return {
      error: target
        ? `${host} renvoie vers une autre adresse : tapez plutôt ${target}. ${NOTHING_STORED}`
        : `${host} renvoie vers une autre adresse. ${NOTHING_STORED}`,
    };
  }
  if (status === 200 && isManifestOf(probe.body, appName)) {
    return { store: { origin } };
  }
  const answered = Number.isInteger(status) && status !== 200 ? ` (réponse ${status})` : '';
  return { error: `${host} répond, mais ce n'est pas un site ${appName}${answered}. ${NOTHING_STORED}` };
}

// The host of another https site a redirect leads to, if it does.
function redirectHost(location, origin) {
  if (typeof location !== 'string') return null;
  let url;
  try {
    url = new URL(location, origin);
  } catch {
    return null;
  }
  return url.protocol === 'https:' && url.origin !== origin ? url.host : null;
}

function isManifestOf(body, appName) {
  let manifest;
  try {
    manifest = JSON.parse(body);
  } catch {
    return false;
  }
  return manifest !== null && typeof manifest === 'object' && manifest.name === appName && manifest.start_url === '/';
}

// The choice stored, `current` (`{ standard: true }`, `{ origin }`, or
// nothing at the first launch, as the Site plugin's `info` gives it), and
// the one just made, `store`: `first`, nothing to leave; `keep`, the same
// site, whose session and reminders stay; `change`, another site.
export function changeKind(current, store) {
  const standard = current?.standard === true;
  const origin = typeof current?.origin === 'string' ? current.origin : null;
  if (!standard && origin === null) return 'first';
  if (standard ? store?.standard === true : store?.origin === origin) return 'keep';
  return 'change';
}

// The current site's readable name: the standard site's, or its host;
// null when there is none.
export function currentLabel(current, applicationId, appName) {
  if (current?.standard === true) return standardLabel(applicationId, appName);
  if (typeof current?.origin !== 'string') return null;
  try {
    return new URL(current.origin).host;
  } catch {
    return null;
  }
}

// Leaving a site: its reminders, scheduled on the phone by
// apps/web/src/local-reminders.js, would still fire, and a touch would open
// the old site's event on the new one. Cancels those scheduled, then clears
// those already shown, through the Local Notifications plugin: `{}`, or
// `{ error }` if either fails, and the change is then not made. No plugin,
// no reminder was ever scheduled.
export async function forgetReminders(plugin) {
  if (plugin === undefined || plugin === null) return {};
  try {
    await plugin.cancelAll();
    await plugin.removeAllDeliveredNotifications();
  } catch {
    return { error: `Les rappels de l'ancien site n'ont pas pu être retirés du téléphone. ${NOTHING_STORED} Réessayez.` };
  }
  return {};
}
