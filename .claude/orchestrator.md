# Briefing orchestrateur — manage_our_home

Lu en entier par chaque sous-agent. **≤ 6 Ko** ; le récit des incidents est dans
`.claude/orchestrator-history.md` (ne pas le lire par défaut).

## Environnement
Pas de toolchain Rust sur l'hôte : tout via Docker. Recettes (lire avant tout
gate) : mémoire locale du projet (`~/.claude/projects/<dossier du projet>/memory/`),
`rust-tests-via-docker.md` et `e2e-full-stack-via-docker.md`. `postgres:16` migré avec
`apps/api/migrations/*.sql` dans l'ordre ; MinIO
`quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z` + bucket `manage-our-home`
via `quay.io/minio/mc:RELEASE.2025-08-13T08-35-41Z` ; `rust:1-slim-bookworm`
(+ `rustup component add rustfmt clippy`), caches registry/target en volumes.
Tous tes conteneurs/réseaux/volumes portent ton préfixe ; `docker rm -f -v` à la fin.

## Gates (jobs de `.github/workflows/ci.yml`, en local avant push)
| Job | Commande |
|---|---|
| `fmt-clippy` | `cargo fmt --all -- --check` ; `cargo clippy --workspace --all-targets -- -D warnings` |
| `audit` | `cargo audit` |
| `test` | `cargo build --workspace --all-targets` ; `cargo test --workspace` (Postgres migré + MinIO) |
| `test-nobypassrls` | `cargo test -p manage_our_home --test '*'` sous un rôle `NOSUPERUSER NOBYPASSRLS` (#213) |
| `e2e` | Playwright contre api+web+Postgres+MinIO |

- CI en `SQLX_OFFLINE=true` ; en local, `DATABASE_URL` valide les `query!` contre une base **migrée**.
- Des tests MinIO se sautent **en silence** (`return`, pas `#[ignore]`) sans `MINIO_ENDPOINT/ACCESS_KEY/SECRET_KEY/BUCKET` : « 0 ignoré » ne prouve rien. Preuve : `-- --nocapture` + `grep 'skipping '`. Leur nombre comme le total : les recompter (`grep -rc real_minio_from_env`), jamais les citer de mémoire.
- `cargo audit` sans index crates.io saute les avis yanked sans le dire : vérifier que l'index s'est mis à jour.

## Sources de vérité
- `.claude/CLAUDE.md` toujours.
- `DESIGN.md` dès que `apps/web` est touché : feuille servie hachée (`apps/web/src/assets.rs`), aucune dépendance CSS externe, polices auto-hébergées, **`var(--x, #hex)` interdit**. Commentaires CSS bienvenus, les règles coûtent.
- Garde-fous de feuille dans `apps/web/src/app.rs` : `SHEET_CEILING` 13 312, `DECLARATIONS_CEILING` 3 136 (flate2 niveau 6 — **nommer l'encodeur** avec tout chiffre de poids). Jamais relevés ni affaiblis sans arbitrage.

## Tests
TDD obligatoire sur la logique pure : `#[cfg(test)] mod tests` d'abord, **vus rouges**, puis code. Modèles : `apps/api/src/messagerie/messages.rs` (`validate_content`), `messagerie/mod.rs` (`can_modify`). Flux bout en bout : `apps/api/tests/*_flow.rs`.

## Dépendances
Version la plus récente qui compile et passe `cargo audit`. Jamais d'`audit.toml` ni d'ignore : corriger le graphe.

## Conventions
- Branches `fix/<N>-<slug>` (bug), sinon `feat/ design/ docs/ test/ chore/ ci/ refactor/`, depuis `origin/main`. `refactor/` = aucun changement de comportement (tests existants inchangés et verts) ; le vérificateur le contrôle.
- Commit : conventionnel une ligne + `(#N)`, corps = pourquoi. PR : `gh pr create --base main --fill`, `Closes #N`.
- **Aucune attribution Claude** (ni `Co-Authored-By`, ni « Generated with Claude Code »).
- **Squash** : le dépôt est en `squash_merge_commit_message = COMMIT_MESSAGES` (recopie tous les commits). Toujours `gh pr merge <P> --squash --delete-branch --subject "<titre> (#<P>)" --body-file <corps de PR>`. Le corps de PR est donc le message définitif.

## `gh` sur ce dépôt
- `gh pr edit` et `gh issue view --comments` échouent (Projects-classic). Commentaires : `gh api repos/:owner/:repo/issues/<N>/comments --jq '.[] | "--- \(.user.login) ---\n\(.body)"'`. Corps de PR : `gh api -X PATCH repos/:owner/:repo/pulls/<N> --input <json>` (`-f body=@` ne lit pas le fichier).
- `gh pr checks --json` n'existe pas : `gh pr view <N> --json statusCheckRollup`.
- Timeout de pull `quay.io` à « Start MinIO » (exit 125) : `gh run rerun <id> --failed`, juger sur le rerun.

## Pièges connus
- **Aucune base déployée** (tags SemVer rétroactifs, aucun CD). Une migration reste modifiable tant qu'aucune base ne porte sa ligne ; modifier une migration existante demande quand même un arbitrage.
- `apps/api` refuse de démarrer sans `MIGRATION_DATABASE_URL` (rôle propriétaire des tables **et** `BYPASSRLS`) : toute recette e2e la pose (`ci.yml` job `e2e`).
- Docker Hub ne sert plus `minio/minio`/`minio/mc`, mais le cache local le masque : prendre les images `quay.io` épinglées ; `docker manifest inspect` pour savoir si une image est tirable.
- `cargo fmt --all` reformate aussi `apps/web` : `--check`, corriger à la main dans le périmètre.
- `cargo sqlx prepare -- --all-targets` depuis `apps/api` (sans `--all-targets`, les entrées des `tests/*_flow.rs` disparaissent).
- Playwright : **recompiler le binaire web avant**. Conteneur `mcr.microsoft.com/playwright:v1.61.1-noble` avec `--user $(id -u):$(id -g)` (sinon fichiers root dans le worktree), `-w /workspace` (`DEFAULT_ASSETS_DIR` repo-relatif → 404 woff2), `ICS_FIXTURE_HOST=<conteneur Playwright>` si réseau séparé. Node ≥ 22.18 (`engine-strict`).
- `e2e/lib/dates.ts` est chargé par Playwright **et** par Node en type-stripping : pas d'`enum`/`namespace`/propriété de constructeur.
- `npm run test:scripts` exige le dépôt complet (lit `apps/shared/...`).
- `/agenda` rend 42 jours autour du mois courant ; `/agenda?date=YYYY-MM-DD` pour un autre mois. Des données hors fenêtre ne sont pas rendues.
- Poids de page : `e2e/scripts/seed-perf-data.ts` + `measure-page-weight.ts`, jamais sur données peu variées.
- Aucune commande écrivante dans l'arbre principal depuis un sous-agent. Vérificateur : `git archive <sha> | tar -x`, pas de clone ; historique en lecture via `git -C <arbre principal> show`.
- Un worktree vide n'est pas forcément libre : vérifier qu'aucun agent/conteneur ne le monte.

## Escalade (arbitrage avant)
Écart à `DESIGN.md`, relèvement d'un plafond, modification de la CI ou d'une migration existante, fermeture d'issue sans PR, énoncé à deux lectures. `/code-review ultra` est facturé et déclenché par l'utilisateur seul : le réclamer explicitement pour migration/sécurité/permissions.
