# platform/

Socle des clusters (#432) : Argo CD, qui se gère lui-même après amorçage,
Sealed Secrets, cert-manager (émetteurs Let's Encrypt), opérateur
CloudNativePG, Kyverno. Modèle « app of apps » : une Application racine
par cluster rend `clusters/<env>/`, qui contient toutes les autres.

```
bootstrap.sh            amorçage d'un cluster : Argo CD par Helm, puis la racine
components/             une brique par dossier (Component Kustomize)
  argocd/               Application d'Argo CD lui-même
  sealed-secrets/       contrôleur Sealed Secrets (kube-system)
  cert-manager/         cert-manager et ClusterIssuers Let's Encrypt
  cloudnative-pg/       opérateur CloudNativePG (les bases : #433)
  kyverno/              Kyverno et la politique des images signées
  app-namespace/        namespace `manage-our-home`, Pod Security `restricted`
clusters/
  staging/              serveur maison, 4 Go
  prod/                 VPS tiers
    root.yaml           l'Application racine, appliquée par bootstrap.sh
    kustomization.yaml  les briques de ce cluster (liste de components)
    manage-our-home.yaml l'Application de l'appli
    postgres.yaml       l'Application de son Postgres (#433)
    secrets/            ses SealedSecrets, scellés pour ce cluster
```

## Environnements

Le paramètre d'environnement de l'amorçage choisit la racine, donc les
briques (la liste `components:` de `clusters/<env>/kustomization.yaml`) et
la branche suivie. L'observabilité (#437) viendra comme une brique de plus,
listée en prod seulement.

| | staging | prod |
|---|---|---|
| Racine `platform` | synchro automatique | **manuelle** |
| `manage-our-home-<env>` | synchro automatique | **manuelle**, sans `syncPolicy.automated` |
| `postgres-<env>` | synchro automatique | **manuelle**, sans `syncPolicy.automated` |
| Briques (Argo CD, Kyverno…) | synchro automatique | automatique, mais leur version et leurs valeurs ne changent qu'à la synchro manuelle de la racine |
| Branche lue | `k3s` pendant l'intégration, `deploy` ensuite | `deploy` |

En prod, un merge sur `deploy` laisse `platform` ou `manage-our-home-prod`
`OutOfSync` ; rien n'est déployé avant que l'utilisateur clique « Sync »
dans Argo CD (arbitrage du 2026-10-10). Aucun jeton Argo CD n'est donné à
la CI ni aux agents.

**Promotion `k3s` → `deploy`** : la PR doit passer `targetRevision: k3s` à
`deploy` dans `clusters/staging/root.yaml` et
`clusters/staging/manage-our-home.yaml` et `clusters/staging/postgres.yaml` ;
`ci/platform-test.sh` échoue sur
une PR vers `deploy` tant que staging suit `k3s`.

## Ordre de la première synchro

Le Job `migrate` est un hook `PreSync` de l'Application de l'appli : il
tourne avant qu'elle applique quoi que ce soit d'autre. Le namespace et le
Secret `migrate-secrets` doivent donc exister avant elle ; c'est la racine
qui les pose, par vagues de synchro (`argocd.argoproj.io/sync-wave`), une
vague ne commençant qu'une fois la précédente `Healthy` :

| Vague | Quoi |
|---|---|
| -10 | les Applications des briques |
| -5 | namespace `manage-our-home`, ClusterIssuers, politique Kyverno |
| -3 | SealedSecrets de `clusters/<env>/secrets/` |
| -2 | `postgres-<env>` : le Cluster CloudNativePG de l'appli (#433) |
| 0 | `manage-our-home-<env>` |

Postgres a son Application, avant celle de l'appli, pour la même raison :
le Job `migrate` doit trouver la base debout. La racine passe à l'appli
quand le Cluster est `Healthy`. En prod, `postgres-prod` et
`manage-our-home-prod` sont manuelles : à la première synchro, la racine
attend que l'utilisateur synchronise `postgres-prod`, puis l'appli.

La santé d'une Application enfant n'est pas calculée par Argo CD depuis
1.8 : `components/argocd/application.yaml` lui rend ce contrôle
(`resource.customizations.health.argoproj.io_Application`), sans quoi la
racine n'attendrait pas ses briques.

## Amorcer un cluster

Prérequis : un k3s, le contexte `kubectl` pointé dessus, `helm`, `kubectl`,
`yq` et `kubeseal` aux versions de `ci/install-tools.sh`.

1. `platform/bootstrap.sh staging` (ou `prod`) : installe Argo CD avec Helm,
   à la version et avec les valeurs de `components/argocd/application.yaml`,
   puis applique `clusters/<env>/root.yaml`. Argo CD installe le reste et
   reprend sa propre release Helm.
2. Quand le contrôleur Sealed Secrets tourne, sceller les Secrets de l'appli
   (ci-dessous), les commiter dans `clusters/<env>/secrets/` par PR. Tant
   qu'ils manquent, le Job `migrate` attend son Secret et la synchro de
   l'appli échoue au bout de son `activeDeadlineSeconds`, puis réessaie.
   Le Cluster Postgres, lui, est déjà créé (sa vague passe, celle des
   Secrets étant vide) : une fois ses Secrets arrivés, toucher l'objet
   Cluster pour qu'il les relise, sans quoi l'archivage des WAL reste en
   échec (« failed to get envs: cache miss ») — constaté en k3d avec
   l'opérateur 1.30.1, et joué par le job k3d :
   `kubectl -n manage-our-home annotate cluster postgres manage-our-home/secrets-sealed-at="$(date -u +%FT%TZ)" --overwrite`,
   puis attendre `kubectl -n manage-our-home wait cluster/postgres --for=condition=ContinuousArchiving`.
3. Sauvegarder la clé du contrôleur (ci-dessous).

Mot de passe `admin` initial d'Argo CD :
`kubectl -n argocd get secret argocd-initial-admin-secret -o jsonpath='{.data.password}' | base64 -d`,
puis `kubectl -n argocd port-forward svc/argocd-server 8080:443`. Pas de
Dex : seul le compte local existe. Changer le mot de passe, puis supprimer
ce Secret.

## Secrets

Aucune valeur secrète en clair dans la branche. Les Secrets de l'appli
(`api-secrets`, `migrate-secrets` ; clés dans le `README.md` racine) sont
des `SealedSecret`, chiffrés pour **un** cluster : seul son contrôleur
détient la clé privée qui les ouvre. `ci/platform-test.sh` échoue si un
`Secret` est rendu, ou si `secrets/` contient autre chose que des
`SealedSecret`.

### Sceller

Sur un poste qui a accès au cluster, sans jamais écrire le Secret en clair
sur disque :

```sh
kubectl -n manage-our-home create secret generic migrate-secrets \
  --dry-run=client -o yaml \
  --from-literal=MIGRATION_DATABASE_URL='postgres://…' \
  | kubeseal --format yaml > platform/clusters/staging/secrets/migrate-secrets.yaml
```

puis l'ajouter à `resources:` de `secrets/kustomization.yaml`. Idem pour
`api-secrets`, une `--from-literal` par clé, et pour les Secrets du
Cluster Postgres (`README.md` racine, « Secrets ») ; ceux des rôles sont de
type `kubernetes.io/basic-auth` :

```sh
kubectl -n manage-our-home create secret generic postgres-app-role \
  --type=kubernetes.io/basic-auth --dry-run=client -o yaml \
  --from-literal=username=app_role --from-literal=password='…' \
  | kubeseal --format yaml > platform/clusters/staging/secrets/postgres-app-role.yaml
```

Le mot de passe d'un rôle est aussi dans l'URL qui le nomme
(`api-secrets`, `migrate-secrets`) : les changer ensemble. Sans accès au cluster :
`kubeseal --fetch-cert > staging.pem` une fois, puis `kubeseal --cert
staging.pem`. Un SealedSecret est lié à son nom et à son namespace : le
renommer le rend illisible.

### Sauvegarder la clé du contrôleur

Perdre la clé privée, c'est perdre la capacité d'ouvrir **tous** les
SealedSecrets du cluster : il faudrait tout resceller. Elle n'est jamais
commitée. Le contrôleur en crée une nouvelle tous les 30 jours et garde
les anciennes : sauvegarder après l'amorçage, puis après chaque
renouvellement.

```sh
kubectl -n kube-system get secret \
  -l sealedsecrets.bitnami.com/sealed-secrets-key \
  -o yaml > sealed-secrets-keys-staging.yaml
```

Ce fichier contient les clés privées en clair : le chiffrer (`age`, `gpg`)
et le ranger hors du serveur et hors du dépôt, un fichier par cluster.

**Restaurer** (cluster réinstallé) : appliquer ce fichier **avant**
l'amorçage, puis amorcer ; le contrôleur reprend les clés qu'il trouve au
démarrage. Si le contrôleur tourne déjà :
`kubectl -n kube-system delete pod -l app.kubernetes.io/name=sealed-secrets`.

## Kyverno

`components/kyverno/signed-images.yaml`, en `Enforce` sur le namespace
`manage-our-home` : une image `ghcr.io/maelprog/manage-our-home-*` n'est
admise que référencée par empreinte et signée sans clé par
`.github/workflows/image-build.yml` de `main`, appelé depuis `main` ou un
tag `v*` (#430), signature inscrite dans Rekor. Les signatures de cosign v3
sont des bundles Sigstore attachés à l'image (référents OCI) : la règle est
de type `SigstoreBundle`.

Kyverno tourne réduit à son contrôleur d'admission (arbitrage du
2026-10-12) : ni contrôleur d'arrière-plan, ni nettoyage, ni rapports. La
politique n'est évaluée qu'à l'admission. Kyverno injoignable, l'admission
des pods de l'appli échoue (`failurePolicy: Fail`) plutôt que de laisser
passer.

## Mémoire

Requête et limite mémoire sur chaque conteneur des briques, un réplica de
chaque composant ; Argo CD sans Dex, notifications ni ApplicationSet.
`ci/platform-test.sh` le vérifie sur le rendu Helm des charts. Le job k3d
borne le nœud à 4 Go, laisse tout tourner 10 minutes et imprime `kubectl
top pods -A` avec le total de la plateforme.

## Versions

Chaque chart est épinglé à une version exacte dans son
`components/*/application.yaml`. Le suivi par Renovate (gestionnaire
`argocd`) demande que la configuration Renovate de `main`, seule lue,
inclue la branche `deploy` : ce n'est pas encore le cas.
