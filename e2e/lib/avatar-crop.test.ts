// Le recadrage de la photo de profil (#860) : apps/web/src/avatar-crop.js,
// chargé par la section « Photo de profil » de /account. Ses fonctions pures
// (le carré, ses bornes, les touches) tournent ici dans un contexte `vm` nu,
// sans `document` : rien ne démarre seul, comme pour local-reminders.js.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const SOURCE = readFileSync(new URL("../../apps/web/src/avatar-crop.js", import.meta.url), "utf8");

type Square = { x: number; y: number; side: number };

function load() {
  const context = vm.createContext({});
  vm.runInContext(SOURCE, context, { filename: "avatar-crop.js" });
  const api = vm.runInContext("MHomeAvatarCrop", context);
  assert.ok(api, "the script defines MHomeAvatarCrop");
  return api;
}

const C = load();

// Ce qui sort du contexte `vm` a les prototypes d'un autre royaume :
// `deepStrictEqual` les comparerait aussi. On recopie en objets d'ici.
function plain(value: unknown): unknown {
  return JSON.parse(JSON.stringify(value));
}

function sq(x: number, y: number, side: number): Square {
  return { x, y, side };
}

// -- le carré de départ ------------------------------------------------------

test("le carré de départ est centré, à 80 % du petit côté : ses poignées restent sur la photo", () => {
  assert.deepEqual(plain(C.initialSquare(1200, 800)), sq(280, 80, 640));
  assert.deepEqual(plain(C.initialSquare(600, 900)), sq(60, 210, 480));
  assert.deepEqual(plain(C.initialSquare(500, 500)), sq(50, 50, 400));
});

test("le carré de départ ne descend pas sous le côté minimal", () => {
  assert.deepEqual(plain(C.initialSquare(400, 200)), sq(120, 20, 160));
  assert.deepEqual(plain(C.initialSquare(300, 100)), sq(100, 0, 100));
});

// -- le côté minimal ---------------------------------------------------------

// `k` : les pixels de la photo par pixel de l'écran, à la taille où la
// scène la montre.
test("le côté minimal : 160 px de la photo, et 48 px à l'écran", () => {
  // 1200 px montrés sur 400 : 48 px d'écran font 144 px de photo, 160 tient.
  assert.equal(C.minSide(1200, 800, 3), 160);
  // 4000 px montrés sur 400 : 48 px d'écran font 480 px de photo.
  assert.equal(C.minSide(4000, 3000, 10), 480);
  // Une photo plus petite que 160 px ne se recadre pas : le carré est son
  // petit côté.
  assert.equal(C.minSide(300, 100, 1), 100);
});

test("un panorama de 12000 × 1000 : le carré ne devient pas plus petit que ses poignées", () => {
  // Montré sur 600 px, un pixel d'écran vaut 20 px de photo : le côté
  // minimal est de 960 px de photo, 48 px d'écran — et non le cinquième du
  // petit côté (200 px de photo, 10 px d'écran).
  assert.equal(C.minSide(12000, 1000, 20), 960);
  assert.deepEqual(plain(C.clamp(sq(0, 0, 10), 12000, 1000, 20)), sq(0, 0, 960));
  assert.deepEqual(plain(C.resize(sq(0, 0, 1000), "se", -5000, -5000, 12000, 1000, 20)), sq(0, 0, 960));
  assert.deepEqual(plain(C.grow(sq(0, 0, 1000), -5000, 12000, 1000, 20)), sq(20, 20, 960));
  assert.deepEqual(plain(C.initialSquare(12000, 1000, 20)), sq(5520, 20, 960));
  // Montré sur 300 px, 48 px d'écran dépassent la hauteur : le carré la prend.
  assert.equal(C.minSide(12000, 1000, 40), 1000);
});

// -- les bornes --------------------------------------------------------------

test("le carré reste dans la photo, sans bande vide", () => {
  assert.deepEqual(plain(C.clamp(sq(-50, -10, 300), 1200, 800)), sq(0, 0, 300));
  assert.deepEqual(plain(C.clamp(sq(1100, 700, 300), 1200, 800)), sq(900, 500, 300));
});

test("le côté du carré est borné par le côté minimal et le petit côté", () => {
  assert.deepEqual(plain(C.clamp(sq(0, 0, 10), 1200, 800)), sq(0, 0, 160));
  assert.deepEqual(plain(C.clamp(sq(0, 0, 5000), 1200, 800)), sq(0, 0, 800));
});

// -- déplacer ----------------------------------------------------------------

test("déplacer décale le carré, et s'arrête au bord", () => {
  assert.deepEqual(plain(C.move(sq(100, 100, 400), 30, -20, 1200, 800)), sq(130, 80, 400));
  assert.deepEqual(plain(C.move(sq(100, 100, 400), -500, 900, 1200, 800)), sq(0, 400, 400));
});

// -- redimensionner par un coin ----------------------------------------------

test("tirer le coin bas droit agrandit le carré, le coin opposé reste fixe", () => {
  // Le côté suit la moyenne des deux composantes du geste : 40 et 60, 50.
  assert.deepEqual(plain(C.resize(sq(100, 100, 300), "se", 40, 60, 1200, 800)), sq(100, 100, 350));
});

test("tirer le coin haut gauche vers l'intérieur réduit le carré, le coin bas droit reste fixe", () => {
  assert.deepEqual(plain(C.resize(sq(100, 100, 300), "nw", 50, 50, 1200, 800)), sq(150, 150, 250));
});

test("les coins haut droit et bas gauche gardent fixe leur coin opposé", () => {
  // ne : le coin bas gauche (100, 400) reste.
  assert.deepEqual(plain(C.resize(sq(100, 100, 300), "ne", 20, -20, 1200, 800)), sq(100, 80, 320));
  // sw : le coin haut droit (400, 100) reste.
  assert.deepEqual(plain(C.resize(sq(100, 100, 300), "sw", -20, 20, 1200, 800)), sq(80, 100, 320));
});

test("un coin tiré au-delà du bord s'arrête au bord, sans déplacer le coin fixe", () => {
  // Le coin haut gauche (900, 300) reste : il y a 300 px à droite, 500 en bas.
  assert.deepEqual(plain(C.resize(sq(900, 300, 200), "se", 900, 900, 1200, 800)), sq(900, 300, 300));
  // Le coin bas droit (300, 500) reste : 300 px à gauche, 500 en haut.
  assert.deepEqual(plain(C.resize(sq(100, 300, 200), "nw", -900, -900, 1200, 800)), sq(0, 200, 300));
});

test("un coin tiré trop loin vers l'intérieur s'arrête au côté minimal", () => {
  assert.deepEqual(plain(C.resize(sq(100, 100, 300), "se", -500, -500, 1200, 800)), sq(100, 100, 160));
  assert.deepEqual(plain(C.resize(sq(100, 100, 300), "nw", 500, 500, 1200, 800)), sq(240, 240, 160));
});

// -- agrandir, réduire, recentrer --------------------------------------------

test("agrandir et réduire gardent le centre du carré", () => {
  assert.deepEqual(plain(C.grow(sq(400, 200, 300), 100, 1200, 800)), sq(350, 150, 400));
  assert.deepEqual(plain(C.grow(sq(400, 200, 300), -100, 1200, 800)), sq(450, 250, 200));
});

test("agrandir contre un bord pousse le carré dans la photo, jusqu'au petit côté", () => {
  assert.deepEqual(plain(C.grow(sq(0, 0, 300), 100, 1200, 800)), sq(0, 0, 400));
  assert.deepEqual(plain(C.grow(sq(0, 0, 700), 500, 1200, 800)), sq(0, 0, 800));
  assert.deepEqual(plain(C.grow(sq(400, 200, 300), -1000, 1200, 800)), sq(470, 270, 160));
});

test("recentrer met le carré au milieu de la photo, à sa taille", () => {
  assert.deepEqual(plain(C.center(sq(0, 0, 400), 1200, 800)), sq(400, 200, 400));
});

// -- les touches -------------------------------------------------------------

test("le pas du clavier : 2 % du grand côté, au moins 1 px", () => {
  assert.equal(C.keyStep(1200, 800), 24);
  assert.equal(C.keyStep(800, 1200), 24);
  assert.equal(C.keyStep(20, 10), 1);
});

test("les flèches déplacent", () => {
  assert.deepEqual(plain(C.keyAction("ArrowLeft", false)), { move: [-1, 0] });
  assert.deepEqual(plain(C.keyAction("ArrowRight", false)), { move: [1, 0] });
  assert.deepEqual(plain(C.keyAction("ArrowUp", false)), { move: [0, -1] });
  assert.deepEqual(plain(C.keyAction("ArrowDown", false)), { move: [0, 1] });
});

test("Maj et une flèche agrandit (droite, bas) ou réduit (gauche, haut)", () => {
  assert.deepEqual(plain(C.keyAction("ArrowRight", true)), { grow: 1 });
  assert.deepEqual(plain(C.keyAction("ArrowDown", true)), { grow: 1 });
  assert.deepEqual(plain(C.keyAction("ArrowLeft", true)), { grow: -1 });
  assert.deepEqual(plain(C.keyAction("ArrowUp", true)), { grow: -1 });
});

test("+ et = agrandissent, - réduit, Début recentre", () => {
  assert.deepEqual(plain(C.keyAction("+", false)), { grow: 1 });
  // Maj est souvent nécessaire pour taper « + » : elle ne change rien.
  assert.deepEqual(plain(C.keyAction("+", true)), { grow: 1 });
  assert.deepEqual(plain(C.keyAction("=", false)), { grow: 1 });
  assert.deepEqual(plain(C.keyAction("-", false)), { grow: -1 });
  assert.deepEqual(plain(C.keyAction("Home", false)), { center: true });
});

test("les autres touches ne font rien : Tab, Entrée et Espace restent au navigateur", () => {
  for (const key of ["Tab", "Enter", " ", "a", "Escape", "End"]) {
    assert.equal(C.keyAction(key, false), null, key);
  }
});

// -- ce qui part au serveur --------------------------------------------------

test("le carré envoyé fait 256 px au plus, jamais plus que le carré choisi", () => {
  assert.equal(C.outputSide(800), 256);
  assert.equal(C.outputSide(256), 256);
  assert.equal(C.outputSide(200.4), 200);
  assert.equal(C.outputSide(0.2), 1);
});

test("le côté annoncé est un pourcentage du petit côté de la photo", () => {
  assert.equal(C.percent(800, 1200, 800), 100);
  assert.equal(C.percent(400, 1200, 800), 50);
  assert.equal(C.percent(160, 1200, 800), 20);
});
