zxing-wasm 3.1.6, tel que publié sur npm (`zxing-wasm-3.1.6.tgz`,
intégrité `sha512-mU7UNuvl+hOT+nlSOQTMHZ2tM8lRMsbjDIp5IO/+HmBl/FOq77q0OSZPJ3cm42A9c1MKU6Uve67FiGdkhsrT9g==`).

- `zxing-reader.js` : `package/dist/iife/reader/index.js`
- `zxing_reader.wasm` : `package/dist/reader/zxing_reader.wasm`
- `LICENSE` : `package/LICENSE` (MIT)
- `LICENSE-zxing-cpp` : `LICENSE` de zxing-cpp au commit
  `2c3dcfeffa7878950a58c1703c86774a428b0c91`, celui que le script déclare
  (Apache 2.0)
- `LICENSE-emscripten` : `LICENSE` d'Emscripten (MIT et University of
  Illinois/NCSA), qui a compilé le module et écrit le code d'amorçage du
  script ; `LICENSE-musl` : `system/lib/libc/musl/COPYRIGHT` d'Emscripten
  (MIT), la libc compilée dans le module. Les deux pris sur
  emscripten-core/emscripten au commit
  `e8b0dd9c350a0396a9b4797f585eb8a2510e3c04`  (branche principale au
  2026-11-13) : le paquet ne dit pas quelle version d'Emscripten l'a
  produit.

Copiés sans modification ; servis par `apps/web/src/assets.rs` (`Vendored`),
pas par `ServeDir`. Voir `docs/credits.md`.
