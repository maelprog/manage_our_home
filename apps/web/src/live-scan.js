// « Scanner en direct » on /stocks/new (#807, `assets::Script::LiveScan`),
// loaded right after the barcode block it drives (`stocks::new::scan_block`).
// The rear camera's preview is read on the device: the browser's own
// `BarcodeDetector` when it reads EAN-13, else the vendored zxing-wasm
// (#806, `assets::Vendored`), fetched only when the button is pressed. The
// first EAN-13, EAN-8 or UPC-A read stops the camera and sends the
// « Code-barres » form below with it — the same `POST /stocks/new/scan` as
// the digits typed by hand. No frame leaves the page, and none is kept.
//
// Without JavaScript, or without `getUserMedia`, the block stays `hidden`
// and the photo and the typed code are what they were. A refused camera, no
// camera or a reader that will not load says so, and points at them.
(function () {
  "use strict";

  var root = document.querySelector("[data-live-scan]");
  if (!root) return;
  var media = navigator.mediaDevices;
  if (!media || typeof media.getUserMedia !== "function") return;

  var start = root.querySelector("[data-live-start]");
  var startLabel = start.innerHTML;
  var hint = root.querySelector("[data-live-hint]");
  var viewer = root.querySelector("[data-live-viewer]");
  var video = viewer.querySelector("video");
  var wait = root.querySelector("[data-live-wait]");
  var status = root.querySelector("[data-live-status]");
  var statusIcons = {
    scan: status.querySelector("[data-icon=scan]"),
    ok: status.querySelector("[data-icon=ok]"),
  };
  var statusText = status.querySelector("[data-live-text]");
  var stop = root.querySelector("[data-live-stop]");
  var denied = root.querySelector("[data-live-denied]");
  var failed = root.querySelector("[data-live-failed]");
  var form = root.parentNode.querySelector('form[action="/stocks/new/scan"]');
  var field = form && form.querySelector('input[name="scan"]');

  // How long the camera may look at nothing before the message turns to
  // the other ways in (the mock-up's « rien lu », validated at 20 s).
  var NOTHING_READ_MS = 20000;
  // Between two reads of the preview: often enough to feel immediate,
  // seldom enough to leave a phone's processor to the camera.
  var READ_EVERY_MS = 200;

  var stream = null;
  var session = 0;
  var timer = null;
  var nothingRead = null;
  var reader = null;

  function show(node, visible) {
    if (visible) node.removeAttribute("hidden");
    else node.setAttribute("hidden", "");
  }

  function say(text, icon, quiet) {
    statusIcons.scan.hidden = icon !== "scan";
    statusIcons.ok.hidden = icon !== "ok";
    status.classList.toggle("ok", icon === "ok");
    statusText.className = quiet ? "sr-only" : "";
    statusText.textContent = text;
  }

  function busy(on) {
    if (on) {
      start.setAttribute("aria-busy", "true");
      start.disabled = true;
      start.textContent = "Préparation du lecteur…";
    } else {
      start.removeAttribute("aria-busy");
      start.disabled = false;
      start.innerHTML = startLabel;
    }
  }

  // The camera, its timers and its preview, gone. `session` makes any
  // answer still on its way (a permission, a frame being read) moot.
  function release() {
    session += 1;
    clearTimeout(timer);
    clearTimeout(nothingRead);
    if (stream) {
      stream.getTracks().forEach(function (track) {
        track.stop();
      });
    }
    stream = null;
    video.srcObject = null;
  }

  // Back to the button: « Arrêter », Échap, an error, or a page put away.
  function idle(focus) {
    release();
    busy(false);
    show(viewer, false);
    show(stop, false);
    show(start, true);
    show(hint, true);
    viewer.classList.remove("ok");
    root.removeAttribute("data-state");
    say("", null);
    if (focus) start.focus();
  }

  function fail(notice) {
    idle(false);
    show(start, false);
    show(hint, false);
    show(notice, true);
  }

  // The reader: the browser's, when it reads the codes asked for, else the
  // vendored one. Either way a function from the video to a code or null.
  function browserReader() {
    if (typeof window.BarcodeDetector !== "function") return Promise.resolve(null);
    var wanted = ["ean_13", "ean_8", "upc_a"];
    return Promise.resolve(window.BarcodeDetector.getSupportedFormats())
      .then(function (formats) {
        var formatsRead = wanted.filter(function (f) {
          return formats.indexOf(f) !== -1;
        });
        if (formatsRead.indexOf("ean_13") === -1) return null;
        var detector = new window.BarcodeDetector({ formats: formatsRead });
        return function (source) {
          return detector.detect(source).then(function (codes) {
            return codes.length ? codes[0].rawValue : null;
          });
        };
      })
      .catch(function () {
        return null;
      });
  }

  function loadScript(src) {
    return new Promise(function (resolve, reject) {
      var script = document.createElement("script");
      script.src = src;
      script.onload = resolve;
      script.onerror = reject;
      document.head.appendChild(script);
    });
  }

  function vendoredReader() {
    var wasm = root.getAttribute("data-reader-wasm");
    var ready = window.ZXingWASM ? Promise.resolve() : loadScript(root.getAttribute("data-reader"));
    return ready
      .then(function () {
        // The library fetches its `.wasm` from a CDN unless told where it
        // is; the CSP would refuse the CDN anyway.
        return window.ZXingWASM.prepareZXingModule({
          overrides: {
            locateFile: function (path, prefix) {
              return /\.wasm$/.test(path) ? wasm : prefix + path;
            },
          },
          fireImmediately: true,
        });
      })
      .then(function () {
        var canvas = document.createElement("canvas");
        var context = canvas.getContext("2d", { willReadFrequently: true });
        return function (source) {
          var width = source.videoWidth;
          var height = source.videoHeight;
          if (!width || !height) return Promise.resolve(null);
          canvas.width = width;
          canvas.height = height;
          context.drawImage(source, 0, 0, width, height);
          var frame = context.getImageData(0, 0, width, height);
          return window.ZXingWASM.readBarcodes(frame, {
            formats: ["EAN13", "EAN8", "UPCA"],
            tryHarder: true,
            maxNumberOfSymbols: 1,
          }).then(function (codes) {
            var code = codes.filter(function (c) {
              return c.isValid;
            })[0];
            return code ? code.text : null;
          });
        };
      });
  }

  function getReader() {
    if (reader) return Promise.resolve(reader);
    return browserReader().then(function (read) {
      if (read) return read;
      // Only this one takes a while: the library and its decoder, 1 MB.
      busy(true);
      say("Préparation du lecteur de code-barres.", null, true);
      return vendoredReader();
    }).then(function (read) {
      reader = read;
      return read;
    });
  }

  // What the readers give that the scan form takes: the digits of an
  // EAN-13, an EAN-8 or a UPC-A, which apps/api checks again.
  function retailCode(text) {
    var code = (text || "").trim();
    return /^(\d{8}|\d{12}|\d{13})$/.test(code) ? code : null;
  }

  function read(current) {
    if (current !== session) return;
    reader(video)
      .then(function (text) {
        if (current !== session) return;
        var code = retailCode(text);
        if (code) found(code);
        else timer = setTimeout(read, READ_EVERY_MS, current);
      })
      .catch(function () {
        if (current === session) timer = setTimeout(read, READ_EVERY_MS, current);
      });
  }

  function found(code) {
    release();
    viewer.classList.add("ok");
    show(stop, false);
    root.setAttribute("data-state", "found");
    say("Code lu : " + code + ". Recherche du produit…", "ok");
    field.value = code;
    if (typeof form.requestSubmit === "function") form.requestSubmit();
    else form.submit();
  }

  function begin() {
    show(denied, false);
    show(failed, false);
    show(start, true);
    var current = (session += 1);
    getReader()
      .then(function () {
        if (current !== session) return null;
        busy(false);
        show(start, false);
        show(hint, false);
        show(viewer, true);
        show(wait, true);
        show(stop, true);
        root.setAttribute("data-state", "waiting");
        say("Autorisez la caméra quand le navigateur le demande.", null);
        stop.focus();
        return media.getUserMedia({ audio: false, video: { facingMode: { ideal: "environment" } } });
      }, function () {
        if (current === session) fail(failed);
        return null;
      })
      .then(function (camera) {
        if (!camera) return null;
        if (current !== session) {
          camera.getTracks().forEach(function (track) {
            track.stop();
          });
          return null;
        }
        stream = camera;
        video.srcObject = camera;
        return video.play().then(function () {
          if (current !== session) return;
          show(wait, false);
          root.setAttribute("data-state", "scanning");
          say("Placez le code-barres dans le cadre.", "scan");
          nothingRead = setTimeout(function () {
            if (current === session) {
              say("Toujours rien. Rapprochez-vous, évitez les reflets, ou saisissez le code ci-dessous.", "scan");
            }
          }, NOTHING_READ_MS);
          read(current);
        });
      })
      .catch(function (error) {
        if (current !== session) return;
        var refused = error && (error.name === "NotAllowedError" || error.name === "SecurityError");
        fail(refused ? denied : failed);
      });
  }

  root.addEventListener("click", function (event) {
    if (event.target.closest("[data-live-start]")) begin();
    else if (event.target.closest("[data-live-stop]")) idle(true);
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape" && stream !== null) idle(true);
    else if (event.key === "Escape" && root.getAttribute("data-state") === "waiting") idle(true);
  });

  // A page left, or put in the back-forward cache, lets the camera go; one
  // brought back from that cache shows the button again.
  window.addEventListener("pagehide", release);
  window.addEventListener("pageshow", function (event) {
    if (event.persisted && root.hasAttribute("data-state")) idle(false);
  });

  if (!form || !field) return;
  show(root, true);
})();
