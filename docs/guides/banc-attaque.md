# Banc d'attaque local

Une pile jetable — Postgres, MinIO, apps/api, apps/web — pour rejouer des
requêtes hostiles contre le vrai code, avec deux foyers fictifs prêts à
l'emploi (#915). Script : `infra/attack-bench/bench.sh`.

## La règle : cible locale uniquement

Le banc ne vise que la pile qu'il démarre, sur ta machine. Jamais le
staging, jamais le cluster k3s, jamais un hôte distant, même « pour voir ».

Le script le fait respecter : toute URL qui n'est pas
`http://127.0.0.1[:port]` ou `http://localhost[:port]` est refusée (exit 2,
message `cible refusée`) avant le premier appel à docker ou à curl. https est
refusé aussi sur localhost : la pile du banc ne parle que http, une URL https
désigne donc une autre pile. Les scripts d'attaque peuvent poser la même garde
sur leur propre cible :

```sh
infra/attack-bench/bench.sh check-target "$URL" || exit 2
```

Toutes les requêtes du script passent `--noproxy '*'`, pour qu'un
`http_proxy` de l'environnement ne les emmène pas ailleurs.

## Usage

```sh
infra/attack-bench/bench.sh up > bench.env   # pile + comptes (quelques minutes à froid)
source bench.env
curl -H "Cookie: $A_OWNER_COOKIE" "$BENCH_API_URL/groups/$A_GROUP_ID"   # 200
curl -H "Cookie: $A_OWNER_COOKIE" "$BENCH_API_URL/groups/$B_GROUP_ID"   # 404
infra/attack-bench/bench.sh down
```

`up` écrit sur stdout des affectations shell et sa progression sur stderr, où
il imprime aussi le tableau des comptes. Il échoue si une pile du même nom
existe déjà.

`down` supprime tout ce que `up` a créé — conteneurs (et leurs volumes
anonymes), volume de compilation, réseau — puis vérifie qu'il n'en reste
rien. Il ne touche pas aux caches partagés `mhome-*` (registre cargo,
sccache), qui ne contiennent aucune donnée de la pile.

### Réglages

| Variable | Défaut | Rôle |
| --- | --- | --- |
| `BENCH_PREFIX` | `mhome-bench` | préfixe des conteneurs, du réseau et du volume ; deux bancs de préfixes différents cohabitent |
| `BENCH_API_PORT` / `BENCH_WEB_PORT` | `18080` / `13000` | ports publiés, sur `127.0.0.1` seulement |
| `BENCH_API_URL` / `BENCH_WEB_URL` (ou `--api-url` / `--web-url`) | `http://127.0.0.1:<port>` | adresse de la pile ; son port est celui publié ; gardée comme ci-dessus |
| `BENCH_CPUS` | `4` | cœurs de la compilation |

`down` lit le même `BENCH_PREFIX` que `up`.

## Ce que `up` monte

- Un réseau Docker dédié, `<préfixe>-net`. Seuls apps/api et apps/web
  publient un port, lié à `127.0.0.1`.
- Postgres avec les rôles du déploiement (`infra/postgres/init/01-roles.sh`) :
  apps/api sert les requêtes en `app_role` (`NOSUPERUSER NOBYPASSRLS`), la
  RLS s'applique comme en production. Migrations par le binaire `migrate`.
- MinIO et son bucket. Mêmes images et pins que `infra/docker-compose.yml`.
- apps/api et apps/web compilés depuis l'arbre courant (cache sccache
  partagé, `scripts/dev/ensure-docker-cache.sh`), cookies non `Secure`
  (`SECURE_COOKIES=false`, cookie `session_id`), SMTP vers un hôte
  inexistant : aucun courriel ne part.

**Aucun secret réel.** Mots de passe des rôles Postgres, identifiants MinIO,
clés de chiffrement et mot de passe des comptes sont tirés de `/dev/urandom` à
chaque `up` et meurent avec la pile.

## Les comptes

Deux foyers, `Foyer A` et `Foyer B`, chacun avec six comptes
`<a|b>-<rôle>@bench.example.test`, un mot de passe commun (`BENCH_PASSWORD`).
Pour chaque compte, `bench.env` donne `<X>_EMAIL`, `<X>_ID` et `<X>_COOKIE`
(`session_id=…`, vide quand il n'y a pas de session), avec `X` parmi :

| `X` (pour A ; idem `B_…`) | Compte | Session |
| --- | --- | --- |
| `A_OWNER` | propriétaire, a créé le foyer | complète |
| `A_MEMBER` | membre simple, entré par invitation | complète |
| `A_FORMER` | ancien membre : entré par invitation, puis a quitté le foyer | complète, ouverte avant son départ |
| `A_UNVERIFIED` | inscrit, e-mail jamais vérifié, membre de rien | aucune : la connexion est refusée |
| `A_DEACTIVATED` | membre, puis désactivé par l'action de support (`POST /admin/users/{id}/deactivate`) | restreinte (#289), ouverte après la désactivation |
| `A_SUPERADMIN` | superadmin technique (`is_superadmin`, posé en SQL comme le fait la suite e2e), membre simple du foyer | complète |

`A_GROUP_ID` et `B_GROUP_ID` donnent les foyers.

Tout passe par l'API réelle — inscription, vérification (le jeton est
re-signé en base comme dans `e2e/lib/db.ts`, puis le vrai lien est suivi),
connexion, invitation, départ, désactivation — sauf le drapeau superadmin,
qu'aucune route ne donne.

## Contrôle intégré

Avant de rendre la main, `up` vérifie que A-propriétaire lit son foyer
(`GET /groups/{A}` → 200) et pas celui de B (`GET /groups/{B}` → 403 ou
404). Sinon il échoue avec `ISOLATION ROMPUE`, et la pile reste en place pour
l'examen (`down` pour la détruire).

## Tests

La garde de cible est testée par `e2e/scripts/lib/attack-bench.test.ts`
(`npm run test:scripts`, job `e2e` de la CI) : elle lance le vrai script avec
un faux `docker` et un faux `curl` en tête du `PATH`, et vérifie qu'une cible
refusée — dont une URL `https://…` — sort en 2 avec le message, sans qu'aucun
des deux n'ait été appelé. `shellcheck infra/attack-bench/bench.sh` doit être
propre.
