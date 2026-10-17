# platform/

Socle des clusters (#432) : Argo CD, qui se gère lui-même après amorçage,
Sealed Secrets, cert-manager (émetteurs Let's Encrypt, et l'autorité
maison du staging), opérateur
CloudNativePG, Kyverno, et les réglages de Traefik, l'ingress de k3s (#434). Modèle « app of apps » : une Application racine
par cluster rend `clusters/<env>/`, qui contient toutes les autres.

```
bootstrap.sh            amorçage d'un cluster : Argo CD par Helm, puis la racine
components/             une brique par dossier (Component Kustomize)
  argocd/               Application d'Argo CD lui-même
  sealed-secrets/       contrôleur Sealed Secrets (kube-system)
  cert-manager/         cert-manager et ClusterIssuers Let's Encrypt
  home-ca/              autorité de certification du réseau maison, en staging seulement
  traefik/              réglages du Traefik de k3s (HelmChartConfig, #434)
  cloudnative-pg/       opérateur CloudNativePG (les bases : #433)
  kyverno/              Kyverno, la politique des images signées et la liste fermée des images
  app-namespace/        namespace `manage-our-home`, Pod Security `restricted`
  observability/        métriques, journaux et alertes (#437), en prod seulement
clusters/
  staging/              serveur maison, 4 Go
  prod/                 VPS tiers
    root.yaml           l'Application racine, appliquée par bootstrap.sh
    kustomization.yaml  les briques de ce cluster (liste de components)
    manage-our-home.yaml l'Application de l'appli
    postgres.yaml       l'Application de son Postgres (#433)
    secrets/            ses SealedSecrets, scellés pour ce cluster
    monitoring-secrets/ ceux de l'observabilité, namespace `monitoring` (#437)
```

## Environnements

Le paramètre d'environnement de l'amorçage choisit la racine, donc les
briques (la liste `components:` de `clusters/<env>/kustomization.yaml`) et
la branche suivie. L'observabilité (#437) est une brique de plus, listée
en prod seulement : sur les 4 Go du serveur de test, elle ne tient pas à
côté du reste (arbitrage du 2026-10-12). La surveillance du staging se
limite à `kubectl top` et aux journaux des pods. `ci/platform-test.sh`
échoue si le rendu de staging en contient quoi que ce soit.

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
| -5 | namespace `manage-our-home`, ClusterIssuers, réglages de Traefik, politique Kyverno ; en prod, les alertes et sondes de l'observabilité |
| -3 | SealedSecrets de `clusters/<env>/secrets/` ; en prod, ceux de `monitoring-secrets/` |
| -2 | `postgres-<env>` : le Cluster CloudNativePG de l'appli (#433) |
| 0 | `manage-our-home-<env>` |

Dans l'Application de l'appli, après le Job `migrate` : MinIO (vague -2),
le Job qui crée son bucket (hook `Sync`, vague -1), puis api et web
(vague 0), chaque vague attendant que la précédente soit `Healthy`
(`README.md` racine, « MinIO »).

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

Prérequis : un k3s (installé comme le décrit `docs/install-k3s.md`), le
contexte `kubectl` pointé dessus, `helm`, `kubectl`, `yq` et `kubeseal`
aux versions de `ci/install-tools.sh`.

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
(`api-secrets`, `migrate-secrets`, `minio` ; clés dans le `README.md` racine) sont
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

`components/kyverno/allowed-images.yaml`, en `Enforce` sur le même
namespace (#435) : une **liste fermée** d'images. Un pod dont un conteneur
(init et éphémères compris) nomme une autre image est refusé. La liste :
les images de l'appli (`ghcr.io/maelprog/manage-our-home-*`, que la
politique précédente exige par empreinte et signées), MinIO et son client
(`README.md` racine, « MinIO »), l'image Postgres du Cluster, et celle de
l'opérateur CloudNativePG, qu'il met dans chaque pod Postgres
(`bootstrap-controller`), par étiquette et empreinte : le `image.tag` de
`components/cloudnative-pg/application.yaml` porte l'empreinte.
Chacune y est écrite exactement : déplacer une image, c'est la déplacer
dans la liste dans la même PR. `ci/platform-test.sh` échoue si une image
de l'overlay, de son Postgres ou de l'opérateur manque à la liste, si la
liste nomme une image que rien ne lance, ou si une entrée autre que celle
de l'appli contient un joker ou n'a pas d'empreinte.

Kyverno tourne réduit à son contrôleur d'admission (arbitrage du
2026-10-12) : ni contrôleur d'arrière-plan, ni nettoyage, ni rapports. Les
politiques ne sont évaluées qu'à l'admission. Kyverno injoignable, l'admission
des pods de l'appli échoue (`failurePolicy: Fail`) plutôt que de laisser
passer.

## Observabilité

`components/observability/` (#437), sur le cluster de prod seulement, dans
le namespace `monitoring` :

- **kube-prometheus-stack** : l'opérateur Prometheus, un Prometheus
  (mesures gardées 15 jours, 8 Go au plus de son volume de 10 Gi), un
  Alertmanager, Grafana, kube-state-metrics et node-exporter. Ni le
  contrôleur, ni l'ordonnanceur, ni le proxy, ni etcd, que k3s fait
  tourner dans son propre processus, ni l'API server, ne sont lus.
- **Loki**, en un seul processus, ses données sur un volume de 10 Gi, et
  **Grafana Alloy**, qui lit les journaux de chaque conteneur par l'API
  Kubernetes (comme `kubectl logs`) et les lui envoie. Alloy ne lit que
  les pods et leurs journaux : aucun Secret.
- **Rétention des journaux : 14 jours** (`retention_period: 336h`,
  `loki.yaml`), appliquée par le compacteur de Loki. Ils peuvent contenir
  des adresses IP : cette durée est celle que la PR #505 inscrit au
  registre des traitements et dans la politique de confidentialité de
  `main` (« Journaux techniques ») ; une fois celle-ci mergée, elle ne
  change qu'avec eux. `ci/platform-test.sh` la
  vérifie sur le rendu, le job `observability` sur le Loki qui tourne.

### Alertes

Celles qui partent par mail sont celles de `alerts.yaml`, chacune
marquée `notify: mail` :

| Alerte | Quand |
|---|---|
| `ContainerNotReady` | un conteneur en marche ne passe plus sa sonde readiness depuis 5 min |
| `ContainerRestarting` | 3 redémarrages ou plus en 30 min (sonde liveness, plantage) |
| `MigrationJobFailed` | le Job `migrate` de l'appli a échoué |
| `PostgresBackupLate` | dernière copie complète de plus de 26 h (une chaque nuit à 3 h UTC) |
| `PostgresWALArchivingFailing` | le dernier envoi d'un WAL a échoué, depuis 10 min |
| `PostgresMetricsAbsent` | plus aucune mesure de Postgres depuis 30 min : les deux précédentes ne pourraient plus partir |
| `CertificateExpiringSoon` | un certificat expire dans moins de 21 jours (cert-manager renouvelle à 30) |
| `CertificateNotReady` | un certificat pas `Ready` depuis 1 h |
| `DiskAlmostFull` | un disque du nœud plein à plus de 80 % depuis 15 min |

Les règles du chart restent, visibles dans Alertmanager et Grafana, sans
mail. `ci/alerts-test.sh` vérifie chaque alerte avec `promtool` contre
`ci/alerts-test.yaml`, et la configuration d'Alertmanager avec `amtool` ;
le job k3d `observability` les fait se déclencher pour de vrai (« CI de
cette branche », README racine). `monitors.yaml` fait lire à Prometheus
les mesures de cert-manager et des instances Postgres, sans toucher à
leurs briques ; `ci/postgres-test.sh` vérifie sur staging que ces pods
les exportent.

Les mails passent par le relais SMTP de l'appli. La configuration
d'Alertmanager, avec le destinataire (une donnée personnelle, qu'une
branche publique ne porte pas) et les identifiants du relais, est le
Secret `alertmanager-config`, scellé pour le cluster. Tant qu'il manque,
Alertmanager tourne avec une configuration qui n'envoie rien. Le
remplir depuis `alertmanager.template.yaml` et le sceller, sans rien
écrire en clair sur disque :

```sh
SMTP_HOST=smtp.tem.scaleway.com SMTP_FROM='…' SMTP_USERNAME='…' \
  SMTP_PASSWORD='…' ALERT_TO='…' \
  platform/components/observability/alertmanager-config.sh \
  | kubectl -n monitoring create secret generic alertmanager-config \
      --dry-run=client -o yaml --from-file=alertmanager.yaml=/dev/stdin \
  | kubeseal --format yaml > platform/clusters/prod/monitoring-secrets/alertmanager-config.yaml
```

puis l'ajouter à `resources:` de `monitoring-secrets/kustomization.yaml`,
par PR. Le mot de passe SMTP est celui de `api-secrets` : les changer
ensemble.

### Grafana

Aucune entrée publique : ni Ingress, ni Service hors du cluster
(`ci/platform-test.sh` le vérifie sur le rendu de chaque chart). Depuis
un poste qui a accès au cluster :

```sh
kubectl -n monitoring get secret kube-prometheus-stack-grafana \
  -o jsonpath='{.data.admin-password}' | base64 -d
kubectl -n monitoring port-forward svc/kube-prometheus-stack-grafana 3000:80
```

puis `http://localhost:3000`, compte `admin`. Le mot de passe est tiré
par le chart à la première synchro, puis gardé : l'Application ignore ce
champ du Secret et l'empreinte qu'en portent les pods de Grafana, que
chaque rendu tirerait à nouveau. Sans volume, Grafana repart à chaque
redémarrage des tableaux du chart et de ce mot de passe. Les sources
Prometheus et Loki y sont déjà.

## Mémoire

Requête et limite mémoire sur chaque conteneur des briques, un réplica de
chaque composant ; Argo CD sans Dex, notifications ni ApplicationSet.
`ci/platform-test.sh` le vérifie sur le rendu Helm des charts. Traefik, dont
k3s fournit le chart, reçoit les siennes de son HelmChartConfig (96 / 256 Mi ;
au repos après les 10 minutes du job k3d, 32 Mi sur GitHub (run
38896429123, #437), 88, 93, 106 et 120 Mi sur quatre passages en local : la requête est dans cet intervalle, la
limite au-dessus de tous) : vérifiées sur ce HelmChartConfig. Le job k3d
borne le nœud à 4 Go, laisse tout tourner 10 minutes et imprime `kubectl
top pods -A` avec le total de la plateforme.

L'observabilité (prod) suit la même règle, Prometheus et Alertmanager
compris (leurs limites sont sur leurs ressources `Prometheus` et
`Alertmanager`, celles des rechargeurs de configuration sur les arguments
de l'opérateur). Le job `observability` imprime aussi `kubectl top` après
5 minutes au repos : 1 267 Mi pour le namespace `monitoring` sur un
passage local du job (Grafana et ses deux sidecars 481 Mi, Prometheus
346 Mi, Alloy 139 Mi, Loki 113 Mi), avec un quart d'heure de mesures
et de journaux seulement : Prometheus et Loki grossissent avec eux, sous
leurs limites de 1 Gi et 512 Mi.

## Versions

Chaque chart est épinglé à une version exacte dans son
`components/*/application.yaml`. Le suivi par Renovate (gestionnaire
`argocd`) demande que la configuration Renovate de `main`, seule lue,
inclue la branche `deploy` : ce n'est pas encore le cas.

Traefik fait exception : son chart et sa version sont ceux de k3s (Traefik
3.7.13 avec k3s v1.37.1) et changent avec lui ; la brique `traefik/` ne
porte que des valeurs.
