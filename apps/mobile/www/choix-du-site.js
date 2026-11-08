// The first-launch page (choix-du-site.html, #737): reads the form, lets
// site-choice.js decide, and hands the probe and the storing to
// MainActivity's Site plugin. Storing a choice restarts the activity on
// it: this page is then gone.
import { judgeProbe, readChoice, standardLabel } from './site-choice.js';

const site = window.Capacitor?.Plugins?.Site;
const standard = document.getElementById('standard');
const form = document.getElementById('custom');
const address = document.getElementById('address');
const error = document.getElementById('address-error');
const status = document.getElementById('status');
let appName = 'MHome';

function busy(text) {
  status.textContent = text;
  for (const button of [standard, form.querySelector('button')]) button.disabled = text !== '';
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

// `onError` says the failure: refuse for a typed address, fail for the
// standard site.
async function store(choice, onError) {
  busy('Ouverture du site…');
  try {
    await site.choose(choice);
  } catch {
    onError("Le choix n'a pas pu être enregistré. Réessayez.");
  }
}

// The app's name and its standard site's host, from the Site plugin; null,
// the page left disabled with a message, if it cannot give them.
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
  // Opened anywhere but in the app's first launch: nothing to store into.
  busy("Cette page ne sert qu'au premier lancement de l'application.");
} else {
  busy('Chargement…');
  const info = await readInfo();
  if (info) start(info);
}
