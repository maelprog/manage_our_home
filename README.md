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
  overlay (#432). Personne ne fait de `kubectl apply` à la main, la CI non
  plus : aucun jeton Argo CD ni accès cluster ne lui est donné.
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
    namespace.yaml      namespace, Pod Security « restricted »
    api.yaml            Deployment, Service, PodDisruptionBudget
    web.yaml            idem
    migrate-job.yaml    Job `migrate`, hook Argo CD PreSync (#425)
    kustomization.yaml  ConfigMaps (réglages en clair)
  overlays/
    staging/            serveur maison, 4 Go : 1 réplica, étalement souple
    prod/               VPS tiers : la base telle quelle, 2 réplicas
platform/               socle du cluster (#432)
ci/                     scripts de la CI de cette branche
.github/workflows/deploy.yml
```

Rendre un overlay : `kustomize build apps/manage-our-home/overlays/staging`.

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
  `RuntimeDefault`, aucun jeton de compte de service monté. Le namespace
  est en Pod Security `restricted` : l'API server refuse un pod qui s'en
  écarte.
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

Le sens de chaque variable est dans `apps/api/README.md` et
`infra/generate-env.sh` sur `main`.

**Premier déploiement d'un environnement** : le Job `migrate` est un hook
`PreSync`, il tourne avant que la synchro applique les autres ressources,
`namespace.yaml` compris. Le namespace `manage-our-home` (avec ses labels
Pod Security) et le Secret `migrate-secrets` doivent donc **exister avant
le PreSync** ; tels quels, ces manifests ne le garantissent pas, et le
premier déploiement d'un environnement échouerait.

La voie reste à trouver et à éprouver avec #432. Pistes **non vérifiées** :

- créer le namespace et le Secret de migration côté `platform/`, hors de
  l'Application de l'appli, avant sa première synchro ;
- retirer `namespace.yaml` du rendu et faire créer le namespace par
  l'Application (`CreateNamespace=true`), labels Pod Security et vague
  de synchro ≤ -2 portés par `managedNamespaceMetadata`, le `SealedSecret`
  de `migrate-secrets` en hook `PreSync` d'une vague intermédiaire.

À écarter : `CreateNamespace=true` en gardant `namespace.yaml` dans le
rendu. D'après une lecture du code d'Argo CD (gitops-engine,
`sync_context.go`) faite en revue de cette PR et non éprouvée sur un
cluster, le namespace n'est alors pas créé d'avance, puisque le rendu le
contient, et `managedNamespaceMetadata` est ignoré.

## Réglages à fixer

Les adresses publiques et le relais SMTP des overlays sont des
emplacements en `.invalid` (RFC 2606 : ne résout nulle part) : l'adresse de
chaque environnement vient avec l'ingress (#434).

## CI de cette branche

`.github/workflows/deploy.yml`, sur chaque PR vers `deploy` et chaque push :

1. `ci/validate.sh` : `kustomize build` de chaque overlay, `kubeconform
   -strict` contre les schémas de Kubernetes 1.37.1.
2. `ci/render-test.sh` : tests sur le rendu — empreintes, limites mémoire,
   sécurité des conteneurs, sondes, délais d'arrêt, PDB, réplicas et
   étalement par environnement, aucun secret en clair.
3. `ci/staging-smoke.sh` dans un cluster k3d jetable : un Postgres de test
   créé avec les rôles de `infra/postgres/init/01-roles.sh` de `main`, le
   Job de migration d'abord, puis api et web jusqu'à `Available` et
   `/readyz` à 200, sans redémarrage, en lecture seule, sur le réseau des
   pods que l'api croit. Puis `kubectl top`.

Les outils (kustomize, kubeconform, yq, kubectl, k3d) sont épinglés par
version et empreinte SHA-256 dans `ci/install-tools.sh`, l'image k3s par
étiquette et empreinte dans le workflow. En local, avec ces outils dans le
`PATH` : `ci/validate.sh && ci/render-test.sh`.
