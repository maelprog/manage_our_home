# Vendored third-party files

Served by apps/web out of its own binary (`include_bytes!`/`include_str!`,
`src/assets.rs` → `Vendored`), under a URL carrying the digest of their
content, never from a CDN: a CDN would hand each visitor's address to a third
party (the rule DESIGN.md states for the fonts).

Each file is byte for byte what the npm registry ships. Do not edit them; to
upgrade, take both packages again together (barcode-detector pins the exact
zxing-wasm release whose `.wasm` it expects) and update this table.

| File | Package | Path in the package | SHA-256 | Licence |
|---|---|---|---|---|
| `barcode-detector/ponyfill.js` | `barcode-detector@3.2.2` | `dist/iife/ponyfill.js` | `e3aa2057178b8ea71dd97003270331bbcb46499197b68bc0c7dd18e40c0863ea` | MIT (`barcode-detector/LICENSE`) |
| `zxing-wasm/zxing_reader.wasm` | `zxing-wasm@3.1.3` | `dist/reader/zxing_reader.wasm` | `2ebda08a93eea3efcd8399cda6b276e6a0b1de4fec60b4d8988a047de4c6d1ba` | MIT (`zxing-wasm/LICENSE`); compiled from zxing-cpp at commit `a17fd9dc65d6aa0dd2f660fdfca7a6a6613d938f`, Apache-2.0 (`zxing-wasm/LICENSE-zxing-cpp`, taken from that commit) |

The `.wasm` digest is the `ZXING_WASM_SHA256` constant `ponyfill.js` carries
for the release it was built against: the pair matches.

What uses them: the "Scanner" button of `/stocks/new` (#402,
`src/stock_scan.js`). A browser with a native `BarcodeDetector` that reads
EAN-13 (Chrome on Android, for one) never loads either file; the others
(every browser on iOS among them) load the ponyfill, which fetches the
`.wasm` from the URL the script gives it. Instantiating WebAssembly is what
`'wasm-unsafe-eval'` in infra/Caddyfile's `script-src` allows.
