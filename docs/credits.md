# Crédits

Œuvres de tiers que `apps/web` sert à ses visiteurs, et les licences sous
lesquelles elles le sont.

## Polices

Fraunces et Source Sans 3, sous SIL Open Font License 1.1 : le texte de la
licence accompagne chaque fichier, `apps/web/assets/fonts/OFL-Fraunces.txt`
et `apps/web/assets/fonts/OFL-SourceSans3.txt`.

## Icônes

Les tracés des icônes d'`apps/web/src/icons.rs` sont ceux de
[Lucide](https://lucide.dev) (paquet `lucide-static` 1.63.0), recopiés dans
le code sans les attributs de trait, que porte la feuille de style. Lucide
est publié sous licence ISC ; les icônes qu'il reprend du projet Feather le
sont aussi sous licence MIT. Parmi celles du jeu : `calendar`, `check`,
`chevron-left`, `chevron-right`, `clock`, `link`, `lock`, `minus`, `plus`,
`search`, `trash` et `x`. Les deux textes suivent, tels que Lucide les publie.

```
ISC License

Copyright (c) 2026 Lucide Icons and Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

```
The MIT License (MIT) (for the icons listed above)

Copyright (c) 2013-present Cole Bemis

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Lecteur de codes-barres

Le scan en direct des stocks (#807) lit les codes-barres dans le navigateur avec
[zxing-wasm](https://github.com/Sec-ant/zxing-wasm) 3.1.6 (paquet npm
`zxing-wasm`, licence MIT, Copyright (c) 2023 Ze-Zheng Wu), qui compile en
WebAssembly le décodeur [ZXing C++](https://github.com/zxing-cpp/zxing-cpp)
(licence Apache 2.0, commit `2c3dcfe` d'après le paquet). `apps/web` en sert
deux fichiers depuis son binaire, sous une URL portant leur empreinte
(`apps/web/src/assets.rs`, `Vendored`) : le script `dist/iife/reader/index.js`
et le module `dist/reader/zxing_reader.wasm`, copiés sans modification. Le
texte des deux licences les accompagne,
`apps/web/vendor/zxing-wasm/LICENSE` et
`apps/web/vendor/zxing-wasm/LICENSE-zxing-cpp`. Rien n'est demandé à un
CDN : la bibliothèque irait chercher son module sur jsDelivr par défaut, la
page qui la charge la pointe vers la copie servie par `apps/web` (#807), et
quoi qu'il arrive la CSP
(`infra/Caddyfile`) refuse toute autre origine.
