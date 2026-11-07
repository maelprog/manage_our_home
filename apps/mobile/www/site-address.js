// The address a member types to open their own instance of MHome (#736),
// made into the origin the app will load, or refused with the reason.
//
// Only an https origin: the shell trusts the system authorities and allows
// no plain http (android/app/src/main/res/xml/network_security_config.xml),
// and nothing after the host, since the app always opens the site at its
// root. What it gives back is what MainActivity accepts from the stored
// preference: `https://host[:port]`, lowercase, no default port, no slash
// (URL.prototype.origin).
//
// - No scheme typed: https is assumed. Not for an IP literal of a private
//   network or of the phone itself (10/8, 172.16/12, 192.168/16, the
//   carrier-grade 100.64/10, 127/8, 169.254/16, 0/8; ::1, fc00::/7,
//   fe80::/10), which a server reached that way rarely has a certificate
//   for: there, `https://` has to be typed.
// - Refused: another scheme than https, a path, a query, a fragment (even
//   empty: `?`, `#`), credentials (`user@`), an empty or unreadable text,
//   a host name java.net.URI would not read (isHostName below).
//
// A plain script of the web directory, loaded by the shell's own pages and
// by `node --test` (scripts/site-address.test.mjs) alike: no dependency.

const EXPLICIT_SCHEME = /^([a-z][a-z0-9+.-]*):\/\//i;
// `javascript:…`, `mailto:…`: a scheme without `//`. Not `host:8443`.
const BARE_SCHEME = /^[a-z][a-z0-9+.-]*:(?!\d)/i;

export function normalizeSiteAddress(typed) {
  if (typeof typed !== 'string') {
    throw new Error("L'adresse est vide.");
  }
  const text = typed.trim();
  if (text === '') {
    throw new Error("L'adresse est vide.");
  }
  const explicit = EXPLICIT_SCHEME.exec(text);
  if (explicit ? explicit[1].toLowerCase() !== 'https' : BARE_SCHEME.test(text)) {
    throw new Error('Seul https est accepté : la connexion au site doit être chiffrée.');
  }
  const rest = explicit ? text.slice(explicit[0].length) : text;
  // The URL parser takes `\` for `/` in an https address.
  const end = rest.search(/[/\\?#]/);
  const authority = end === -1 ? rest : rest.slice(0, end);
  const after = end === -1 ? '' : rest.slice(end);
  if (authority.includes('@')) {
    throw new Error("L'adresse ne doit pas contenir d'identifiant ni de mot de passe.");
  }
  if (after !== '' && after !== '/') {
    throw new Error("Seule l'adresse du site est attendue, sans chemin ni paramètre après le nom.");
  }
  let url;
  try {
    url = new URL(`https://${authority}`);
  } catch {
    throw new Error(`Adresse illisible : ${text}`);
  }
  if (!isIpLiteral(url.hostname) && !isHostName(url.hostname)) {
    throw new Error(`Adresse illisible : ${text}`);
  }
  if (!explicit && isPrivateIp(url.hostname)) {
    throw new Error('Adresse IP privée ou locale : tapez https:// devant pour confirmer que le site a un certificat.');
  }
  return url.origin;
}

// `hostname` as the URL parser leaves it: IPv4 in dotted decimal, IPv6
// compressed and in brackets.
function isIpLiteral(hostname) {
  return hostname.startsWith('[') || /^\d+\.\d+\.\d+\.\d+$/.test(hostname);
}

// A host name java.net.URI reads as one (RFC 2396, which MainActivity
// parses the stored origin with), so that what is given back here is what
// the app opens: labels of letters, digits and inner hyphens, the last one
// starting with a letter, a final dot allowed. The URL parser lets through
// `_`, `*`, `$`, empty labels and the like.
function isHostName(hostname) {
  const labels = hostname.replace(/\.$/, '').split('.');
  return (
    labels.every((label) => /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(label)) &&
    /^[a-z]/.test(labels[labels.length - 1])
  );
}

function isPrivateIp(hostname) {
  if (hostname.startsWith('[')) {
    const ip = hostname.slice(1, -1);
    if (ip === '::1' || ip === '::') return true;
    const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(ip);
    if (mapped) {
      const [high, low] = mapped.slice(1).map((h) => parseInt(h, 16));
      return isPrivateIpv4([high >> 8, high & 0xff, low >> 8, low & 0xff]);
    }
    const first = parseInt(ip.split(':')[0] || '0', 16);
    return (first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80;
  }
  const v4 = /^(\d+)\.(\d+)\.(\d+)\.(\d+)$/.exec(hostname);
  return v4 !== null && isPrivateIpv4(v4.slice(1).map(Number));
}

function isPrivateIpv4([a, b]) {
  return (
    a === 10 ||
    a === 127 ||
    a === 0 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    (a === 169 && b === 254) ||
    (a === 100 && b >= 64 && b <= 127)
  );
}
