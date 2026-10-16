# Branche `deploy` — manifests Kubernetes de manage_our_home

Cette branche décrit **ce qui tourne** sur les clusters k3s ; le code de
l'application est sur `main`. Elle ne partage aucun historique avec `main`
(branche orpheline, #431).

**Branche d'intégration `k3s`** (arbitrage du 2026-10-13) : les PR de
l'epic k3s (#431 à #437) visent `k3s`, pas `deploy`. Rien n'arrive sur
`deploy` avant que toute l'architecture tourne sur le serveur de test ;
alors une seule PR `k3s` → `deploy`, mergée à la main par l'utilisateur,
fait la promotion. La CI de cette branche (`deploy.yml`) tourne pareil sur
les deux.

## Le modèle

- **GitOps** : un Argo CD par cluster lit cette branche et applique son
  overlay (#432) ; pendant l'intégration, celui du serveur de test lit
  `k3s`, `deploy` restant vide jusqu'à la promotion
  (`platform/README.md`). Personne ne fait de `kubectl apply` à la main,
  hors l'amorçage d'un cluster (`platform/bootstrap.sh`), la CI non plus :
  aucun jeton Argo CD ni accès cluster ne lui est donné.
- **Une branche, un dossier par environnement** (arbitrage du 2026-10-10).
  Une branche par environnement est un anti-modèle GitOps : la promotion se
  ferait par merge entre branches, et les branches dériveraient. Ici, ce qui
  diffère entre staging et prod tient dans deux `kustomization.yaml` qu'on
  lit côte à côte.
- **Tout passe par une PR mergée à la main par l'utilisateur**, staging
  compris : ni push direct, ni auto-merge, ni merge par un bot ou un agent
  (arbitrage du 2026-10-10 ; ruleset de la branche : « Promotion »).
- **Staging** suit `main` : Argo CD le synchronise automatiquement.
  **Prod** suit les tags `v*` : un merge ici la laisse `OutOfSync` jusqu'à
  ce que l'utilisateur clique « Sync » (#432).
- **Images par empreinte** (`@sha256:…`), jamais par étiquette seule : une
  étiquette se déplace, une empreinte non. Les images sont publiées, signées
  et attestées par `.github/workflows/images.yml` de `main` (#430) ; elles
  se déplacent par les PR de « Promotion ».

## Arborescence

```
apps/manage-our-home/
  base/                 commun à tous les environnements
    api.yaml            Deployment, Service, PodDisruptionBudget
    web.yaml            idem
    migrate-job.yaml    Job `migrate`, hook Argo CD PreSync (#425)
    ingress/            routes et Middlewares Traefik, certificat (#434)
      host/             Component : l'hôte de l'overlay dans les règles
    minio/              MinIO et le Job qui crée son bucket (#435)
    kustomization.yaml  ConfigMaps (réglages en clair)
    postgres/           Cluster CloudNativePG et sa sauvegarde nocturne (#433)
  overlays/
    staging/            serveur maison, 4 Go : 1 réplica, étalement souple
      postgres/         son Postgres, réduit au serveur de 4 Go
    prod/               VPS tiers : la base telle quelle, 2 réplicas
      postgres/         son Postgres, la base telle quelle
platform/               socle des clusters, Applications Argo CD (#432)
ci/                     scripts de la CI de cette branche
.github/workflows/deploy.yml
.github/rulesets/deploy.json   ruleset de la branche `deploy` (« Promotion »)
```

Rendre un overlay : `kustomize build apps/manage-our-home/overlays/staging`,
et son Postgres : `kustomize build apps/manage-our-home/overlays/staging/postgres`.

## Ce que porte la base

- **api et web** : 2 réplicas, mise à jour sans jamais descendre sous ce
  nombre (`maxSurge: 1`, `maxUnavailable: 0`), étalés par nœud
  (`topologySpreadConstraints`), un PodDisruptionBudget chacun en
  `maxUnavailable: 1` (à un réplica, un `minAvailable: 1` bloquerait tout
  drain).
- **Sondes** (#424) : démarrage et vivacité sur `/healthz`, disponibilité sur
  `/readyz` avec `timeoutSeconds: 3` — au-dessus des 2 s que la route attend
  elle-même Postgres (api) ou l'api (web), sinon une dépendance lente se lit
  comme un timeout de sonde au lieu du 503 de la route.
- **Arrêt** (#424) : le serveur ferme son listener dès SIGTERM ; un
  `preStop` de 10 s le laisse servir le temps que son retrait des endpoints
  se propage, puis il draine `SHUTDOWN_GRACE_SECONDS` (25 s).
  `terminationGracePeriodSeconds: 40` couvre les deux.
- **Sécurité des pods** : UID 10001 non root, système de fichiers racine en
  lecture seule, aucune capacité, pas d'élévation de privilèges, seccomp
  `RuntimeDefault`, aucun jeton de compte de service monté. Le namespace,
  posé par la plateforme (`platform/components/app-namespace`), est en Pod
  Security `restricted` : l'API server refuse un pod qui s'en écarte.
- **Mémoire** : requête et limite sur chaque conteneur, Job compris. Au repos,
  api 7 à 8 Mi et web 3 à 5 Mi (`kubectl top`, k3s v1.37.1, deux relevés dans la PR de
  #431 ; le job k3d le réimprime à chaque passage). Les limites couvrent le
  pire cas borné sur `main` : api 256 Mi pour la porte d'envoi (8 pièces
  jointes de 20 Mi lues en mémoire à la fois) ; web 448 Mi pour la même
  porte (environ 170 Mi) plus deux décodages de photo code-barres
  simultanés (`DECODE_PERMITS = 2`, jusqu'à 124 Mi chacun), soit environ
  420 Mi. Une limite n'est pas réservée : le planificateur ne compte que
  les requêtes ; elle ne sert qu'à tuer un pod qui la dépasse.
- **Migration** : le Job `migrate` lance la commande du même nom de l'image
  api, comme hook `PreSync` : Argo CD n'applique le reste de la synchro que
  s'il réussit. Lui seul reçoit l'URL du rôle de migration.
- **`TRUSTED_PROXY_CIDRS=10.42.0.0/16`** : le réseau des pods de k3s, où
  sont web et le contrôleur d'ingress. Sans ce réglage, l'api garderait le
  défaut Docker (`172.16.0.0/12`) et tous les clients partageraient un seul
  compteur du frein du login (#178). Un cluster installé avec un autre
  `--cluster-cidr` doit le changer.

## Ingress

Traefik, le contrôleur d'ingress que k3s installe lui-même (Traefik 3.7.13
avec k3s v1.37.1), devant web et api sur un seul hôte, comme
`infra/Caddyfile` de `main` : `/api/*` va à api sans son préfixe, le reste
à web (#434). Les routes et les Middlewares sont dans `base/ingress/`
(une `IngressRoute` Traefik : chaque route y a sa chaîne de Middlewares) ;
ce qu'aucune route n'atteint — délais des connexions, adresse du client —
se règle sur Traefik lui-même, par la plateforme
(`platform/components/traefik`, un `HelmChartConfig` que k3s fusionne
dans les valeurs de son chart).

**L'hôte** est celui de `FRONTEND_BASE_URL` dans l'overlay : le Component
`base/ingress/host` le recopie dans chaque règle et dans le certificat.
`PUBLIC_BASE_URL` et `API_PUBLIC_BASE_URL` le nomment sous `/api` ;
`ci/render-test.sh` échoue si l'un d'eux, une règle ou le certificat s'en
écarte. Le certificat vient de Let's Encrypt (`letsencrypt`, staging
compris : les navigateurs ne font pas confiance à `letsencrypt-staging`)
par cert-manager, en HTTP-01 : le nom doit résoudre vers le serveur, ports
80 et 443 ouverts. En attendant, Traefik répond avec son certificat
auto-signé par défaut.

**Les garde-fous du Caddyfile** et ce qui les reprend :

| Caddyfile (`main`) | Ici |
|---|---|
| En-têtes de sécurité (#141) : CSP, `Reporting-Endpoints`, HSTS, `nosniff`, `X-Frame-Options`, `Referrer-Policy` | Middleware `security-headers`, mêmes valeurs ; le job k3d les compare à celles du Caddyfile de `main`, sur `/`, `/login` et `/api/healthz` |
| `encode zstd gzip`, rien sous 512 octets | Middleware `compress`, `encodings: [zstd, gzip]`, `minResponseBodyBytes: 512`, sur une liste de types textuels (Traefik n'accepte pas `text/*`) |
| `read_body 16m` : délai de toute la requête | `readTimeout: 2m` sur les deux points d'entrée, le même délai de Go (`ReadTimeout`), abaissé pour borner aussi les en-têtes (ligne suivante ; arbitrage du 2026-10-15). Un envoi doit donc arriver en 2 min : 175 ko/s, environ 1,4 Mbit/s montant, pour le plus gros corps accepté (21 037 056 octets), 42 ko/s pour une photo de 5 Mo. Sur toutes les routes sauf les deux d'envoi, un corps silencieux 30 s reçoit d'abord le 408 des applications. Sur les deux routes d'envoi, Traefik lit le corps lui-même avant de le transmettre (`upload-limit`) : un corps plus lent que 2 min s'y termine par un 499 de Traefik, pas par leur 408 |
| `read_header 10s` : en-têtes au goutte-à-goutte coupés à 10 s | Traefik n'a pas de délai propre aux en-têtes. En HTTP/1.1 ils tombent sous le `readTimeout` : un client qui égrène ses en-têtes est coupé à 2 min, le job k3d le mesure. En HTTP/2, le `readTimeout` ne les atteint pas : c'est l'`idleTimeout` qui ferme la connexion, à 5 min |
| `idle 5m` | `idleTimeout: 5m` |
| Le WebSocket de la messagerie échappe à `read_body` | Idem : ni `readTimeout` ni `idleTimeout` ne l'atteignent une fois ouvert ; le job k3d le tient 5 min 20 s en silence, puis y reçoit un message |
| `Connection: close` sur toute réponse à une requête HTTP/1.1 avec corps, 408 compris (#244) | Middleware `connection-close`, sur les routes des requêtes avec corps : toute méthode sauf GET, HEAD et OPTIONS, et ces trois-là quand elles annoncent un `Content-Length` (Caddy fermait aussi après le 408 d'un GET). Un GET au corps envoyé en `chunked` n'est pas vu : Go retire `Transfer-Encoding` des en-têtes que lit une règle Traefik. Traefik ne sait pas choisir une route selon la version du protocole : il s'applique aussi en HTTP/2 (arbitrage du 2026-10-15), où Go en fait un GOAWAY — la connexion se ferme une fois ses requêtes en cours servies, le navigateur en rouvre une après chaque formulaire envoyé. Mesuré sur 3.7.13 (POST de 100 ko envoyé à 5 ko/s, 401 sans lecture) : 19 s sans lui en HTTP/1.1, 0,03 s avec ; le job k3d vérifie un 401 de l'api en moins de 5 s, corps encore en cours |
| `request_body max_size` sur les deux routes d'envoi : 413 au-delà de 21 037 056 octets | Middleware `upload-limit` (`buffering`, `maxRequestBodyBytes: 21037056`) sur ces deux routes, en POST (arbitrage du 2026-10-15). Traefik lit le corps en entier avant de le transmettre (1 Mio en mémoire, le reste en fichier) : la réponse anticipée d'une application à un envoi n'arrive qu'une fois le corps reçu. Le job k3d vérifie le 413 immédiat au-delà, le passage à la limite |
| `X-Forwarded-For` : la valeur du client est remplacée, pas complétée | Idem par défaut dans Traefik (aucune `forwardedHeaders.trustedIPs`). Et `externalTrafficPolicy: Local` sur son Service : sans lui, ServiceLB et kube-proxy masquaient l'adresse du client, que Traefik voyait comme 10.42.0.1 — dans `TRUSTED_PROXY_CIDRS`, un seul compteur du frein du login pour tous. Le job k3d verrouille une adresse par dix échecs et vérifie qu'un second client ne l'est pas |
| Redirection HTTP vers HTTPS (HTTPS automatique) | `IngressRoute` `manage-our-home-http` : 301 pour GET et HEAD, 308 sinon (Caddy : 308) |

S'y ajoute un refus : `/readyz` et `/api/readyz` répondent 403 par
l'ingress (#424). Ces routes sont pour le kubelet, qui atteint les pods
directement ; par Caddy, elles étaient un aller-retour à Postgres que
n'importe qui pouvait demander sans frein.

`externalTrafficPolicy: Local` a un prix : seul un nœud qui fait tourner
un pod Traefik répond. k3s n'en lance qu'un ; avec plusieurs nœuds, l'adresse
publique doit viser celui-là, ou Traefik tourner sur chacun.

## Secrets

Aucune valeur secrète n'est dans cette branche : la base nomme des Secrets,
leurs valeurs arrivent scellées (`SealedSecret`, #432). `ci/render-test.sh`
échoue si un overlay rend un `Secret`, ou passe en clair une des variables
listées ci-dessous.

| Secret | Clés | Lu par |
|---|---|---|
| `api-secrets` | `DATABASE_URL`, `ADMIN_DATABASE_URL`, `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `OAUTH_ENCRYPTION_KEY`, `MESSAGE_ENCRYPTION_KEY`, `CALENDAR_FEED_ENCRYPTION_KEY`, `LOGIN_THROTTLE_KEY`, `VAPID_PRIVATE_KEY` (facultative), `SMTP_USERNAME`, `SMTP_PASSWORD` | api |
| `migrate-secrets` | `MIGRATION_DATABASE_URL` | Job `migrate` |
| `minio` | `MINIO_ROOT_USER`, `MINIO_ROOT_PASSWORD` | MinIO, le Job `minio-bucket`, et api (ses `MINIO_ACCESS_KEY` et `MINIO_SECRET_KEY`) |
| `postgres-app-role`, `postgres-migration-role`, `postgres-admin-role` | `username`, `password` (type `kubernetes.io/basic-auth`) | Cluster `postgres` : mot de passe de chaque rôle |
| `postgres-backup` | `ACCESS_KEY_ID`, `ACCESS_SECRET_KEY`, `REGION` (`fr-par`) | Cluster `postgres` : clé du bucket de sauvegarde |

Les URL de `api-secrets` et `migrate-secrets` visent le service
`postgres-rw` du namespace (`postgres://app_role:<mot de passe>@postgres-rw:5432/manage_our_home`,
de même `admin_role` et `migration_role`), avec le mot de passe scellé
dans le Secret du rôle : les deux doivent être scellés ensemble.

Le sens de chaque variable est dans `apps/api/README.md` et
`infra/generate-env.sh` sur `main`.

Leurs valeurs, scellées pour chaque cluster, sont dans
`platform/clusters/<env>/secrets/` ; procédure de scellement et de
sauvegarde de la clé du contrôleur : `platform/README.md`.

**Premier déploiement d'un environnement** : le Job `migrate` est un hook
`PreSync`, il tourne avant que la synchro de l'appli applique quoi que ce
soit d'autre. Le namespace `manage-our-home` (avec ses labels Pod
Security) et le Secret `migrate-secrets` sont donc posés par la plateforme,
hors de l'Application de l'appli, dans des vagues de synchro antérieures
à elle (#432, `platform/README.md`, « Ordre de la première synchro ») :
la base de l'appli ne rend plus de namespace.

## Postgres

Le Cluster CloudNativePG `postgres` (`base/postgres/`, #433) est
synchronisé par une Application à lui, `postgres-<env>`
(`platform/clusters/<env>/postgres.yaml`), dans une vague antérieure à
celle de l'appli : le Job `migrate` est un hook `PreSync`, il tourne avant
que l'Application de l'appli applique quoi que ce soit, la base ne peut donc
pas en faire partie. Argo CD ne le supprime jamais (`Delete=false`,
`Prune=false`) : supprimer le Cluster supprime son volume.

- **Instances** : une au départ. Pour plusieurs nœuds, passer
  `instances: 3` dans l'overlay : un primaire et deux réplicas en
  streaming, un par nœud (anti-affinité `required` sur
  `kubernetes.io/hostname`), et l'opérateur bascule seul sur un réplica.
  Le service `postgres-rw` suit le primaire, l'appli n'a rien à changer.
- **Rôles** : ceux de `infra/postgres/init/01-roles.sh` de `main`,
  attributs et droits compris. Les instructions du script sont rejouées à
  la création (`postInitApplicationSQL`), les rôles créés sans mot de
  passe ; `managed.roles` leur donne ensuite le leur, depuis leur Secret,
  et rétablit leurs attributs si on les change à la main. La base appartient
  à `mhome` (le `POSTGRES_USER` de Compose), qui ne se connecte pas. Pas de
  superutilisateur joignable par le réseau (`enableSuperuserAccess: false`).
  Le job k3d compare les rôles, les privilèges par défaut, les droits sur
  `public`, les extensions et la locale avec ceux d'un Postgres initialisé
  par le script de `main`.
- **Image** : `ghcr.io/cloudnative-pg/postgresql:16.15-system-trixie`, par
  étiquette et empreinte : la version majeure de Compose, et la variante
  `system`, qui porte barman-cloud.
- **Mémoire et réglages** :

  | | staging | prod (la base) |
  |---|---|---|
  | requête / limite mémoire | 256 / 384 Mi | 512 Mi / 1 Gi |
  | `shared_buffers` | 64MB | 256MB |
  | `effective_cache_size` | 192MB | 768MB |
  | `work_mem` / `maintenance_work_mem` | 2MB / 32MB | 4MB / 64MB |
  | `max_connections` | 100 | 150 |
  | volume | 10 Gi | 20 Gi |

  `max_connections` suit le budget de connexions de `apps/api/README.md`
  sur `main` : 37 par pod api (les 20 des handlers, 5 du pool admin, 4 + 2
  + 2 des trois limites de débit, une par job de fond), autant de pods que
  de réplicas plus un pendant un déploiement (`maxSurge: 1`), plus 1 pour
  le Job `migrate` et 2 pour `reconcile_attachments` : 77 en staging,
  114 en prod, sous les 97 et 147 laissés par les 3 connexions réservées au
  superutilisateur. `ci/render-test.sh` refait ce compte sur le rendu, avec
  les tailles de pool de la ConfigMap de l'api.

### Sauvegardes

Continues, hors du serveur, vers Scaleway Object Storage `fr-par`
(sous-traitant déjà au registre ; destination et durée y sont déclarées par
#440) : chaque segment WAL dès qu'il est plein, et au plus tard toutes les
5 minutes (`archive_timeout` de l'opérateur), plus une sauvegarde de base
chaque nuit à 3 h UTC et une à la création (`base/postgres/scheduled-backup.yaml`).
On peut donc restaurer à n'importe quel instant des **30 derniers jours**
(`retentionPolicy: 30d`), à 5 minutes près au pire pour les dernières
écritures ; au-delà, l'opérateur supprime sauvegardes et WAL — ceux du
dossier de son `serverName` seulement, après chacune de ses sauvegardes de
base. Après une restauration, l'ancien dossier n'est plus purgé par
personne : la procédure le purge à la main (« Restaurer », étape 8), sans
quoi la durée déclarée n'est plus tenue.

Un bucket par environnement (`manage-our-home-backups-staging`,
`manage-our-home-backups-prod`), chacun avec une clé d'API qui n'atteint
que lui : la clé du serveur maison ne lit pas les sauvegardes de la prod.

**Chiffrement** : en transit par HTTPS ; au repos par le chiffrement par
défaut du bucket (SSE-ONE, clés gérées par Scaleway), à activer **à la
création du bucket, avant la première sauvegarde** — il ne s'applique
qu'aux objets envoyés après :

```sh
aws s3api put-bucket-encryption --endpoint-url https://s3.fr-par.scw.cloud \
  --bucket manage-our-home-backups-staging \
  --server-side-encryption-configuration \
  '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'
aws s3api get-bucket-encryption --endpoint-url https://s3.fr-par.scw.cloud \
  --bucket manage-our-home-backups-staging
```

Le réglage par requête (`encryption: AES256` dans le Cluster) n'est pas
utilisé : Scaleway refuse l'en-tête qu'il envoie, et `ci/render-test.sh`
échoue s'il apparaît. Ce chiffrement protège les objets chez Scaleway,
qui détient la clé ; barman-cloud ne chiffre pas côté client.

Les sauvegardes passent par la prise en charge native de barman-cloud
par l'opérateur (`spec.backup.barmanObjectStore`), dépréciée depuis
CloudNativePG 1.26 au profit du greffon Barman Cloud mais toujours prise en
charge par l'opérateur 1.30.1 installé (chart 0.29.1). **Elle disparaît
avec CloudNativePG 1.31.0** (avertissement que l'opérateur 1.30.1 renvoie à
chaque `kubectl apply` du Cluster) : passer l'opérateur en 1.31 demande
d'abord de migrer les sauvegardes vers le greffon, qui ajoute un
Deployment et un conteneur dans le pod de Postgres, sur le budget du
serveur de 4 Go. D'ici là, le chart reste en 0.29.x
(`platform/components/cloudnative-pg/application.yaml`).

### Restaurer

À un instant `T` (UTC), base **et** bucket au même point
(`docs/v2-deployment.md` de `main`, item 9) : l'api supprime chaque jour
les objets de pièces jointes de plus de 24 h qu'aucune ligne
`event_attachments` ne désigne (`attachment_reconcile`). Une base revenue
à `T` avec un bucket resté au présent laisse sans ligne toutes les pièces
envoyées après `T`, que la passe suivante efface pour de bon ; un bucket
revenu avant la base laisse des lignes sans objet. D'où l'ordre :

1. **Arrêter Argo CD sur l'appli et sur Postgres**, la racine d'abord (elle
   rétablirait la synchro des autres) : retirer `syncPolicy.automated` de
   `platform`, `postgres-<env>` et `manage-our-home-<env>` (en prod, aucune
   n'en a).
2. **Arrêter l'api** (`kubectl -n manage-our-home scale deploy/api
   --replicas=0`) : plus d'écriture, et plus de passe
   `attachment_reconcile`.
3. **Le bucket à `T`** : le bucket des pièces jointes (« MinIO ») n'a pas
   encore de sauvegarde, il ne peut pas revenir à `T`. Ne rien y
   supprimer : le copier à part, api arrêtée, depuis le volume de MinIO,

   ```sh
   kubectl -n manage-our-home exec minio-0 -- \
     tar -C /bitnami/minio/data -cf - manage-our-home > pieces-jointes-$(date -u +%Y%m%d%H%M).tar
   ```

   puis ne relancer l'api (étape 7) qu'avec un `ADMIN_DATABASE_URL` sur un
   rôle sans `BYPASSRLS`, ce qui fait refuser chaque passe de
   réconciliation (et coupe aussi les routes `/admin/*` et les purges
   horaires) jusqu'à ce que lignes et objets soient remis d'accord
   (item 9). La copie contient les pièces jointes en clair : la ranger
   comme le serait une sauvegarde, hors du dépôt.
4. **Supprimer le Cluster**, volume compris :
   `kubectl -n manage-our-home delete cluster postgres`.
5. **Le recréer depuis le bucket de sauvegarde à `T`** : une PR sur
   `overlays/<env>/postgres/kustomization.yaml` ajoute ce patch, puis on
   resynchronise `postgres-<env>` (Argo CD le recrée, le Cluster ayant
   disparu) :

   ```yaml
   - target: {kind: Cluster, name: postgres}
     patch: |-
       - op: add
         path: /spec/externalClusters
         value:
           - name: origin
             barmanObjectStore:  # celui de spec.backup, serverName compris
               endpointURL: https://s3.fr-par.scw.cloud
               destinationPath: s3://manage-our-home-backups-<env>/
               serverName: postgres
               s3Credentials: {…les mêmes…}
               wal: {compression: gzip}
               data: {compression: gzip}
       - op: replace
         path: /spec/bootstrap
         value:
           recovery:
             source: origin
             database: manage_our_home
             owner: mhome
             recoveryTarget:
               targetTime: "2026-10-14 08:30:00.000000+00:00"
       - op: replace
         path: /spec/backup/barmanObjectStore/serverName
         value: postgres-r1
   ```

   Le nouveau `serverName` est obligatoire : l'opérateur refuse d'archiver
   dans un dossier qui contient déjà les WAL d'un autre cluster. Le
   nouveau nom reste dans l'overlay ensuite : les sauvegardes d'après la
   restauration sont sous lui. Une restauration ultérieure à un instant
   postérieur à celle-ci prend donc ce dossier pour source
   (`serverName: postgres-r1` dans `externalClusters`) et écrit sous un
   nom neuf (`postgres-r2`) — une fois que ce dossier a sa première
   sauvegarde de base (étape 6) ; à un instant antérieur, l'ancien dossier
   `postgres`, tant qu'il n'est pas purgé (étape 8).
   Une fois le Cluster sain (étape 6), une seconde PR ne garde de ce
   patch que le remplacement du `serverName` (les deux premières
   opérations retirées, le `bootstrap.initdb` de la base revient) : le
   `bootstrap` ne sert
   qu'à la création, mais laissé dans l'overlay, il rejouerait la
   restauration à `T` si le Cluster était un jour recréé.
6. **Vérifier, puis sauvegarder** : `kubectl -n manage-our-home get
   cluster postgres` en `Cluster in healthy state`, condition
   `ContinuousArchiving` vraie, les données attendues présentes. Puis une
   sauvegarde de base à la demande, attendue jusqu'à `completed` : le
   `immediate` du ScheduledBackup n'a joué qu'à sa création, et sans elle
   le nouveau dossier n'a aucune sauvegarde de base avant 3 h UTC, donc
   rien d'où restaurer.

   ```sh
   kubectl -n manage-our-home apply -f - <<EOF
   apiVersion: postgresql.cnpg.io/v1
   kind: Backup
   metadata: {name: postgres-after-restore-$(date -u +%Y%m%d%H%M)}
   spec: {cluster: {name: postgres}}
   EOF
   kubectl -n manage-our-home get backups.postgresql.cnpg.io -w   # PHASE completed
   ```
7. **Relancer l'api** (`replicas` de l'overlay), puis rétablir la synchro
   automatique, la racine en dernier.
8. **Purger l'ancien dossier** : l'opérateur n'applique la rétention qu'au
   dossier de son `serverName` courant. Celui d'avant la restauration
   (`postgres`) se purge à la main, chaque jour (ou à chaque sauvegarde
   nocturne) tant qu'il existe, depuis le pod de l'instance, qui porte
   barman-cloud, avec la clé du bucket :

   ```sh
   kubectl -n manage-our-home exec postgres-1 -c postgres -- env \
     AWS_ACCESS_KEY_ID='…' AWS_SECRET_ACCESS_KEY='…' AWS_DEFAULT_REGION=fr-par \
     barman-cloud-backup-delete --endpoint-url https://s3.fr-par.scw.cloud \
     --retention-policy "RECOVERY WINDOW OF 30 DAYS" \
     s3://manage-our-home-backups-<env>/ postgres
   ```

   Trente jours après la restauration, tout ce dossier a dépassé la durée
   déclarée (il ne reçoit plus rien depuis l'étape 4) : le supprimer en
   entier (`aws s3 rm --recursive --endpoint-url https://s3.fr-par.scw.cloud
   s3://manage-our-home-backups-<env>/postgres/`, ou la console Scaleway).

Le job k3d joue les étapes 1, 2, 4, 5, 6 et la relance de l'api
(`ci/postgres-test.sh`) : une ligne écrite avant `T`, une après, le
Cluster supprimé avec son volume et recréé avec ces trois changements
depuis un MinIO jetable — appliqués par `kubectl` au rendu de l'overlay,
là où la procédure passe par une PR et la synchro d'Argo CD ; la première
ligne est là, la seconde non, les rôles sont toujours ceux de
`01-roles.sh`, RLS s'applique, le Cluster restauré archive de nouveau, sa
sauvegarde à la demande aboutit et figure sous le nouveau `serverName`, la
commande de purge de l'étape 8 tourne sur l'ancien dossier (sans rien y
supprimer : ses sauvegardes ont quelques minutes) et l'api répond 200 sur
`/readyz`. Ni la seconde PR de l'étape 5 ni la suppression du dossier à
trente jours ne sont jouées. De l'étape 3, `ci/minio-test.sh` joue la copie
du bucket et vérifie qu'elle contient la pièce jointe envoyée.

## MinIO

Le stockage objet des pièces jointes d'événements (`base/minio/`, #435),
dans le namespace de l'appli : api l'atteint à `MINIO_ENDPOINT`
(`http://minio:9000`), bucket `MINIO_BUCKET` (`manage-our-home`). Il est
synchronisé par l'Application de l'appli, dans cet ordre : le Job `migrate`
(PreSync, il n'a pas besoin de MinIO), MinIO (vague -2), le Job qui crée le
bucket (vague -1), puis api et web (vague 0).

- **Un nœud, un disque** : un StatefulSet à un réplica, son volume de
  20 Gi venu de la classe de stockage par défaut (`local-path` sur k3s).
  Le volume survit au StatefulSet (`persistentVolumeClaimRetentionPolicy`
  en `Retain`) : ni sa suppression ni un élagage d'Argo CD ne retirent les
  pièces jointes. Perdre le disque du nœud, c'est les perdre (« Restaurer »).
- **Image** : celle de `infra/docker-compose.yml` sur `main`, même étiquette
  et même empreinte (`docker.io/bitnamilegacy/minio`, le miroir Bitnami :
  l'organisation `minio` refuse les tirages anonymes, #274). Le binaire
  MinIO est lancé directement, sans l'entrypoint Bitnami, qui veut écrire
  dans l'image et, sur un volume existant, être root : ici l'UID 1001 de
  l'image, système de fichiers racine en lecture seule, aucune capacité,
  comme les pods de l'appli. Les données sont au même chemin que dans
  Compose (`/bitnami/minio/data`). La console web est coupée
  (`MINIO_BROWSER=off`) : rien n'y mène.
- **Bucket** : le Job `minio-bucket`, image `minio-client` de Compose
  (son service `minio-init`), est un hook `Sync` d'Argo CD : il tourne à
  chaque synchro. Si le bucket existe, il le dit (« already there, nothing
  to do ») et sort en 0 sans rien changer ; sinon il le crée. Il ne
  supprime ni ne vide rien. Le nom du bucket y est répété (la ConfigMap de
  l'api n'est appliquée qu'à la vague 0) ; `ci/render-test.sh` vérifie que
  les deux concordent.
- **Identifiants** : le Secret `minio` (« Secrets »), lu par MinIO, par le
  Job et par api, comme Compose donne à l'api les identifiants racine de
  MinIO.
- **Mémoire** : requête 128 Mi, limite 384 Mi (arbitrage du 2026-10-12 :
  256 à 384 Mi) ; Job : 16 / 64 Mi. Mesuré sous Docker avec cette image,
  cet utilisateur, la racine en lecture seule et la limite de 384 Mi
  **sans swap** (`--memory-swap 384m`, comme un pod), pour la PR de #435
  (un conteneur neuf par essai, mémoire relevée toutes les 20 ms environ,
  180 à 330 relevés par essai) : 66 à 70 Mi au repos. 8, c'est la porte
  d'envoi d'**un** pod api ; la prod en a deux, donc jusqu'à 16 envois à
  la fois. Pires valeurs observées, en objets de 20 Mi envoyés par `mc`,
  arrondies au Mi supérieur :

  | Envois | Essais | Mémoire anonyme | Hors cache inactif (`memory.current` − `inactive_file`) |
  |---|---|---|---|
  | 8 à la fois | 3 | 138 Mi | 228 Mi |
  | 16 à la fois | 3 | 193 Mi | 345 Mi |
  | 8 à la fois, 5 lots enchaînés | 1 | 181 Mi | 246 Mi |
  | 16 à la fois, 5 lots enchaînés | 3 | 228 Mi | 380 Mi |

  Aucun OOM ni swap en 10 essais. La mémoire anonyme, celle qui ne se
  récupère pas, a gardé au moins 156 Mi de marge sous la limite. Le
  reste est du cache de fichiers, partie active comprise, que le noyau
  récupère à la limite. Avec le swap que Docker accorde par défaut, la
  mémoire anonyme culminait plus bas, une partie partant en swap : ces
  chiffres-là ne valent pas pour un pod. Le job k3d réimprime le repos.
- **Images admises** : MinIO, son client, l'image Postgres du Cluster et
  celle de l'opérateur CloudNativePG (qu'il met dans chaque pod Postgres)
  sont sur la liste fermée du namespace (`platform/README.md`, « Kyverno ») :
  toute autre image y est refusée à l'admission. Changer l'une d'elles,
  c'est changer la liste dans la même PR ; `ci/platform-test.sh` échoue
  sinon.
- **Téléchargement** : l'api rend des URL signées sur `MINIO_ENDPOINT`,
  soit `http://minio:9000`, qui ne se résout que dans le cluster. web y
  redirige le navigateur, qui ne l'atteint pas : comme avec Compose, où
  MinIO n'est pas publié non plus. Ni l'appli ni l'ingress ne le règlent
  encore.
- **Pas de sauvegarde** du bucket pour l'instant (`docs/v2-deployment.md`
  de `main`, item 8) : voir « Restaurer », étape 3.

**Plusieurs nœuds** : le mode distribué de MinIO n'est pas un changement de
`replicas`. Il code les objets par effacement sur plusieurs disques, au
moins 2 (`docs/distributed/README.md` de MinIO au tag
`RELEASE.2025-07-23T15-54-02Z`, la version de l'image) — par exemple 4
réplicas, un volume chacun, un pod par nœud — et chaque serveur
reçoit la liste de tous, écrite avec les noms du Service sans tête
`minio-headless` :
`minio server http://minio-{0...3}.minio-headless.manage-our-home.svc.cluster.local/bitnami/minio/data`.
Un déploiement à un disque ne se convertit pas sur place : monter le
nouveau à côté, y recopier le bucket (`mc mirror`), basculer
`MINIO_ENDPOINT`, puis retirer l'ancien.

## Promotion

Les empreintes des overlays ne bougent que par une PR vers cette branche,
mergée à la main par l'utilisateur (arbitrage du 2026-10-10), staging
compris.

- **Déplacer un overlay** :
  `ci/set-images.sh <staging|prod> <source> <empreinte api> <empreinte web>`
  ne change que les deux lignes `digest:` de l'overlay, chacune suivie de
  la source de la paire en commentaire (`# main 10aabde`, `# v1.4.0`).
  Relancé avec la même paire, il ne change rien : une PR ouverte se met à
  jour sans commit vide.
- **Staging** : après la publication des images d'un commit de `main`
  (#430), une PR qui ne touche que `overlays/staging`, mise à jour tant
  qu'elle n'est pas mergée plutôt que doublée.
- **Prod** : sur un tag `v*`, une PR qui ne touche que `overlays/prod`,
  dont le corps liste les commits depuis la version en prod et les
  migrations nouvelles. Elle promeut la paire de `overlays/staging`, celle
  qui tourne en staging, et non les images que `images.yml` construit sur
  le tag (arbitrage du 2026-10-16 ; `images.yml` reste tel quel) : celles-là
  sont reconstruites, rien ne garantit que leurs empreintes soient celles
  de staging, et elles n'y ont pas tourné. L'étiquette `sha-<commit>` ne
  désigne pas non plus la paire de staging de façon sûre : la construction
  du tag la pousse à nouveau (`image-build.yml`).
- **Le bot** qui ouvre ces PR est le workflow `promote.yml` de `main`,
  livré par une PR vers `main` (#436) : un workflow déclenché par `main` et
  ses tags ne peut vivre que sur `main`. Son identité — une GitHub App qui
  ne peut qu'ouvrir des PR, hors de toute liste de contournement, jamais un
  jeton personnel — est créée par l'utilisateur. Tant qu'il n'est pas en
  place, ces PR s'ouvrent à la main avec `ci/set-images.sh`.

### Ce que la CI vérifie

Le job `promotion` de `deploy.yml`, sur chaque PR :

- `ci/verify-signatures.sh` : chaque image de l'appli nommée par un
  overlay porte la signature que Kyverno exigera à l'admission, vérifiée
  avec cosign ; l'émetteur et l'identité sont lus dans
  `platform/components/kyverno/signed-images.yaml`, pas recopiés. Une
  empreinte non signée est rouge avant le merge, plutôt que refusée par
  Kyverno à la synchro.
- `ci/promotion-check.sh` : la PR est rouge si elle mène prod à une paire
  qui n'est ni celle que staging fait tourner au bout de la branche visée,
  ni une paire que prod a fait tourner à un commit antérieur de cette
  branche (le retour arrière ci-dessous). La paire se compare entière, api
  et web ensemble : elles viennent d'un même commit et ont tourné ensemble
  en staging. Une paire que staging a fait tourner autrefois mais plus
  maintenant est refusée.

`ci/promotion-test.sh` (job `render`) teste ces scripts et le ruleset
ci-dessous sur des copies jetables de la branche, cosign remplacé par un
double.

### Ruleset de `deploy`

`.github/rulesets/deploy.json` : PR obligatoire, liste de contournement
vide, ni suppression ni force-push, et les jobs de `deploy.yml` (`render`,
`promotion`, `k3d`) exigés au vert. Il ne vise que `deploy` : `k3s` reste
mergée par la vérification des PR de l'epic (arbitrage du 2026-10-13).
L'utilisateur, administrateur du dépôt, l'applique une fois :

```
gh api -X POST repos/maelprog/manage_our_home/rulesets --input .github/rulesets/deploy.json
```

puis, pour le modifier, `gh api -X PUT repos/maelprog/manage_our_home/rulesets/<id> --input …`.
L'auto-merge est désactivé sur tout le dépôt (`allow_auto_merge`) ; à
contrôler avec le ruleset :

```
gh api repos/maelprog/manage_our_home --jq .allow_auto_merge         # false
gh api repos/maelprog/manage_our_home/rulesets                        # deploy, bypass_actors vide
git push origin HEAD:deploy                                           # refusé
```

### Revenir en arrière

Un retour arrière est la PR du `git revert` du commit de promotion :

```
git fetch origin
git switch -c revert/prod-<version> origin/deploy
git revert <commit de la promotion>
git push -u origin revert/prod-<version>
gh pr create --base deploy --fill
```

`ci/promotion-check.sh` l'accepte : prod revient à une paire qu'elle a
déjà fait tourner. Une fois mergée à la main, staging se resynchronise
seul ; prod attend le clic « Sync ». Limite : si la version annulée a
apporté une migration, l'ancienne paire ne repart pas — l'api refuse un
schéma migré par une version plus récente (`apps/api/src/migrations.rs` de
`main`). Il faut alors corriger en avant (un commit sur `main`, un nouveau
tag) ou restaurer Postgres à un instant antérieur (« Restaurer »).

## Réglages à fixer

Les adresses publiques et le relais SMTP des overlays sont des
emplacements en `.invalid` (RFC 2606 : ne résout nulle part). L'adresse
d'un environnement se fixe dans `FRONTEND_BASE_URL` de son overlay, avec
`PUBLIC_BASE_URL` et `API_PUBLIC_BASE_URL` sur le même hôte : l'ingress et
son certificat suivent (« Ingress »). Tant qu'elle reste en `.invalid`,
Let's Encrypt refuse le certificat et l'Application de l'appli ne passe
pas `Healthy`. Les buckets de
sauvegarde de Postgres (`manage-our-home-backups-<env>`) sont à créer chez
Scaleway, chiffrement par défaut activé (« Sauvegardes »), avant le premier
déploiement ; un autre nom se change dans `base/postgres/cluster.yaml` et
l'overlay de staging.

## CI de cette branche

`.github/workflows/deploy.yml`, sur chaque PR vers `deploy` ou `k3s` et
chaque push sur l'une d'elles :

1. `ci/validate.sh` : `kustomize build` de chaque overlay, de son
   Postgres et de la plateforme de chaque cluster, `kubeconform -strict`
   contre les schémas de Kubernetes 1.37.1 (et ceux du catalogue
   CRDs-catalog, à un commit épinglé, pour les Applications,
   ClusterIssuers, ClusterPolicy, Cluster, ScheduledBackup,
   IngressRoutes, Middlewares, Certificate et HelmChartConfig).
2. `ci/render-test.sh` : tests sur le rendu des overlays — empreintes,
   limites mémoire, jamais sous leur plancher (api 256 Mi et web 448 Mi,
   le pire cas borné ci-dessus ; migrate 128 Mi, la limite sous laquelle
   les migrations de `main` ont tourné, observée et non bornée), sécurité
   des conteneurs, sondes, délais d'arrêt, une seule source `envFrom` (la
   ConfigMap, sans préfixe), hook `PreSync` du Job et sa politique de
   suppression, PDB,
   réplicas et étalement par environnement, aucun secret en clair ;
   MinIO : un réplica, volume gardé et monté là où le serveur écrit,
   identifiants du Secret `minio` pour lui et pour api, `MINIO_ENDPOINT`
   sur son Service, limite de 256 à 384 Mi en staging, Job de bucket en
   hook `Sync` sur le bucket de l'api, sans suppression, entre MinIO et
   l'appli ;
   l'ingress : un seul hôte, celui de `FRONTEND_BASE_URL`, dans chaque
   règle, le certificat et les URL publiques, l'émetteur `letsencrypt`,
   la chaîne de Middlewares de chaque route, `/readyz` refusé, HTTP
   réduit à la redirection, les six en-têtes de sécurité posés ; et
   sur le rendu de Postgres : une instance, anti-affinité, image par
   empreinte, mémoire (256 à 384 Mi en staging) et `shared_buffers` sous
   le quart de la limite, paramètres présents, budget de connexions,
   rôles et leurs attributs, sauvegarde vers un bucket Scaleway propre à
   l'environnement avec rétention, sans en-tête de chiffrement refusé,
   sous le `serverName` `postgres` ou `postgres-r<n>`.
3. `ci/platform-test.sh` : tests sur le rendu de la plateforme — prod
   sans `syncPolicy.automated` (racine, appli, Postgres), branche
   suivie, ordre des vagues (Postgres entre les Secrets et l'appli), namespace
   `restricted`, politiques Kyverno (et aucune exclusion, précondition
   ni sélecteur qui restreigne leur règle ; chaque image de l'overlay, de
   son Postgres et de l'opérateur CloudNativePG sur la liste fermée, rien
   d'autre, sans joker ni étiquette sans empreinte hors des images de
   l'appli ; l'étiquette de l'opérateur est l'appVersion de son chart),
   charts épinglés, réglages de
   Traefik (`externalTrafficPolicy: Local`, délais de 2 et 5 min,
   limite mémoire, avant l'appli), et sur le rendu Helm
   de chaque chart : limite mémoire sur chaque conteneur, un réplica,
   Argo CD sans Dex, notifications ni ApplicationSet, Kyverno réduit au
   contrôleur d'admission.
4. `ci/promotion-test.sh` : tests de `ci/set-images.sh`,
   `ci/promotion-check.sh`, `ci/verify-signatures.sh` et du ruleset de
   `deploy` (« Promotion »).
5. Job `promotion` : `ci/verify-signatures.sh`, et sur une PR
   `ci/promotion-check.sh` contre la branche visée (« Promotion »).
6. `ci/staging-smoke.sh` dans un cluster k3d jetable dont le nœud a les
   4 Go du serveur de test : un Postgres de référence créé avec les rôles
   de `infra/postgres/init/01-roles.sh` de `main` et un MinIO jetable pour
   les sauvegardes, puis `platform/bootstrap.sh staging`, la racine pointée
   sur le commit testé ; les Secrets scellés avec le contrôleur du
   cluster ; Argo CD synchronise la plateforme, le Postgres de staging
   (sauvegardé vers ce MinIO, seul écart au rendu, `ci/postgres-backup-patch.yaml`)
   puis l'appli (certificat d'un émetteur auto-signé, seul écart à son
   rendu, `ci/certificate-patch.yaml` ; Job de migration en PreSync, api et web
   jusqu'à `Available` et `/readyz` à 200, sans redémarrage, en lecture
   seule, sur le réseau des pods que l'api croit). Kyverno doit refuser
   l'image de l'appli par étiquette et un manifeste non signé de son dépôt,
   et admettre l'image signée ; refuser une image hors de la liste fermée
   (celle de MinIO, par étiquette seule). Puis 10 minutes au repos sans
   redémarrage ni OOM, tous les pods prêts, et `kubectl top pods -A`.
7. `ci/ingress-test.sh` sur le même cluster, par l'adresse du nœud comme
   un client (« Ingress ») : Traefik tourne avec ses réglages et sert le
   certificat de l'appli ; les en-têtes de sécurité de `/`, `/login` et
   `/api/healthz` sont ceux de Caddy avec le Caddyfile de `main`, lancé
   devant les mêmes pods (`ci/caddy.yaml`) ; HTTP redirige ; `/readyz`
   répond 403 ; compression zstd puis gzip, feuille de style comprise et
   son `Cache-Control` gardé, police non recompressée ; une adresse
   verrouillée par le frein du login le reste quel que soit son
   `X-Forwarded-For`, un second client ne l'est pas ; le WebSocket de la
   messagerie reçoit un message après 5 min 20 s de silence ; un client
   qui égrène ses en-têtes est coupé au délai de lecture de 2 min ; une
   réponse à un POST arrive avant la fin de son corps (#244) ; les routes
   d'envoi répondent 413 au-delà de 21 037 056 octets.
8. `ci/minio-test.sh` sur le même cluster : le Job de bucket de la synchro
   a créé le bucket ; une pièce jointe envoyée par l'api (au travers de
   l'ingress) est sur le volume de MinIO et se relit à l'identique par
   l'URL signée que rend l'api ; une nouvelle synchro d'Argo CD rejoue le
   Job, qui trouve le bucket et ne fait rien, la pièce jointe toujours
   là ; Argo CD ne suit pas le volume de MinIO (il ne peut donc pas
   l'élaguer) ; MinIO redémarré, la pièce jointe est encore là ; la copie du bucket de
   « Restaurer », étape 3, la contient.
9. `ci/postgres-test.sh` sur le même cluster : rôles identiques à ceux de
   `01-roles.sh`, RLS appliquée à `app_role`, puis la restauration à un
   instant (« Restaurer » ci-dessus).

Les outils (kustomize, kubeconform, yq, kubectl, k3d, helm, kubeseal,
cosign) sont épinglés par version et empreinte SHA-256 dans
`ci/install-tools.sh`, l'image k3s par étiquette et empreinte dans le
workflow. En local, avec ces outils dans le `PATH` :
`ci/validate.sh && ci/render-test.sh && ci/platform-test.sh && ci/promotion-test.sh`.
