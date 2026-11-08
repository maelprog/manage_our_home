// The page of the choice of a site (choix-du-site.html), at the first
// launch (#737) and when the site is changed afterwards (#738): reads the
// form, lets site-choice.js decide, and hands the probe and the storing to
// MainActivity's Site plugin. Storing a choice, or keeping the current
// one, restarts the activity on that site: this page is then gone.
import { changeKind, currentLabel, forgetReminders, judgeProbe, readChoice, standardLabel } from './site-choice.js';

const site = window.Capacitor?.Plugins?.Site;
const standard = document.getElementById('standard');
const keep = document.getElementById('keep');
const form = document.getElementById('custom');
const address = document.getElementById('address');
const error = document.getElementById('address-error');
const status = document.getElementById('status');
let appName = 'MHome';
// The choice stored before this page, if the site is being changed.
let current = null;

function busy(text) {
  status.textContent = text;
  for (const button of [standard, keep, form.querySelector('button')]) button.disabled = text !== '';
}

// A typed address refused: the message is the field's.
function refuse(message) {
  busy('');
  error.textContent = message;
  address.setAttribute('aria-invalid', 'true');
  address.focus();
}

// Anything else that went wrong: said in the status line, the field left
// as it is.
function fail(message) {
  busy('');
  status.textContent = message;
}

async function reopen(onError) {
  busy('Ouverture du site…');
  try {
    await site.keep();
  } catch {
    onError("Le site actuel n'a pas pu être rouvert. Réessayez.");
  }
}

// `onError` says the failure: refuse for a typed address, fail for the
// standard site. The current site chosen again is kept as it is; another
// one leaves it, its reminders first (the Site plugin then clears its
// session).
async function store(choice, onError) {
  const kind = changeKind(current, choice);
  if (kind === 'keep') {
    await reopen(onError);
    return;
  }
  busy('Ouverture du site…');
  if (kind === 'change') {
    const forgot = await forgetReminders(window.Capacitor?.Plugins?.LocalNotifications);
    if (forgot.error) {
      onError(forgot.error);
      return;
    }
  }
  try {
    await site.choose(choice);
  } catch {
    onError("Le choix n'a pas pu être enregistré. Réessayez.");
  }
}

// The app's name, its standard site's host and the current choice, from
// the Site plugin; null, the page left disabled with a message, if it
// cannot give them.
async function readInfo() {
  try {
    const info = await site.info();
    return { ...info, host: new URL(info.standardOrigin).host };
  } catch {
    busy("L'application n'a pas pu lire ses réglages. Fermez-la, puis rouvrez-la.");
    return null;
  }
}

function start(info) {
  busy('');
  appName = info.appName;
  const label = standardLabel(info.applicationId, info.appName);
  standard.textContent = `Ouvrir ${label}`;
  document.getElementById('standard-host').textContent = `${label} : ${info.host}`;

  const kept = currentLabel(info.current, info.applicationId, info.appName);
  if (kept !== null) {
    current = info.current;
    document.getElementById('intro').textContent =
      "Un autre site vous déconnecte de celui-ci et retire ses rappels de ce téléphone. Vous pourrez y revenir en vous reconnectant.";
    document.getElementById('current-host').textContent = kept;
    keep.textContent = `Garder ${kept}`;
    document.getElementById('current').hidden = false;
    keep.addEventListener('click', () => reopen(fail));
  }

  standard.addEventListener('click', () => store(readChoice({ kind: 'standard' }).store, fail));

  address.addEventListener('input', () => {
    error.textContent = '';
    address.removeAttribute('aria-invalid');
  });

  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    error.textContent = '';
    status.textContent = '';
    const choice = readChoice({ kind: 'custom', address: address.value });
    if (choice.error) {
      refuse(choice.error);
      return;
    }
    const online = navigator.onLine;
    let probe = null;
    if (online) {
      busy(`Vérification de ${new URL(choice.probe).host}…`);
      try {
        probe = await site.probe({ url: choice.url });
      } catch {
        probe = { failure: 'unreachable' };
      }
    }
    const verdict = judgeProbe(choice.probe, probe, { online, appName });
    if (verdict.error) {
      refuse(verdict.error);
      return;
    }
    await store(verdict.store, refuse);
  });
}

if (!site) {
  // Opened anywhere but in the app's choice of a site: nothing to store into.
  busy("Cette page ne sert qu'au choix du site, dans l'application.");
} else {
  busy('Chargement…');
  const info = await readInfo();
  if (info) start(info);
}
