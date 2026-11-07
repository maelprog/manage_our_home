// The address a member types to open their own instance (#736): see
// www/site-address.js.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { normalizeSiteAddress } from '../www/site-address.js';

test('an https origin is kept, in its canonical form', () => {
  for (const [typed, origin] of [
    ['https://maison.example.org', 'https://maison.example.org'],
    ['https://maison.example.org/', 'https://maison.example.org'],
    ['https://Maison.Example.ORG', 'https://maison.example.org'],
    ['HTTPS://maison.example.org', 'https://maison.example.org'],
    ['https://maison.example.org:8443', 'https://maison.example.org:8443'],
    ['https://maison.example.org:443', 'https://maison.example.org'],
    ['  https://maison.example.org  ', 'https://maison.example.org'],
    ['https://maison.example.org.', 'https://maison.example.org.'],
    ['https://café.example', 'https://xn--caf-dma.example'],
    ['https://staging.watallo.fr', 'https://staging.watallo.fr'],
  ]) {
    assert.equal(normalizeSiteAddress(typed), origin, typed);
  }
});

test('an address without a scheme is taken as https', () => {
  assert.equal(normalizeSiteAddress('maison.example.org'), 'https://maison.example.org');
  assert.equal(normalizeSiteAddress('maison.example.org/'), 'https://maison.example.org');
  assert.equal(normalizeSiteAddress('maison.example.org:8443'), 'https://maison.example.org:8443');
  assert.equal(normalizeSiteAddress('203.0.113.7'), 'https://203.0.113.7');
});

test('a public IP literal given in https is kept', () => {
  assert.equal(normalizeSiteAddress('https://203.0.113.7'), 'https://203.0.113.7');
  assert.equal(normalizeSiteAddress('https://[2001:db8::1]'), 'https://[2001:db8::1]');
});

test('a private or loopback IP literal is kept only when https is typed', () => {
  for (const ip of ['10.0.0.2', '172.16.0.1', '172.31.255.254', '192.168.1.10', '127.0.0.1', '169.254.1.1', '0.0.0.0', '100.64.0.1', '100.127.255.254', '[::1]', '[fd00::1]', '[fe80::1]', '[::ffff:c0a8:10a]']) {
    assert.equal(normalizeSiteAddress(`https://${ip}`), `https://${ip}`, ip);
    assert.throws(() => normalizeSiteAddress(ip), /privée|https/, ip);
  }
  // Around the private ranges, public addresses.
  for (const ip of ['172.15.0.1', '172.32.0.1', '192.169.0.1', '11.0.0.1', '100.63.255.255', '100.128.0.1']) {
    assert.equal(normalizeSiteAddress(ip), `https://${ip}`, ip);
  }
});

test('plain http is refused, whatever the host', () => {
  for (const typed of ['http://maison.example.org', 'HTTP://maison.example.org', 'http://192.168.1.10', 'http://127.0.0.1:8080']) {
    assert.throws(() => normalizeSiteAddress(typed), /https/, typed);
  }
});

test('another scheme is refused', () => {
  for (const typed of ['ftp://maison.example.org', 'file:///etc/hosts', 'javascript:alert(1)', 'data:text/html,x', 'wss://maison.example.org']) {
    assert.throws(() => normalizeSiteAddress(typed), /https/, typed);
  }
});

test('a path, a query or a fragment is refused', () => {
  for (const typed of [
    'https://maison.example.org/app',
    'https://maison.example.org/app/',
    'maison.example.org/app',
    'https://maison.example.org/?x=1',
    'https://maison.example.org?x=1',
    'https://maison.example.org/#top',
    'https://maison.example.org#',
    'https://maison.example.org?',
    'https://maison.example.org\\app',
  ]) {
    assert.throws(() => normalizeSiteAddress(typed), /seule|chemin/, typed);
  }
});

test('credentials in the address are refused', () => {
  for (const typed of ['https://moi@maison.example.org', 'https://moi:secret@maison.example.org', 'moi@maison.example.org', 'https://:secret@maison.example.org']) {
    assert.throws(() => normalizeSiteAddress(typed), /identifiant/, typed);
  }
});

test('an empty or unreadable address is refused', () => {
  for (const typed of ['', '   ', 'https://', 'https:///', 'pas une adresse', 'https://exa mple.org', undefined, null, 42]) {
    assert.throws(() => normalizeSiteAddress(typed), Error, String(typed));
  }
  assert.throws(() => normalizeSiteAddress(''), /vide/);
});

test('a host name made of anything but letters, digits and inner hyphens is refused', () => {
  // What the URL parser lets through and MainActivity (java.net.URI) would
  // not open: the address would be stored and then ignored.
  for (const typed of [
    'https://ex_ample.com',
    'https://a..b.example',
    'https://-a.example',
    'https://a-.example',
    'https://*.example',
    'https://a$b.example',
    'https://a!b.example',
    'https://example.1a',
    'https://example.123x',
    'ex_ample.com',
  ]) {
    assert.throws(() => normalizeSiteAddress(typed), /illisible/, typed);
  }
  // Inner hyphens, digits anywhere but at the start of the last label.
  for (const typed of ['https://a-b.example', 'https://1a.example', 'https://xn--caf-dma.example', 'https://a.b2', 'https://localhost']) {
    assert.equal(normalizeSiteAddress(typed), typed, typed);
  }
  assert.throws(() => normalizeSiteAddress('  '), /vide/);
});
