# manage_our_home

## Design system

`DESIGN.md` (racine) est la source de vérité pour toute décision visuelle dans
`apps/web` : couleurs, typographie, espacement, layout, composants, motion.
Le lire avant de toucher au CSS ou au markup d'une route. Ne pas s'en écarter
sans accord explicite.

Contraintes structurantes qu'il rappelle : le CSS est servi depuis le binaire
sous une URL portant l'empreinte de son contenu (`/assets/style-<hash>.css`,
`apps/web/src/assets.rs`) — il était inliné dans chaque réponse jusqu'à #89,
il reste borné par un budget compressé mais n'est plus refacturé à chaque page
vue ; aucune dépendance CSS externe ; les polices sont auto-hébergées (un CDN
exposerait l'IP des visiteurs à un tiers, à déclarer au registre RGPD) ; et
les replis `var(--x, #hex)` sont interdits — c'est ce motif qui a cassé le
thème sombre de l'agenda sans que personne le voie.

L'état des lieux qui a motivé ce système est dans `docs/design-audit.md`.

## Development process

- Write unit tests before implementing new logic (TDD): for any new pure-logic
  function (validation, permission checks, formatting, parsing, etc.), write
  the `#[cfg(test)] mod tests` cases first, watch them fail, then implement.
  See `apps/api/src/messagerie/messages.rs` (`validate_content`) or
  `apps/api/src/messagerie/mod.rs` (`can_modify`) for the expected shape.
- Integration/flow tests (`apps/api/tests/*_flow.rs`) still cover end-to-end
  behavior against a real DB; unit tests are for the pure logic pulled out of
  handlers, not a replacement for them.

## Dependency policy

- Prefer the most recent version of a crate whenever it compiles clean and
  `cargo audit` reports no advisories for it. Don't pin to an older version
  "for stability" if a newer one is audit-clean and passes the test suite.
- Never silence `cargo audit` with an ignore list to make CI pass. If an
  advisory fires, fix the actual dependency graph (upgrade the crate, upgrade
  whatever pulls it in transitively, or drop unused default features that
  pull in the vulnerable path) — don't add `audit.toml` ignores as a
  workaround. See the sqlx/oauth2/aws-sdk-s3 upgrade in the CI-audit PR: the
  fix was disabling `aws-sdk-s3`'s default features (which pulled in a
  legacy rustls 0.21/webpki path alongside the modern one) and bumping sqlx
  0.7 -> 0.9 (0.8 still listed `sqlx-mysql`, and transitively `rsa`, in
  Cargo.lock even with `default-features = false` and `mysql` unselected;
  0.9 dropped that edge entirely).
