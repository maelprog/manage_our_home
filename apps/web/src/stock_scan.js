// The "Scanner" button of /stocks/new (#402), loaded after the markup it
// enhances (`assets::Script::StockScan`). Without JavaScript, or without a
// camera, the block stays `hidden` and the "Code-barres" field above it is
// the way in: it sends the code to the same `/stocks/new?scan=…` this script
// navigates to. The script only decodes the image; what the code means is
// decided by apps/api.
//
// Decoding uses the browser's `BarcodeDetector` when it has one that reads
// EAN-13, else barcode-detector's ponyfill and its ZXing WebAssembly reader,
// both served by apps/web under `/assets` (their URLs are on the block's
// `data-scan-polyfill` and `data-scan-wasm`), never from a CDN.
(function () {
  "use strict";

  var root = document.querySelector("[data-scan]");
  var media = navigator.mediaDevices;
  if (!root || !media || !media.getUserMedia) return;

  // `hidden` is toggled on the two wrappers, never on a button: the sheet
  // gives `button` a `display` of its own, which overrides the attribute.
  var idle = root.querySelector("[data-scan-idle]");
  var start = idle.querySelector("button");
  var view = root.querySelector("[data-scan-view]");
  var video = view.querySelector("video");
  var stop = root.querySelector("[data-scan-stop]");
  var status = root.querySelector("[data-scan-status]");
  // UPC-E is left out on purpose: apps/api does not accept it.
  var FORMATS = ["ean_13", "ean_8", "upc_a"];
  var stream = null;
  var timer = null;
  var detector = null;

  function say(text) {
    status.textContent = text;
  }

  function loadPolyfill() {
    return new Promise(function (resolve, reject) {
      var script = document.createElement("script");
      script.src = root.getAttribute("data-scan-polyfill");
      script.addEventListener("load", resolve);
      script.addEventListener("error", reject);
      document.head.appendChild(script);
    }).then(function () {
      var api = window.BarcodeDetectionAPI;
      var wasm = root.getAttribute("data-scan-wasm");
      api.setZXingModuleOverrides({
        locateFile: function () {
          return wasm;
        },
      });
      return new api.BarcodeDetector({ formats: FORMATS });
    });
  }

  function getDetector() {
    if (!detector) {
      var Native = window.BarcodeDetector;
      detector =
        Native && Native.getSupportedFormats
          ? Native.getSupportedFormats().then(function (supported) {
              return supported.indexOf("ean_13") >= 0
                ? new Native({
                    formats: FORMATS.filter(function (f) {
                      return supported.indexOf(f) >= 0;
                    }),
                  })
                : loadPolyfill();
            })
          : loadPolyfill();
      // A failed load may be retried on the next press.
      detector.catch(function () {
        detector = null;
      });
    }
    return detector;
  }

  // Stops the camera: the tracks, not only the preview, so the browser's
  // "camera in use" indicator goes off.
  function close() {
    clearTimeout(timer);
    timer = null;
    if (stream) {
      stream.getTracks().forEach(function (track) {
        track.stop();
      });
      stream = null;
    }
    video.srcObject = null;
    view.hidden = true;
    idle.hidden = false;
  }

  function scanLoop(reader) {
    if (!stream) return;
    var next = function () {
      timer = setTimeout(function () {
        scanLoop(reader);
      }, 250);
    };
    if (video.readyState < 2) return next();
    reader.detect(video).then(function (codes) {
      if (!stream) return;
      if (codes.length) {
        var raw = codes[0].rawValue;
        close();
        say("Code lu : " + raw);
        window.location.assign("/stocks/new?scan=" + encodeURIComponent(raw));
      } else {
        next();
      }
    }, next);
  }

  function open() {
    say("Ouverture de la caméra…");
    idle.hidden = true;
    view.hidden = false;
    media
      .getUserMedia({ video: { facingMode: { ideal: "environment" } }, audio: false })
      .then(function (camera) {
        stream = camera;
        video.srcObject = camera;
        var playing = video.play();
        if (playing && playing.catch) playing.catch(function () {});
        say("Visez le code-barres de l'article.");
        return getDetector().then(scanLoop);
      })
      .catch(function () {
        close();
        say("Caméra ou lecteur indisponible : saisissez le code-barres dans le champ ci-dessus.");
      });
  }

  start.addEventListener("click", open);
  stop.addEventListener("click", function () {
    close();
    say("");
  });
  // Leaving the page, or putting it in the background, releases the camera.
  window.addEventListener("pagehide", close);
  document.addEventListener("visibilitychange", function () {
    if (document.hidden && stream) {
      close();
      say("");
    }
  });

  root.hidden = false;
})();
