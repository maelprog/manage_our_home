// Recadrer la photo de profil avant l'envoi (#860, `assets::Script::AvatarCrop`),
// chargé juste après la section « Photo de profil » de /account
// (`account::avatar::section`). La photo choisie est lue sur l'appareil et
// montrée entière ; un carré posé dessus se glisse, se redimensionne par ses
// coins ou au clavier. À l'envoi, le navigateur découpe ce carré (256 px au
// plus, JPEG) et le poste à `POST /account/avatar`, le même que le formulaire :
// apps/api reste seul juge du fichier, et recadre au centre un carré qui
// est déjà carré.
//
// Sans JavaScript, ou sans `createImageBitmap`, `toDataURL` ou `fetch`, le bloc
// reste `hidden` et le champ fichier natif fait ce qu'il faisait.
//
// `MHomeAvatarCrop` porte les calculs, sans DOM : un carré est
// `{ x, y, side }` en pixels de la photo, de largeur `w` et de hauteur `h` ;
// `k` est le nombre de pixels de la photo par pixel de l'écran, à la taille
// où la scène la montre.
// `e2e/lib/avatar-crop.test.ts` les lit dans un contexte sans `document`.
var MHomeAvatarCrop = (function () {
  "use strict";

  // Le carré envoyé : apps/api le réduit de toute façon à son `AVATAR_SIDE`.
  var OUTPUT_SIDE = 256;
  // Le côté minimal, en pixels de la photo (maquette validée le 2026-11-18)…
  var MIN_SIDE = 160;
  // … et à l'écran : la cible d'une poignée, `--s12`. Sans lui, une grande
  // photo montrée petite (un panorama de 12000 × 1000 sur 600 px) laissait
  // le carré devenir plus petit que ses poignées.
  var MIN_SCREEN = 48;

  function bound(value, low, high) {
    return Math.min(high, Math.max(low, value));
  }

  function minSide(w, h, k) {
    return Math.min(w, h, Math.max(MIN_SIDE, Math.ceil(MIN_SCREEN * (k || 0))));
  }

  function clamp(sq, w, h, k) {
    var side = bound(sq.side, minSide(w, h, k), Math.min(w, h));
    return {
      x: bound(sq.x, 0, w - side),
      y: bound(sq.y, 0, h - side),
      side: side,
    };
  }

  // Centré, à 80 % du petit côté : la photo reste visible tout autour, et
  // les poignées des coins ne sont pas coupées par le bord de la scène.
  function initialSquare(w, h, k) {
    var side = Math.max(minSide(w, h, k), Math.min(w, h) * 0.8);
    return { x: (w - side) / 2, y: (h - side) / 2, side: side };
  }

  function move(sq, dx, dy, w, h, k) {
    return clamp({ x: sq.x + dx, y: sq.y + dy, side: sq.side }, w, h, k);
  }

  // `corner` : "nw", "ne", "sw" ou "se". Le coin opposé reste où il est ; le
  // côté suit la moyenne des deux composantes du geste, vers l'extérieur.
  function resize(sq, corner, dx, dy, w, h, k) {
    var east = corner.charAt(1) === "e";
    var south = corner.charAt(0) === "s";
    var ax = east ? sq.x : sq.x + sq.side;
    var ay = south ? sq.y : sq.y + sq.side;
    var room = Math.min(east ? w - ax : ax, south ? h - ay : ay);
    var wanted = sq.side + ((east ? dx : -dx) + (south ? dy : -dy)) / 2;
    var side = bound(wanted, minSide(w, h, k), Math.min(room, w, h));
    return { x: east ? ax : ax - side, y: south ? ay : ay - side, side: side };
  }

  function grow(sq, delta, w, h, k) {
    var side = bound(sq.side + delta, minSide(w, h, k), Math.min(w, h));
    var cx = sq.x + sq.side / 2;
    var cy = sq.y + sq.side / 2;
    return clamp({ x: cx - side / 2, y: cy - side / 2, side: side }, w, h, k);
  }

  function center(sq, w, h, k) {
    return clamp({ x: (w - sq.side) / 2, y: (h - sq.side) / 2, side: sq.side }, w, h, k);
  }

  function keyStep(w, h) {
    return Math.max(1, Math.round(Math.max(w, h) * 0.02));
  }

  var ARROWS = {
    ArrowLeft: [-1, 0],
    ArrowRight: [1, 0],
    ArrowUp: [0, -1],
    ArrowDown: [0, 1],
  };

  // Ce qu'une touche fait au carré, ou `null` : la touche reste au navigateur.
  function keyAction(key, shift) {
    var arrow = ARROWS[key];
    if (arrow) {
      if (shift) return { grow: arrow[0] + arrow[1] };
      return { move: arrow.slice() };
    }
    if (key === "+" || key === "=") return { grow: 1 };
    if (key === "-") return { grow: -1 };
    if (key === "Home") return { center: true };
    return null;
  }

  function outputSide(side) {
    return Math.max(1, Math.min(OUTPUT_SIDE, Math.round(side)));
  }

  function percent(side, w, h) {
    return Math.round((side / Math.min(w, h)) * 100);
  }

  return {
    minSide: minSide,
    clamp: clamp,
    initialSquare: initialSquare,
    move: move,
    resize: resize,
    grow: grow,
    center: center,
    keyStep: keyStep,
    keyAction: keyAction,
    outputSide: outputSide,
    percent: percent,
  };
})();

(function () {
  "use strict";

  if (typeof document === "undefined") return;
  var root = document.querySelector("[data-crop]");
  if (!root) return;
  if (
    typeof window.createImageBitmap !== "function" ||
    typeof window.fetch !== "function" ||
    typeof window.FormData !== "function" ||
    typeof window.DOMParser !== "function" ||
    typeof window.atob !== "function" ||
    !HTMLCanvasElement.prototype.toDataURL
  ) {
    return;
  }

  var C = MHomeAvatarCrop;
  var form = root.closest("form");
  var section = form.closest("section");
  var input = form.querySelector('input[type="file"]');
  var stage = root.querySelector("[data-crop-stage]");
  var view = stage.querySelector("canvas");
  var box = root.querySelector("[data-crop-box]");
  var preview = root.querySelector("[data-crop-preview]");
  var live = root.querySelector("[data-crop-live]");
  var save = root.querySelector("[data-crop-save]");
  var other = root.querySelector("[data-crop-other]");
  var saveLabel = save.textContent;
  var otherLabel = other.textContent;
  // Le plus grand côté dessiné sur la page : la photo envoyée est découpée
  // dans l'image d'origine, celle-ci n'est qu'une vue.
  var VIEW_MAX = 1600;

  var state = "empty";
  var session = 0;
  var image = null;
  var W = 0;
  var H = 0;
  var square = null;
  var drag = null;
  var notice = null;

  // Le champ natif et son bouton cèdent la place au bloc ; le champ reste
  // dans la page, c'est lui qu'ouvre « Choisir une photo ».
  input.closest("label").hidden = true;
  form.querySelector('button[type="submit"]').hidden = true;
  root.hidden = false;

  function setState(next) {
    state = next;
    root.setAttribute("data-state", next);
    var shown = section.querySelectorAll("[data-in]");
    for (var i = 0; i < shown.length; i++) {
      shown[i].hidden = shown[i].getAttribute("data-in").split(" ").indexOf(next) < 0;
    }
    var sending = next === "sending";
    save.disabled = next !== "framing";
    other.disabled = sending;
    other.textContent = next === "reading" ? "Annuler" : otherLabel;
    if (sending) {
      save.setAttribute("aria-busy", "true");
      save.textContent = "Envoi en cours…";
    } else {
      save.removeAttribute("aria-busy");
      save.textContent = saveLabel;
    }
    box.tabIndex = next === "framing" ? 0 : -1;
  }

  function say(text) {
    live.textContent = text;
  }

  function sizeText() {
    return "côté " + C.percent(square.side, W, H) + " % de la photo";
  }

  function draw() {
    box.style.left = (square.x / W) * 100 + "%";
    box.style.top = (square.y / H) * 100 + "%";
    box.style.width = (square.side / W) * 100 + "%";
    box.style.height = (square.side / H) * 100 + "%";
    var ctx = preview.getContext("2d");
    ctx.clearRect(0, 0, preview.width, preview.height);
    ctx.drawImage(image, square.x, square.y, square.side, square.side, 0, 0, preview.width, preview.height);
  }

  // Les pixels de la photo par pixel de l'écran, à la taille de la scène.
  function photoPixels() {
    return stage.clientWidth ? W / stage.clientWidth : 0;
  }

  function choose() {
    input.value = "";
    input.click();
  }

  function show(bitmap) {
    if (image) image.close();
    image = bitmap;
    W = bitmap.width;
    H = bitmap.height;
    var scale = Math.min(1, VIEW_MAX / Math.max(W, H));
    view.width = Math.max(1, Math.round(W * scale));
    view.height = Math.max(1, Math.round(H * scale));
    view.getContext("2d").drawImage(bitmap, 0, 0, view.width, view.height);
    // La scène a les proportions de la photo, et 70 % de la fenêtre de
    // haut au plus : une photo en hauteur ne pousse pas les boutons hors
    // de l'écran d'un téléphone.
    stage.style.aspectRatio = W + " / " + H;
    stage.style.maxWidth = "calc(70vh * " + (W / H).toFixed(4) + ")";
    setState("framing");
    square = C.initialSquare(W, H, photoPixels());
    draw();
    box.focus({ preventScroll: true });
    say("Photo chargée. Le carré de découpage est sur la photo, " + sizeText() + ".");
  }

  function read(file) {
    var mine = ++session;
    setState("reading");
    window
      .createImageBitmap(file, { imageOrientation: "from-image" })
      .then(function (bitmap) {
        if (mine !== session) bitmap.close();
        else show(bitmap);
      })
      .catch(function () {
        if (mine === session) setState("unreadable");
      });
  }

  // Le refus du serveur, dans le bandeau où /account le met de lui-même
  // (sous l'en-tête du profil) ; le cadrage reste, pour réessayer.
  function fail(text) {
    setState("framing");
    var main = section.parentNode;
    // Seuls les bandeaux d'une réponse, des `p.notice`, s'en vont : le
    // panneau d'une suppression programmée et l'avis des CGU, des
    // `section.notice`, restent.
    var old = main.querySelectorAll(":scope > p.notice");
    for (var i = 0; i < old.length; i++) old[i].remove();
    notice = document.createElement("p");
    notice.className = "notice error";
    notice.setAttribute("role", "alert");
    notice.textContent = text || root.getAttribute("data-failed");
    var head = main.querySelector(":scope > .profile-head");
    if (head) head.after(notice);
    else section.before(notice);
    notice.scrollIntoView({ block: "nearest" });
  }

  function answered(resp) {
    var url = new URL(resp.url);
    if (resp.ok && !url.searchParams.has("error")) {
      window.location.assign(resp.url);
      return;
    }
    if (!url.searchParams.has("error")) {
      fail();
      return;
    }
    resp.text().then(function (html) {
      var page = new DOMParser().parseFromString(html, "text/html");
      var refusal = page.querySelector("p.notice.error");
      fail(refusal && refusal.textContent);
    }, function () {
      fail();
    });
  }

  function send() {
    if (state !== "framing") return;
    setState("sending");
    if (notice) notice.remove();
    var side = C.outputSide(square.side);
    var out = document.createElement("canvas");
    out.width = side;
    out.height = side;
    out.getContext("2d").drawImage(image, square.x, square.y, square.side, square.side, 0, 0, side, side);
    var blob = jpeg(out);
    if (!blob) {
      fail();
      return;
    }
    var body = new FormData();
    body.append("file", blob, "photo.jpg");
    window
      .fetch(form.action, { method: "POST", body: body, credentials: "same-origin" })
      .then(answered, function () {
        fail();
      });
  }

  // Le carré en JPEG, encodé sur-le-champ (#963). Dans Chromium, `toBlob`
  // n'encode que pendant les temps morts de la page. Il ne s'y force
  // qu'après 1 s si aucun n'est venu, et, si l'encodage a commencé sans
  // finir, 5,7 s après ce premier délai (≈ 6,7 s après l'appel ; 4 s puis
  // 9 s sur Android) : sur un appareil chargé, l'envoi attendait jusque-là
  // sans rien faire. Un carré de 256 px au plus s'encode en quelques
  // millisecondes.
  function jpeg(canvas) {
    var url = canvas.toDataURL("image/jpeg", 0.9);
    var comma = url.indexOf(",");
    if (comma < 0 || comma === url.length - 1) return null;
    var bytes = window.atob(url.slice(comma + 1));
    var buffer = new Uint8Array(bytes.length);
    for (var i = 0; i < bytes.length; i++) buffer[i] = bytes.charCodeAt(i);
    return new Blob([buffer], { type: url.slice(5, url.indexOf(";")) });
  }

  input.addEventListener("change", function () {
    var file = input.files && input.files[0];
    if (file) read(file);
  });
  root.querySelector("[data-crop-choose]").addEventListener("click", choose);
  other.addEventListener("click", function () {
    if (state === "reading") {
      session++;
      input.value = "";
      setState("empty");
    } else {
      choose();
    }
  });
  save.addEventListener("click", send);
  form.addEventListener("submit", function (e) {
    e.preventDefault();
  });

  box.addEventListener("pointerdown", function (e) {
    if (state !== "framing" || e.button !== 0) return;
    var handle = e.target.closest("[data-corner]");
    box.setPointerCapture(e.pointerId);
    box.focus({ preventScroll: true });
    drag = {
      corner: handle ? handle.getAttribute("data-corner") : null,
      x: e.clientX,
      y: e.clientY,
      start: square,
      k: photoPixels(),
    };
    e.preventDefault();
  });
  box.addEventListener("pointermove", function (e) {
    if (!drag) return;
    var dx = (e.clientX - drag.x) * drag.k;
    var dy = (e.clientY - drag.y) * drag.k;
    square = drag.corner
      ? C.resize(drag.start, drag.corner, dx, dy, W, H, drag.k)
      : C.move(drag.start, dx, dy, W, H, drag.k);
    draw();
  });
  function release() {
    if (!drag) return;
    drag = null;
    say("Carré à la position choisie, " + sizeText() + ".");
  }
  box.addEventListener("pointerup", release);
  box.addEventListener("pointercancel", release);

  var DIRECTIONS = { "-1,0": "vers la gauche", "1,0": "vers la droite", "0,-1": "vers le haut", "0,1": "vers le bas" };
  box.addEventListener("keydown", function (e) {
    if (state !== "framing" || e.altKey || e.ctrlKey || e.metaKey) return;
    var action = C.keyAction(e.key, e.shiftKey);
    if (!action) return;
    e.preventDefault();
    var step = C.keyStep(W, H);
    var k = photoPixels();
    if (action.move) {
      square = C.move(square, action.move[0] * step, action.move[1] * step, W, H, k);
      say("Carré déplacé " + DIRECTIONS[action.move.join(",")] + ".");
    } else if (action.grow) {
      square = C.grow(square, action.grow * step, W, H, k);
      say("Carré " + (action.grow > 0 ? "agrandi" : "réduit") + ", " + sizeText() + ".");
    } else {
      square = C.center(square, W, H, k);
      say("Carré recentré.");
    }
    draw();
  });

  // Une page ressortie du cache du navigateur par le retour arrière rend
  // la main au cadrage.
  window.addEventListener("pageshow", function (e) {
    if (e.persisted && state === "sending") setState("framing");
  });

  setState("empty");
})();
