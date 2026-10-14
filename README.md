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
  (arbitrage du 2026-10-10 ; ruleset de la branche : #436).
- **Staging** suit `main` : Argo CD le synchronise automatiquement.
  **Prod** suit les tags `v*` : un merge ici la laisse `OutOfSync` jusqu'à
  ce que l'utilisateur clique « Sync » (#432).
- **Images par empreinte** (`@sha256:…`), jamais par étiquette seule : une
  étiquette se déplace, une empreinte non. Les images sont publiées, signées
  et attestées par `.github/workflows/images.yml` de `main` (#430) ; les PR
  qui déplacent les empreintes sont ouvertes par #436.

## Arborescence

```
apps/manage-our-home/
  base/                 commun à tous les environnements
    api.yaml            Deployment, Service, PodDisruptionBudget
    web.yaml            idem
    migrate-job.yaml    Job `migrate`, hook Argo CD PreSync (#425)
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

## Secrets

Aucune valeur secrète n'est dans cette branche : la base nomme des Secrets,
leurs valeurs arrivent scellées (`SealedSecret`, #432). `ci/render-test.sh`
échoue si un overlay rend un `Secret`, ou passe en clair une des variables
listées ci-dessous.

| Secret | Clés | Lu par |
|---|---|---|
| `api-secrets` | `DATABASE_URL`, `ADMIN_DATABASE_URL`, `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `OAUTH_ENCRYPTION_KEY`, `MESSAGE_ENCRYPTION_KEY`, `CALENDAR_FEED_ENCRYPTION_KEY`, `LOGIN_THROTTLE_KEY`, `VAPID_PRIVATE_KEY` (facultative), `SMTP_USERNAME`, `SMTP_PASSWORD`, `MINIO_ACCESS_KEY`, `MINIO_SECRET_KEY` | api |
| `migrate-secrets` | `MIGRATION_DATABASE_URL` | Job `migrate` |
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
écritures ; au-delà, l'opérateur supprime sauvegardes et WAL.

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
charge par l'opérateur 1.30.1 installé (chart 0.29.1), sans date de
retrait annoncée. Le greffon ajouterait un Deployment et un conteneur dans
le pod de Postgres, sur le budget du serveur de 4 Go ; la bascule se fera
quand l'opérateur l'imposera.

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
3. **Le bucket à `T`** : restaurer le bucket des pièces jointes au même
   instant, depuis sa propre sauvegarde (MinIO dans le cluster, #435). Sans
   sauvegarde du bucket à `T`, ne rien supprimer : copier le bucket à part,
   et ne relancer l'api qu'avec un `ADMIN_DATABASE_URL` sur un rôle sans
   `BYPASSRLS`, ce qui fait refuser chaque passe de réconciliation (et
   coupe aussi les routes `/admin/*` et les purges horaires) jusqu'à ce que
   lignes et objets soient remis d'accord (item 9).
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
   dans un dossier qui contient déjà les WAL d'un autre cluster, et
   l'ancien dossier reste la source de cette restauration et des
   suivantes. Le garder ensuite (`postgres-r2` à la restauration
   d'après) ; le `bootstrap` ne sert qu'à la création, le laisser ne
   change plus rien.
6. **Vérifier** : `kubectl -n manage-our-home get cluster postgres` en
   `Cluster in healthy state`, condition `ContinuousArchiving` vraie, les
   données attendues présentes.
7. **Relancer l'api** (`replicas` de l'overlay), puis rétablir la synchro
   automatique, la racine en dernier.

Le job k3d joue les étapes 1, 2, 4, 5, 6 et la relance de l'api
(`ci/postgres-test.sh`) : une ligne écrite avant `T`, une après, le
Cluster supprimé avec son volume et recréé avec ces trois changements
depuis un MinIO jetable — appliqués par `kubectl` au rendu de l'overlay,
là où la procédure passe par une PR et la synchro d'Argo CD ; la première
ligne est là, la seconde non, les rôles sont toujours ceux de
`01-roles.sh`, RLS s'applique, le Cluster restauré archive de nouveau et
l'api répond 200 sur `/readyz`. Le bucket des pièces jointes (étape 3)
arrive avec #435.

## Réglages à fixer

Les adresses publiques et le relais SMTP des overlays sont des
emplacements en `.invalid` (RFC 2606 : ne résout nulle part) : l'adresse de
chaque environnement vient avec l'ingress (#434). Les buckets de
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
   ClusterIssuers, ClusterPolicy, Cluster et ScheduledBackup).
2. `ci/render-test.sh` : tests sur le rendu des overlays — empreintes,
   limites mémoire, jamais sous leur plancher (api 256 Mi et web 448 Mi,
   le pire cas borné ci-dessus ; migrate 128 Mi, la limite sous laquelle
   les migrations de `main` ont tourné, observée et non bornée), sécurité
   des conteneurs, sondes, délais d'arrêt, une seule source `envFrom` (la
   ConfigMap, sans préfixe), hook `PreSync` du Job et sa politique de
   suppression, PDB,
   réplicas et étalement par environnement, aucun secret en clair ; et
   sur le rendu de Postgres : une instance, anti-affinité, image par
   empreinte, mémoire (256 à 384 Mi en staging) et `shared_buffers` sous
   le quart de la limite, paramètres présents, budget de connexions,
   rôles et leurs attributs, sauvegarde vers un bucket Scaleway propre à
   l'environnement avec rétention, sans en-tête de chiffrement refusé.
3. `ci/platform-test.sh` : tests sur le rendu de la plateforme — prod
   sans `syncPolicy.automated` (racine, appli, Postgres), branche
   suivie, ordre des vagues (Postgres entre les Secrets et l'appli), namespace
   `restricted`, politique Kyverno (et aucune exclusion, précondition
   ni sélecteur qui restreigne sa règle), charts épinglés, et sur le rendu Helm
   de chaque chart : limite mémoire sur chaque conteneur, un réplica,
   Argo CD sans Dex, notifications ni ApplicationSet, Kyverno réduit au
   contrôleur d'admission.
4. `ci/staging-smoke.sh` dans un cluster k3d jetable dont le nœud a les
   4 Go du serveur de test : un Postgres de référence créé avec les rôles
   de `infra/postgres/init/01-roles.sh` de `main` et un MinIO jetable pour
   les sauvegardes, puis `platform/bootstrap.sh staging`, la racine pointée
   sur le commit testé ; les Secrets scellés avec le contrôleur du
   cluster ; Argo CD synchronise la plateforme, le Postgres de staging
   (sauvegardé vers ce MinIO, seul écart au rendu, `ci/postgres-backup-patch.yaml`)
   puis l'appli (Job de migration en PreSync, api et web
   jusqu'à `Available` et `/readyz` à 200, sans redémarrage, en lecture
   seule, sur le réseau des pods que l'api croit). Kyverno doit refuser
   l'image de l'appli par étiquette et un manifeste non signé de son dépôt,
   et admettre l'image signée. Puis 10 minutes au repos sans redémarrage
   ni OOM, tous les pods prêts, et `kubectl top pods -A`.
5. `ci/postgres-test.sh` sur le même cluster : rôles identiques à ceux de
   `01-roles.sh`, RLS appliquée à `app_role`, puis la restauration à un
   instant (« Restaurer » ci-dessus).

Les outils (kustomize, kubeconform, yq, kubectl, k3d, helm, kubeseal,
cosign) sont épinglés par version et empreinte SHA-256 dans
`ci/install-tools.sh`, l'image k3s par étiquette et empreinte dans le
workflow. En local, avec ces outils dans le `PATH` :
`ci/validate.sh && ci/render-test.sh && ci/platform-test.sh`.
