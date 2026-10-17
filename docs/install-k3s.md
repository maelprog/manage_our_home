# Installer un cluster k3s et y basculer depuis Compose

Procédure pour monter les deux clusters qui font tourner manage_our_home
(#438), y amorcer Argo CD, puis y reprendre les données d'une pile Compose
(`infra/docker-compose.yml` de `main`). Ce que les clusters font tourner
est décrit ailleurs : `README.md` (l'appli, Postgres, MinIO, la promotion)
et `platform/README.md` (Argo CD, Sealed Secrets, Kyverno,
l'observabilité). Ce document s'arrête là où commence la branche : il
prépare les machines, installe k3s et ce qui tourne à côté d'Argo CD,
puis passe la main.

Conventions : `<…>` est à remplacer ; `serveur$` se tape sur un nœud
(en SSH), `poste$` sur le poste d'administration (avec `kubectl`, `helm`,
`yq` et `kubeseal`, ce dépôt cloné), `compose$` sur la machine qui fait
tourner Compose. Les adresses d'exemple sont celles d'un réseau maison
`192.168.1.0/24`.

## 1. Les deux cibles

Arbitrage (b) du 2026-10-10 :

| | staging | prod |
|---|---|---|
| Machines | serveur maison, un ou plusieurs nœuds | VPS loué à un hébergeur tiers, un nœud |
| Usage | familial, essais | public |
| Argo CD | synchro automatique | « Sync » manuel, rien ne part sans clic |
| Branche suivie | `k3s` pendant l'intégration, `deploy` ensuite | `deploy` |
| Observabilité (#437) | non | oui |
| Entrée HTTP | ServiceLB (un nœud) ou MetalLB (plusieurs) | ServiceLB |
| Adresse, certificat | `staging.home.arpa`, réseau maison seulement, autorité maison | nom public, Let's Encrypt |
| Adresse de l'API Kubernetes | kube-vip (plusieurs serveurs) | celle du nœud |

### Prérequis matériels

**Staging, le serveur de test** (arbitrage du 2026-10-12) : 4 Go de RAM,
un swap déjà en place, 1 To de disque. Il porte la plateforme (Argo CD,
Sealed Secrets, cert-manager, CloudNativePG, Kyverno), Postgres, MinIO, api
et web, dans un budget d'environ 3 Go pour tout le cluster. N'y tournent
**pas** :

- **Ollama** : reporté après la v1 (aucun manifeste ne le décrit) ;
- **l'observabilité** (#437) : Prometheus, Alertmanager, Grafana, Loki et
  Alloy ne tiennent pas à côté du reste ; la surveillance du staging se
  limite à `kubectl top` et aux journaux des pods (`platform/README.md`).

Chaque conteneur a une limite mémoire (`ci/render-test.sh` et
`ci/platform-test.sh` le vérifient sur le rendu), une seule instance de
chaque composant, Argo CD et Kyverno allégés. Sur le rendu de la branche
(#438, conteneurs principaux, Traefik compris, sans les autres pods
propres à k3s), les requêtes
mémoire du staging font 1 160 Mi et ses limites 4 512 Mi : une limite
n'est pas réservée, la somme peut dépasser la RAM. Relevé sur une VM
Debian 13 de 4 Go (2 Go de swap) avec un seul nœud, le staging déployé
par Argo CD : `kubectl top nodes` à 3 010 Mi juste après l'installation,
3 047 Mi au repos après un redémarrage, dont 88 à 105 Mi pour un MinIO de
test qui n'existe pas au vrai serveur (« Relevé après installation »).

**Staging à plusieurs nœuds** : les nœuds sont reliés en **Ethernet**,
sur le même réseau local. etcd attend une réponse de la majorité des
serveurs à chaque écriture et coupe un membre qui tarde (battement de
100 ms, élection à 1 s par défaut) ; le Wi-Fi, ses pertes et ses pointes
de latence en font sortir des nœuds. Le trafic des pods entre nœuds
(VXLAN de flannel) passe par le même lien.

**Prod** : un VPS d'au moins **8 Go** de RAM. Elle fait tourner tout ce que
porte le staging, avec la base telle quelle (api et web à 2 réplicas,
Postgres à 512 Mi de requête et 1 Gi de limite au lieu de 256 et 384 Mi),
plus l'observabilité : 1 267 Mi pour le namespace `monitoring` au repos
sur un passage local du job `observability` (`platform/README.md`,
« Mémoire »), qui grossit avec les mesures et les journaux gardés. Sur
le rendu, ses requêtes font 2 544 Mi et ses limites 8 992 Mi (mêmes
règles de compte que ci-dessus) ; 8 Go laissent la place des requêtes,
de k3s et du système, pas celle de toutes les limites atteintes à la
fois. Aucune prod n'a encore tourné : ce chiffre n'est pas mesuré.

**Disque** : un SSD pour `/var/lib/rancher/k3s`, qui porte etcd, les
images et, avec la classe `local-path`, les volumes de Postgres (10 Gi en
staging, 20 Gi en prod) et de MinIO (20 Gi). Voir « Le disque d'etcd ».

## 2. Préparer chaque machine

Sur chaque nœud, staging et prod. Système : **Debian 13** (trixie) ;
au serveur maison, l'image *netinst* amd64, installée sans environnement
de bureau, avec « serveur SSH » et « utilitaires usuels du système »
seulement ; au VPS, l'image Debian 13 de l'hébergeur. Au serveur maison,
fixer l'adresse de chaque nœud par un bail DHCP statique sur la box : etcd
et les agents retrouvent les serveurs par leur adresse.

### Accès

Depuis le poste, une clé par administrateur :

```sh
poste$ ssh-keygen -t ed25519            # si pas déjà de clé
poste$ ssh-copy-id <utilisateur>@<nœud>
```

Sur le nœud :

```sh
serveur$ sudo apt update && sudo apt install -y curl ca-certificates ufw unattended-upgrades fio
serveur$ sudo apt full-upgrade -y
serveur$ [ ! -e /var/run/reboot-required ] || sudo reboot     # un noyau neuf : redémarrer avant k3s
serveur$ printf 'PasswordAuthentication no\nPermitRootLogin no\n' | sudo tee /etc/ssh/sshd_config.d/00-durcissement.conf
serveur$ sudo sshd -t && sudo systemctl restart ssh
serveur$ sudo sshd -T | grep -E '^(passwordauthentication|permitrootlogin) '   # no, no
```

Dans un fichier de `sshd_config.d/` au nom qui passe en premier, et non
dans `sshd_config` : sshd garde la **première** valeur lue, et
`sshd_config` inclut ce dossier avant ses propres lignes. Une image
cloud-init (celle d'un VPS) y dépose `50-cloud-init.conf`, qui peut
rouvrir la connexion par mot de passe. `sshd -T` affiche ce qui
s'applique.

Ouvrir une **seconde** session SSH avant de fermer la première : une
erreur ici ferme la porte.

### Mises à jour de sécurité

```sh
serveur$ echo 'unattended-upgrades unattended-upgrades/enable_auto_updates boolean true' | sudo debconf-set-selections
serveur$ sudo dpkg-reconfigure -f noninteractive unattended-upgrades
serveur$ systemctl is-enabled unattended-upgrades apt-daily-upgrade.timer   # enabled, enabled
```

unattended-upgrades ne redémarre pas la machine (son
`Automatic-Reboot` est à `false` par défaut, le laisser ainsi). Après un
nouveau noyau, le paquet pose `/var/run/reboot-required` (son crochet
`/etc/kernel/postinst.d/unattended-upgrades`, d'où son installation
avant `full-upgrade` ci-dessus), et c'est kured qui redémarre, un nœud à
la fois, après l'avoir vidé (« kured »).

### Pare-feu

ufw ne voit pas tout : k3s écrit ses propres règles (kube-proxy, flannel,
ServiceLB) et les ports qu'il publie pour un Service passent avant celles
d'ufw. Les règles ci-dessous ouvrent ce que k3s attend d'un nœud ; le
reste reste fermé de l'extérieur. Sur chaque nœud, `<lan>` le réseau des
nœuds (`192.168.1.0/24`) :

```sh
serveur$ sudo ufw allow 22/tcp
serveur$ sudo ufw allow 80/tcp
serveur$ sudo ufw allow 443/tcp
serveur$ sudo ufw allow from 10.42.0.0/16 to any      # réseau des pods
serveur$ sudo ufw allow from 10.43.0.0/16 to any      # réseau des Services
serveur$ sudo ufw allow from <lan> to any port 6443 proto tcp        # API Kubernetes
serveur$ sudo ufw allow from <lan> to any port 2379:2380 proto tcp   # etcd, entre serveurs
serveur$ sudo ufw allow from <lan> to any port 10250 proto tcp       # kubelet
serveur$ sudo ufw allow from <lan> to any port 8472 proto udp        # VXLAN de flannel
serveur$ sudo ufw allow from <lan> to any port 7946                  # MetalLB, entre nœuds
serveur$ sudo ufw --force enable                # sans la question : 22 est déjà ouvert
serveur$ sudo ufw status numbered
```

Au VPS (un seul nœud), seules les trois premières et les deux réseaux du
cluster servent ; l'API Kubernetes s'atteint alors par un tunnel SSH
(`ssh -fN -L 6443:127.0.0.1:6443 <utilisateur>@<vps>`) plutôt que d'ouvrir
6443 sur Internet.

### Swap

Le serveur de test en a déjà un : le **vérifier**, ne pas en recréer.

```sh
serveur$ sudo swapon --show
serveur$ free -h
```

Les pods ne s'en servent pas : k3s lance le kubelet avec
`--fail-swap-on=false` et le comportement de swap par défaut
(`NoSwap`) — un pod qui dépasse sa limite est tué, il ne déborde pas sur le
disque. Le swap sert au système et à k3s lui-même. `vm.swappiness=10`
l'y réserve aux cas de manque :

```sh
serveur$ echo 'vm.swappiness=10' | sudo tee /etc/sysctl.d/99-swappiness.conf
serveur$ sudo sysctl --system | grep swappiness
```

### Le disque d'etcd

etcd écrit chaque transaction avec `fdatasync` ; un disque lent le fait
manquer ses battements, et le cluster perd son leader. Mesure de
référence de la documentation d'etcd (« Hardware recommendations ») :
le 99ᵉ centile de `fdatasync` doit rester **sous 10 ms**. La mesurer dans
le dossier qui portera `/var/lib/rancher/k3s` (avant d'installer k3s, son
parent `/var/lib` s'il est sur le même disque) :

```sh
serveur$ sudo mkdir -p /var/lib/fio-etcd
serveur$ sudo fio --name=etcd --directory=/var/lib/fio-etcd --rw=write --ioengine=sync \
  --fdatasync=1 --size=22m --bs=2300 | grep -A7 'fsync/fdatasync/sync_file_range'
serveur$ sudo rm -rf /var/lib/fio-etcd
```

Lire la ligne `99.00th` du bloc `sync percentiles` (en µs ou en ms selon
l'échelle que `fio` annonce en tête du bloc). Seul le disque réel compte :
les VM de l'essai de ce document (disques virtuels sur un poste de
développement) donnaient 19,3 et 21,9 ms, ce qui ne dit rien du serveur.

**Sur un disque dur** (le 1 To du serveur de test, s'il tourne) : placer
`/var/lib/rancher/k3s` sur un SSD, **avant** d'installer k3s. Avec un SSD
vu comme `/dev/sdb` (`mkfs` l'efface entièrement : vérifier le nom par
sa taille et son modèle) :

```sh
serveur$ lsblk -o NAME,SIZE,ROTA,MODEL          # ROTA=0 : SSD ; ROTA=1 : disque dur
serveur$ sudo mkfs.ext4 -L k3s /dev/sdb
serveur$ sudo mkdir -p /var/lib/rancher/k3s
serveur$ echo 'LABEL=k3s /var/lib/rancher/k3s ext4 defaults,noatime 0 2' | sudo tee -a /etc/fstab
serveur$ sudo systemctl daemon-reload && sudo mount /var/lib/rancher/k3s
serveur$ findmnt /var/lib/rancher/k3s
```

puis refaire la mesure sur ce disque, le dossier de `fio` pris dessous
(`/var/lib/rancher/k3s/fio-etcd`, supprimé ensuite). Le disque dur reste
utile pour ce qui n'est pas dans le cluster (copies de sauvegarde, par
exemple).

## 3. Installer k3s

Version : celle du job k3d (`K3S_IMAGE` de `.github/workflows/deploy.yml`)
et des schémas de `ci/validate.sh`, aujourd'hui **v1.37.1+k3s1**. Le
script d'installation est celui de k3s, téléchargé depuis `get.k3s.io` ;
la version est figée par `INSTALL_K3S_VERSION`. Les réglages vont dans
`/etc/rancher/k3s/config.yaml`, écrit **avant** l'installation : k3s le lit
à chaque démarrage, une option qu'on y change prend effet au
redémarrage suivant.

Ne pas changer `cluster-cidr` (10.42.0.0/16) ni `service-cidr`
(10.43.0.0/16) : `TRUSTED_PROXY_CIDRS` de l'api et le pare-feu ci-dessus
les supposent (`README.md`, « Ce que porte la base »).

### Combien de serveurs

Un nœud **serveur** fait tourner l'API Kubernetes et un membre d'etcd ; un
**agent** ne fait tourner que des pods. etcd ne fonctionne qu'avec la
majorité de ses membres : **1 ou 3 serveurs, jamais 2**. Avec 2, la perte
de l'un arrête le cluster, comme avec 1, et la probabilité de panne
double. Au-delà de 3 nœuds, les suivants sont des agents.

Le premier serveur est installé avec `cluster-init` (etcd embarqué) même
seul : c'est ce qui permet d'en ajouter deux plus tard. Sans lui, k3s
prend SQLite, qu'on ne convertit pas en etcd sur place.

Un jeton partagé, tiré une fois et rangé avec les autres secrets
d'administration (gestionnaire de mots de passe) : il suffit, avec
l'adresse de l'API, à faire entrer une machine dans le cluster.

```sh
poste$ openssl rand -hex 32
```

### Premier serveur

`<vip>` est l'adresse de l'API : au staging à plusieurs serveurs, une
adresse libre du réseau local, hors de la plage DHCP de la box, que
kube-vip portera (ci-dessous) ; au VPS ou avec un seul serveur, l'adresse
du nœud.

```sh
serveur$ sudo mkdir -p /etc/rancher/k3s
serveur$ sudo tee /etc/rancher/k3s/config.yaml > /dev/null <<'EOF'
cluster-init: true
token: <jeton>
tls-san:
  - <vip>
node-ip: <adresse du nœud>
# Staging à plusieurs nœuds seulement : MetalLB remplace ServiceLB.
# disable:
#   - servicelb
EOF
serveur$ sudo chmod 600 /etc/rancher/k3s/config.yaml
serveur$ curl -sfL https://get.k3s.io | sudo INSTALL_K3S_VERSION=v1.37.1+k3s1 sh -s - server
serveur$ sudo k3s kubectl wait --for=create node/$(hostname) --timeout=300s
serveur$ sudo k3s kubectl wait --for=condition=Ready node/$(hostname) --timeout=300s
serveur$ sudo k3s kubectl get nodes -o wide
```

### kube-vip : une adresse pour l'API (staging, plusieurs serveurs)

Les agents et le poste parlent à l'API par une seule adresse ; si elle est
celle d'un serveur, la perte de ce serveur les coupe tous. kube-vip la
porte sur le serveur élu parmi les trois (annonce ARP sur le réseau
local) et la déplace quand il tombe. Il tourne en DaemonSet sur les
serveurs, depuis un manifeste que k3s applique lui-même au démarrage
(`/var/lib/rancher/k3s/server/manifests/`), avant toute synchro d'Argo CD.
`<interface>` est la carte du nœud sur le réseau local (`ip -br link`) :

```sh
serveur$ sudo tee /var/lib/rancher/k3s/server/manifests/kube-vip.yaml > /dev/null <<'EOF'
# kube-vip v1.2.4 (#438) : l'adresse <vip> de l'API Kubernetes, portée
# par un serveur élu. Généré par `kube-vip manifest daemonset --arp
# --controlplane --leaderElection --inCluster --taint`, image fixée par
# empreinte, limite mémoire ajoutée.
apiVersion: v1
kind: ServiceAccount
metadata:
  name: kube-vip
  namespace: kube-system
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: system:kube-vip-role
rules:
  - apiGroups: [""]
    resources: ["services/status"]
    verbs: ["update"]
  - apiGroups: [""]
    resources: ["services", "endpoints"]
    verbs: ["list", "get", "watch", "update"]
  - apiGroups: [""]
    resources: ["nodes"]
    verbs: ["list", "get", "watch", "update", "patch"]
  - apiGroups: ["coordination.k8s.io"]
    resources: ["leases"]
    verbs: ["list", "get", "watch", "update", "create"]
  - apiGroups: ["discovery.k8s.io"]
    resources: ["endpointslices"]
    verbs: ["list", "get", "watch", "update"]
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["list"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: system:kube-vip-binding
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: system:kube-vip-role
subjects:
  - kind: ServiceAccount
    name: kube-vip
    namespace: kube-system
---
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: kube-vip-ds
  namespace: kube-system
spec:
  selector:
    matchLabels:
      app.kubernetes.io/name: kube-vip-ds
  template:
    metadata:
      labels:
        app.kubernetes.io/name: kube-vip-ds
    spec:
      affinity:
        nodeAffinity:
          requiredDuringSchedulingIgnoredDuringExecution:
            nodeSelectorTerms:
              - matchExpressions:
                  - key: node-role.kubernetes.io/control-plane
                    operator: Exists
      containers:
        - name: kube-vip
          image: ghcr.io/kube-vip/kube-vip:v1.2.4@sha256:dde4c0669d9058c74c69c0bc2f0122e26900e1e4b913c03577cb6e4e28083079
          args: ["manager"]
          env:
            - {name: vip_arp, value: "true"}
            - {name: port, value: "6443"}
            - name: vip_nodename
              valueFrom: {fieldRef: {fieldPath: spec.nodeName}}
            - {name: vip_interface, value: <interface>}
            - {name: vip_subnet, value: "32"}
            - {name: cp_enable, value: "true"}
            - {name: cp_namespace, value: kube-system}
            - {name: vip_leaderelection, value: "true"}
            - {name: vip_leasename, value: plndr-cp-lock}
            - {name: vip_leaseduration, value: "15"}
            - {name: vip_renewdeadline, value: "10"}
            - {name: vip_retryperiod, value: "2"}
            - {name: address, value: <vip>}
          resources:
            requests: {memory: 32Mi}
            limits: {memory: 96Mi}
          securityContext:
            capabilities:
              add: [NET_ADMIN, NET_RAW]
              drop: [ALL]
      hostNetwork: true
      serviceAccountName: kube-vip
      tolerations:
        - {effect: NoSchedule, operator: Exists}
        - {effect: NoExecute, operator: Exists}
EOF
serveur$ sudo k3s kubectl -n kube-system wait --for=create ds/kube-vip-ds --timeout=120s
serveur$ sudo k3s kubectl -n kube-system rollout status ds/kube-vip-ds
poste$ curl -sk -o /dev/null -w '%{http_code}\n' https://<vip>:6443/livez   # 401 : l'API répond par l'adresse (k3s refuse l'anonyme)
```

### Serveurs suivants, puis agents

Sur chacun des deux autres serveurs, la même préparation (§ 2), puis :

```sh
serveur$ sudo mkdir -p /etc/rancher/k3s
serveur$ sudo tee /etc/rancher/k3s/config.yaml > /dev/null <<'EOF'
server: https://<vip>:6443
token: <jeton>
tls-san:
  - <vip>
node-ip: <adresse du nœud>
# disable:
#   - servicelb
EOF
serveur$ sudo chmod 600 /etc/rancher/k3s/config.yaml
serveur$ curl -sfL https://get.k3s.io | sudo INSTALL_K3S_VERSION=v1.37.1+k3s1 sh -s - server
```

`disable` doit être le même sur tous les serveurs. Un **agent** :

```sh
serveur$ sudo mkdir -p /etc/rancher/k3s
serveur$ printf 'server: https://<vip>:6443\ntoken: <jeton>\nnode-ip: <adresse du nœud>\n' | sudo tee /etc/rancher/k3s/config.yaml > /dev/null
serveur$ sudo chmod 600 /etc/rancher/k3s/config.yaml
serveur$ curl -sfL https://get.k3s.io | sudo INSTALL_K3S_VERSION=v1.37.1+k3s1 sh -s - agent
```

Puis, depuis un serveur :

```sh
serveur$ sudo k3s kubectl get nodes -o wide        # tous Ready, à la même version
serveur$ sudo k3s etcd-snapshot list               # vide au début : un instantané à 0 h et 12 h, 5 gardés
```

### Le poste d'administration

`kubectl` du poste lit le fichier d'accès administrateur du cluster,
recopié d'un serveur, l'adresse locale remplacée par `<vip>` :

```sh
poste$ mkdir -p ~/.kube
poste$ ssh <utilisateur>@<serveur> sudo cat /etc/rancher/k3s/k3s.yaml \
  | sed 's/127.0.0.1/<vip>/' > ~/.kube/manage-our-home-<env>.yaml
poste$ chmod 600 ~/.kube/manage-our-home-<env>.yaml
poste$ export KUBECONFIG=~/.kube/manage-our-home-<env>.yaml
poste$ kubectl get nodes
```

Ce fichier donne tous les droits sur le cluster : il se garde comme une
clé SSH. Au VPS, `<vip>` est `127.0.0.1` derrière le tunnel SSH (§ 2,
« Pare-feu »).

Les outils, aux versions et empreintes de la CI :

```sh
poste$ git clone --branch deploy https://github.com/maelprog/manage_our_home.git manage_our_home-deploy
poste$ cd manage_our_home-deploy
poste$ ci/install-tools.sh ~/.local/bin kubectl helm yq kubeseal
```

(pendant l'intégration de l'epic, `--branch k3s`).

### L'entrée HTTP : ServiceLB ou MetalLB

Traefik, l'ingress que k3s installe, a un Service `LoadBalancer` en
`externalTrafficPolicy: Local` (`README.md`, « Ingress ») : seul un nœud
qui fait tourner un pod Traefik répond, et l'adresse du client arrive
intacte.

- **Un seul nœud** (le VPS, ou un staging d'un nœud) : **ServiceLB**, celui
  de k3s, rien à faire. Les ports 80 et 443 du nœud mènent à Traefik.
- **Plusieurs nœuds** (staging) : ServiceLB ouvre 80 et 443 sur chaque
  nœud, mais seul celui de Traefik répond ; `staging.home.arpa`, qui
  résout vers une adresse fixe, viserait un nœud que kured vide un jour
  ou l'autre.
  **MetalLB** donne au Service une adresse à lui, annoncée en ARP par le
  nœud qui porte Traefik, et la déplace avec lui. ServiceLB désactivé sur
  **tous** les serveurs : soit dès l'installation (les lignes `disable`
  décommentées dans `config.yaml`), soit après, sur chaque serveur :

  ```sh
  serveur$ printf 'disable:\n  - servicelb\n' | sudo tee -a /etc/rancher/k3s/config.yaml
  serveur$ sudo systemctl restart k3s
  ```

  Les pods `svclb-traefik-*` disparaissent ; l'`EXTERNAL-IP` de Traefik
  affiche encore l'adresse du nœud, qui ne répond plus, jusqu'à ce que
  MetalLB lui en donne une.

MetalLB, chart 0.16.1, en mode L2, limites mémoire posées :

```sh
poste$ helm upgrade --install metallb metallb --repo https://metallb.github.io/metallb \
  --version 0.16.1 --namespace metallb-system --create-namespace \
  --set controller.resources.requests.memory=64Mi --set controller.resources.limits.memory=128Mi \
  --set speaker.resources.requests.memory=48Mi --set speaker.resources.limits.memory=128Mi \
  --set frrk8s.enabled=false \
  --wait
poste$ kubectl apply -f - <<'EOF'
apiVersion: metallb.io/v1beta1
kind: IPAddressPool
metadata:
  name: ingress
  namespace: metallb-system
spec:
  addresses:
    - <adresse de l'ingress>/32
---
apiVersion: metallb.io/v1beta1
kind: L2Advertisement
metadata:
  name: ingress
  namespace: metallb-system
spec:
  ipAddressPools: [ingress]
EOF
poste$ kubectl -n kube-system get svc traefik       # EXTERNAL-IP : <adresse de l'ingress>
poste$ curl -s -o /dev/null -w '%{http_code}\n' http://<adresse de l'ingress>/   # 404 de Traefik tant que l'appli n'est pas là
```

`<adresse de l'ingress>` : une adresse libre du réseau local, hors DHCP,
autre que `<vip>`. C'est elle que `staging.home.arpa` désigne (§ 4,
« Le réseau maison »).

### kured : redémarrer un nœud à la fois

kured surveille `/var/run/reboot-required` sur chaque nœud ; quand le
fichier paraît, il prend un verrou (un nœud à la fois), vide le nœud
(`kubectl drain`), le redémarre et le remet en service. Chart 6.1.0
(kured 1.23.0), fenêtre de nuit :

```sh
poste$ helm upgrade --install kured kured --repo https://kubereboot.github.io/charts \
  --version 6.1.0 --namespace kube-system \
  --set configuration.timeZone=Europe/Paris \
  --set configuration.startTime=03:30 --set configuration.endTime=05:30 \
  --set configuration.drainTimeout=10m --set configuration.forceReboot=true \
  --set resources.requests.memory=48Mi --set resources.limits.memory=96Mi \
  --wait
poste$ kubectl -n kube-system get ds kured
```

`drainTimeout` et `forceReboot` : le Postgres à une instance ne peut pas
partir. CloudNativePG pose sur son primaire un PodDisruptionBudget
`postgres-primary` (`minAvailable: 1`, aucune éviction permise) : le
vidage le réessaie sans fin. Au bout de 10 minutes, kured abandonne le
vidage et redémarre quand même ; Postgres s'arrête avec le système et
repart avec lui, sur son volume. Sans `forceReboot`, le nœud resterait
cordonné, sans redémarrer.

**Le prix sur un nœud seul** (le VPS, ou un staging d'un nœud) : rien ne
peut aller ailleurs, le vidage arrête api, web et MinIO dès son début.
Joué sur la VM de 4 Go : service coupé 13 min 22 s entre le dépôt de
`/var/run/reboot-required` et le retour des trois Applications à
`Synced Healthy` (10 min de vidage vain, puis le redémarrage). D'où la
fenêtre de nuit. Avec plusieurs nœuds, api et web repartent sur un autre
nœud ; Postgres, lui, reste là où est son volume `local-path`.

Essai, hors fenêtre de nuit : fenêtre ouverte et vérification chaque
minute (au lieu d'une heure) le temps de l'essai, puis retour aux
réglages ci-dessus :

```sh
poste$ helm upgrade kured kured --repo https://kubereboot.github.io/charts \
  --version 6.1.0 --namespace kube-system --reuse-values \
  --set configuration.startTime=00:00 --set configuration.endTime=23:59 \
  --set configuration.period=1m --wait
serveur$ sudo touch /var/run/reboot-required
poste$ kubectl get nodes -w          # SchedulingDisabled, puis NotReady, puis Ready
poste$ helm upgrade kured kured --repo https://kubereboot.github.io/charts \
  --version 6.1.0 --namespace kube-system --reuse-values \
  --set configuration.startTime=03:30 --set configuration.endTime=05:30 \
  --set configuration.period=1h --wait
```

### system-upgrade-controller : monter k3s de version

Les montées de version de k3s se font par des `Plan` que
system-upgrade-controller (v0.20.2) applique nœud par nœud : les serveurs
un à un, puis les agents. Installer le contrôleur une fois :

```sh
poste$ kubectl apply -f https://github.com/rancher/system-upgrade-controller/releases/download/v0.20.2/crd.yaml \
  -f https://github.com/rancher/system-upgrade-controller/releases/download/v0.20.2/system-upgrade-controller.yaml
poste$ kubectl -n system-upgrade set resources deploy/system-upgrade-controller \
  --requests=memory=64Mi --limits=memory=128Mi
poste$ kubectl -n system-upgrade rollout status deploy/system-upgrade-controller
```

Monter de version, c'est d'abord une PR sur cette branche qui déplace
`K3S_IMAGE` de `deploy.yml`, `KUBECTL_VERSION` de `ci/install-tools.sh` et
les schémas de `ci/validate.sh` : la CI joue la nouvelle version en k3d.
Une fois mergée, les deux Plans, à la version de `K3S_IMAGE` (étiquette
`v1.37.1-k3s1` → version `v1.37.1+k3s1`) :

```sh
poste$ kubectl apply -f - <<'EOF'
apiVersion: upgrade.cattle.io/v1
kind: Plan
metadata:
  name: k3s-server
  namespace: system-upgrade
spec:
  concurrency: 1
  cordon: true
  nodeSelector:
    matchExpressions:
      - {key: node-role.kubernetes.io/control-plane, operator: In, values: ["true"]}
  serviceAccountName: system-upgrade
  upgrade:
    image: rancher/k3s-upgrade
  version: <version>
---
apiVersion: upgrade.cattle.io/v1
kind: Plan
metadata:
  name: k3s-agent
  namespace: system-upgrade
spec:
  concurrency: 1
  cordon: true
  nodeSelector:
    matchExpressions:
      - {key: node-role.kubernetes.io/control-plane, operator: DoesNotExist}
  prepare:
    args: ["prepare", "k3s-server"]
    image: rancher/k3s-upgrade
  serviceAccountName: system-upgrade
  upgrade:
    image: rancher/k3s-upgrade
  version: <version>
EOF
poste$ kubectl -n system-upgrade get plans,jobs
poste$ kubectl get nodes -o wide -w     # VERSION passe à <version>, un nœud à la fois
```

Pendant la montée du serveur qui porte `<vip>`, l'API refuse les
connexions le temps que k3s y redémarre (kube-vip, un pod, garde
l'adresse sur ce nœud) : un `kubectl` qui échoue là se relance. Le staging d'abord, la
prod une fois le staging monté sans incident.
La prod (un nœud) est indisponible le temps que son unique nœud
redémarre k3s ; les pods, eux, ne sont pas arrêtés par la montée.

### Relevé après installation

```sh
poste$ kubectl top nodes
poste$ kubectl top pods -A --sort-by=memory
```

À noter avec la date et le commit de la branche : c'est la référence à
laquelle comparer la mémoire quand une brique change. Sur trois serveurs
et un agent nus (kube-vip, MetalLB, kured et system-upgrade-controller,
sans Argo CD ni l'appli), VM de 1,5 Go par serveur et 1 Go pour l'agent :
808 à 1 125 Mi par serveur, 592 Mi pour l'agent. Le staging complet sur
un nœud : voir « Prérequis matériels ».

## 4. Amorcer Argo CD et sceller les secrets

Sur un cluster vide, depuis le clone de la branche (§ 3, « Le poste
d'administration ») : `platform/README.md`, « Amorcer un cluster », pas à
pas. En bref :

```sh
poste$ platform/bootstrap.sh <staging|prod>
poste$ kubectl -n argocd get applications
```

La racine suit la branche de `clusters/<env>/root.yaml` : `deploy` pour la
prod ; `k3s` pour le staging pendant l'intégration, `deploy` après la
PR de promotion `k3s` → `deploy` (`platform/README.md`, « Environnements »).

**Les secrets**, une fois le contrôleur Sealed Secrets en marche : un
`SealedSecret` par Secret de `README.md`, « Secrets », scellé par
`kubeseal` avec la clé de **ce** cluster, commité dans
`platform/clusters/<env>/secrets/` par une PR. Les mots de passe des rôles
et les clés se tirent au hasard, une fois :

```sh
poste$ openssl rand -hex 24          # un mot de passe (rôles, MinIO)
poste$ openssl rand -base64 32       # une clé (OAUTH_, MESSAGE_, CALENDAR_FEED_ENCRYPTION_KEY, LOGIN_THROTTLE_KEY)
```

**Sauf pour une bascule depuis Compose** (§ 6) : les trois clés de
chiffrement et `VAPID_PRIVATE_KEY` sont alors **celles de `infra/.env`**
de la pile Compose. Des clés neuves rendraient illisibles les messages,
les jetons Google et les flux d'agenda chiffrés, et invalideraient les
abonnements aux notifications.

Exemple, le Secret de l'api (les URL nomment les mots de passe scellés
dans les Secrets des rôles, les mêmes) :

```sh
poste$ db=postgres-rw.manage-our-home.svc.cluster.local:5432/manage_our_home
poste$ kubectl -n manage-our-home create secret generic api-secrets --dry-run=client -o yaml \
  --from-literal=DATABASE_URL="postgres://app_role:<mot de passe app_role>@$db" \
  --from-literal=ADMIN_DATABASE_URL="postgres://admin_role:<mot de passe admin_role>@$db" \
  --from-literal=GOOGLE_CLIENT_ID='<…>' --from-literal=GOOGLE_CLIENT_SECRET='<…>' \
  --from-literal=OAUTH_ENCRYPTION_KEY='<…>' --from-literal=MESSAGE_ENCRYPTION_KEY='<…>' \
  --from-literal=CALENDAR_FEED_ENCRYPTION_KEY='<…>' --from-literal=LOGIN_THROTTLE_KEY='<…>' \
  --from-literal=VAPID_PRIVATE_KEY='<…>' \
  --from-literal=SMTP_USERNAME='<…>' --from-literal=SMTP_PASSWORD='<…>' \
  | kubeseal --format yaml > platform/clusters/<env>/secrets/api-secrets.yaml
```

De même `migrate-secrets`, `minio`, `postgres-app-role`,
`postgres-migration-role`, `postgres-admin-role` (type
`kubernetes.io/basic-auth`) et `postgres-backup` (`platform/README.md`,
« Sceller »), chacun ajouté à `resources:` de
`platform/clusters/<env>/secrets/kustomization.yaml`. Une fois la PR
mergée et les Secrets **ouverts par le contrôleur** (pas avant : touché
trop tôt, le Cluster reste sur son erreur), toucher le Cluster pour qu'il
relise ses Secrets, puis attendre l'archivage (`platform/README.md`,
« Amorcer un cluster », étape 2) :

```sh
poste$ kubectl -n manage-our-home wait --for=create secret/postgres-backup --timeout=10m
poste$ kubectl -n manage-our-home annotate cluster postgres manage-our-home/secrets-sealed-at="$(date -u +%FT%TZ)" --overwrite
poste$ kubectl -n manage-our-home wait cluster/postgres --for=condition=ContinuousArchiving --timeout=10m
```

**La première sauvegarde de base.** Celle que le ScheduledBackup lance à
sa création (`immediate`) part en même temps que le Cluster, avant ou
après les Secrets selon le moment. Partie avant, elle reste en
`walArchivingFailing` et bloque les suivantes (l'opérateur n'en fait
qu'une à la fois) : sans sauvegarde de base, rien n'est restaurable avant
la nuit. Les deux cas se sont produits sur deux installations
successives de la même VM. Si `get backups` en montre une en
`walArchivingFailing`, la supprimer ; puis en lancer une, et l'attendre :

```sh
poste$ kubectl -n manage-our-home get backups.postgresql.cnpg.io
poste$ kubectl -n manage-our-home delete backups.postgresql.cnpg.io <celle en walArchivingFailing>
poste$ kubectl -n manage-our-home apply -f - <<EOF
apiVersion: postgresql.cnpg.io/v1
kind: Backup
metadata: {name: postgres-first-$(date -u +%Y%m%d%H%M)}
spec: {cluster: {name: postgres}}
EOF
poste$ kubectl -n manage-our-home get backups.postgresql.cnpg.io -w   # PHASE completed
```

Puis **sauvegarder la clé du contrôleur** hors du serveur (même page,
« Sauvegarder la clé du contrôleur »).

**Avant le premier déploiement** : le bucket de sauvegarde de
l'environnement chez Scaleway, chiffrement par défaut activé avant la
première sauvegarde (`README.md`, « Sauvegardes ») ; en prod, l'adresse
publique de l'overlay et son DNS (`README.md`, « Réglages à fixer ») et
le Secret `alertmanager-config` (`platform/README.md`, « Observabilité ») ;
au staging, le réseau maison (ci-dessous).

### Le réseau maison (staging)

Le staging n'est pas exposé à Internet (arbitrage du 2026-10-17) : pas
de redirection de ports sur la box, pas de nom public. Il est servi sous
`staging.home.arpa`, un nom réservé aux réseaux domestiques (RFC 8375),
avec un certificat de l'autorité maison du cluster
(`platform/components/home-ca`) : cert-manager en crée la racine au
premier démarrage et signe le certificat de l'appli avec elle.

**Le nom.** Chaque appareil doit résoudre `staging.home.arpa` vers
l'adresse du serveur (au staging à plusieurs nœuds, `<adresse de
l'ingress>` de MetalLB) : dans le DNS local de la box si elle le permet
(une entrée d'hôte), sinon dans le fichier `hosts` de chaque appareil
(`/etc/hosts`, `C:\Windows\System32\drivers\etc\hosts`) :

```
<adresse du serveur>  staging.home.arpa
```

Un téléphone n'a pas de fichier `hosts` modifiable : sans DNS de la box,
il ne peut pas joindre le staging.

**La racine.** Une fois la plateforme synchronisée, la sortir du
cluster (elle est publique : c'est un certificat, pas une clé) :

```sh
poste$ kubectl -n cert-manager wait certificate/home-ca-root --for=condition=Ready --timeout=5m
poste$ kubectl -n cert-manager get secret home-ca-root -o jsonpath='{.data.ca\.crt}' | base64 -d > home-ca.crt
poste$ openssl x509 -in home-ca.crt -noout -subject -enddate -fingerprint -sha256
```

puis l'installer comme autorité de confiance sur chaque appareil qui
ouvre le staging :

| Appareil | Où |
|---|---|
| Debian, Ubuntu | `sudo cp home-ca.crt /usr/local/share/ca-certificates/ && sudo update-ca-certificates` |
| Windows | double-clic sur le fichier, « Installer le certificat », magasin « Autorités de certification racines de confiance » |
| macOS | Trousseau d'accès, trousseau « Système », puis « Toujours approuver » sur le certificat |
| Android | Paramètres, Sécurité, « Installer un certificat », « Certificat CA » (Chrome le reconnaît) |
| iOS | ouvrir le fichier (AirDrop, mail), installer le profil, puis Réglages, Général, Informations, « Réglages des certificats », activer la confiance totale |
| Firefox | il a son propre magasin : Paramètres, Vie privée et sécurité, Certificats, Importer, « Confirmer cette AC pour identifier des sites web » |

Vérifier depuis le poste, sans `--insecure` :

```sh
poste$ curl -sS --cacert home-ca.crt -o /dev/null -w '%{http_code}\n' https://staging.home.arpa/api/healthz   # 200
```

**Ce que l'appareil accepte avec elle** : des certificats pour des noms
en `home.arpa` seulement. La racine porte une contrainte de nom critique
(`nameConstraints`) : un certificat qu'elle signerait pour un autre nom
est refusé par le client. Lui faire confiance ne permet donc pas au
cluster de signer pour les sites que visite l'appareil. La vérifier :

```sh
poste$ openssl x509 -in home-ca.crt -noout -ext nameConstraints   # Permitted: DNS:home.arpa
```

Le service worker, la PWA et les notifications push exigent un
certificat reconnu : sur un appareil sans la racine, ils ne marchent pas,
même après avoir passé l'avertissement du navigateur.

La racine vaut 10 ans ; cert-manager la renouvelle 30 jours avant, avec
la même clé : redonner alors la racine renouvelée aux appareils. Sa clé
ne vit que dans le cluster : un cluster réinstallé en tire une nouvelle,
à réinstaller sur chaque appareil (en retirant l'ancienne). Le certificat de l'appli, lui, se renouvelle seul
tous les 60 jours, sans rien changer aux appareils.

## 5. Déployer en prod

Rien n'arrive en prod sans deux gestes de l'utilisateur (arbitrage du
2026-10-10, `README.md`, « Promotion ») :

1. **Merger la PR de promotion** vers `deploy`, celle qui déplace
   `overlays/prod` vers la paire qui tourne en staging (ouverte par le
   bot `promote.yml` de `main` sur un tag `v*`, ou à la main avec
   `ci/set-images.sh`). Ses checks sont exigés au vert par le ruleset.
   Argo CD de la prod voit alors `manage-our-home-prod` `OutOfSync`.
2. **« Sync »** dans l'Argo CD du VPS (par le tunnel :
   `kubectl -n argocd port-forward svc/argocd-server 8080:443`, puis
   `https://localhost:8080`) : `manage-our-home-prod`, et avant elle
   `platform` ou `postgres-prod` si la PR les touche. Sans le CLI d'Argo
   CD, la même demande de synchro par `kubectl` :

   ```sh
   poste$ kubectl -n argocd patch application manage-our-home-prod --type merge \
     -p '{"operation":{"initiatedBy":{"username":"<vous>"},"sync":{"revision":"deploy"}}}'
   poste$ kubectl -n argocd get application manage-our-home-prod -w   # Synced, Healthy
   ```

Le Job `migrate` passe avant le reste (hook `PreSync`) ; s'il échoue, rien
d'autre ne bouge. Revenir en arrière : `README.md`, « Revenir en
arrière ».

## 6. Basculer depuis Compose

Reprendre les données d'une pile Compose en service : la base, le bucket
des pièces jointes, et les clés de chiffrement. Le service est coupé
pendant la bascule (15 s de l'arrêt de l'api à sa reprise sur une base
d'essai d'un compte ; la durée suit le volume des pièces jointes) : la
copie se fait api arrêtée des deux côtés, pour que rien ne s'écrive
entre la copie et la reprise.

**Avant** : le cluster est installé, l'appli y tourne (`Synced`,
`Healthy`) sur une base vide, avec les clés de `infra/.env` dans
`api-secrets` (§ 4) ; le DNS vise toujours Compose. La version de l'appli
est la même des deux côtés : l'api refuse un schéma migré par une version
plus récente, et une base plus ancienne que l'image ne serait pas migrée
(le Job `migrate` a déjà tourné).

```sh
compose$ cd ~/manage_our_home/infra
compose$ docker compose exec -T postgres psql -U mhome -d manage_our_home -Atc 'SELECT max(version) FROM _sqlx_migrations'
poste$ kubectl -n manage-our-home exec postgres-1 -c postgres -- psql -d manage_our_home -Atc 'SELECT max(version) FROM _sqlx_migrations'
```

Les deux nombres doivent être égaux. Sinon, aligner d'abord la version de
Compose sur celle du cluster (`git checkout <tag>`, `docker compose up -d
--build`), ou attendre que la promotion amène la même.

**Même machine** : si Compose et le cluster tournent sur le même nœud,
Caddy tient déjà les ports 80 et 443 que ServiceLB veut pour Traefik.
Arrêter Caddy (`docker compose stop caddy`) avant d'installer k3s : le
service est coupé dès ce moment-là, et la bascule doit suivre.

### 1. Arrêter les écritures

Compose : api et web s'arrêtent, Postgres et MinIO restent pour la copie.

```sh
compose$ docker compose stop caddy web api
```

Cluster : Argo CD ne doit pas relancer l'api pendant la copie. Retirer sa
synchro automatique, la racine d'abord (elle la rétablirait sur les
autres), puis arrêter l'api — comme `README.md`, « Restaurer », étapes 1
et 2 :

```sh
poste$ for a in platform postgres-<env> manage-our-home-<env>; do
  kubectl -n argocd patch application "$a" --type merge -p '{"spec":{"syncPolicy":{"automated":null}}}'
done
poste$ kubectl -n manage-our-home scale deploy/api --replicas=0
poste$ kubectl -n manage-our-home wait --for=delete pod -l app.kubernetes.io/name=api --timeout=120s
```

(en prod, aucune n'a de synchro automatique : seule l'api s'arrête).

### 2. La base

`pg_dump` côté Compose, en format personnalisé ; `pg_restore` dans
l'instance primaire du Cluster, en superutilisateur local (le socket du
pod : aucun superutilisateur n'est joignable par le réseau). `--clean
--if-exists` remplace les objets de la base vide par ceux de Compose,
`_sqlx_migrations` compris ; `--single-transaction` annule tout à la
première erreur ; les propriétaires (`migration_role`) et les droits
viennent du dump, les rôles existant des deux côtés sous les mêmes noms.

```sh
compose$ docker compose exec -T postgres pg_dump -U mhome -Fc manage_our_home > ~/bascule.dump
poste$ scp <utilisateur>@<hôte compose>:bascule.dump .
poste$ primary=$(kubectl -n manage-our-home get clusters.postgresql.cnpg.io postgres -o jsonpath='{.status.currentPrimary}')
poste$ kubectl -n manage-our-home exec -i "$primary" -c postgres -- \
  pg_restore -d manage_our_home --clean --if-exists --single-transaction --exit-on-error < bascule.dump
```

Le dump contient toutes les données en clair : le supprimer une fois la
bascule vérifiée (`shred -u bascule.dump`, des deux côtés).

### 3. Le bucket

MinIO garde chaque objet dans un dossier du volume (`xl.meta` et ses
parties), au même chemin dans Compose et dans le cluster
(`/bitnami/minio/data`, même image). Le bucket se copie donc en `tar`,
Compose arrêté pour lui aussi :

```sh
compose$ docker compose stop minio
compose$ docker run --rm -v infra_minio_data:/data:ro debian:13 tar -C /data -cf - manage-our-home > ~/bascule-bucket.tar
poste$ scp <utilisateur>@<hôte compose>:bascule-bucket.tar .
poste$ kubectl -n manage-our-home exec -i minio-0 -- tar -C /bitnami/minio/data -xf - < bascule-bucket.tar
poste$ kubectl -n manage-our-home delete pod minio-0
poste$ kubectl -n manage-our-home rollout status statefulset/minio
```

Le nom du volume est celui de `docker volume ls | grep minio` (le nom du
projet Compose en préfixe, `infra_` par défaut). Le bucket existe déjà
dans le cluster (Job `minio-bucket`) : la copie y ajoute les objets.

### 4. Vérifier

Mêmes nombres des deux côtés :

```sh
compose$ docker compose exec -T postgres psql -U mhome -d manage_our_home -Atc \
  'SELECT (SELECT count(*) FROM users), (SELECT count(*) FROM events), (SELECT count(*) FROM event_attachments)'
poste$ kubectl -n manage-our-home exec "$primary" -c postgres -- psql -d manage_our_home -Atc \
  'SELECT (SELECT count(*) FROM users), (SELECT count(*) FROM events), (SELECT count(*) FROM event_attachments)'
poste$ tar -tf bascule-bucket.tar | grep -c '/xl.meta$'
poste$ kubectl -n manage-our-home exec minio-0 -- sh -c 'find /bitnami/minio/data/manage-our-home -name xl.meta | wc -l'
```

Le nombre d'objets est celui des pièces jointes (une ligne de
`event_attachments` par objet), à quelques objets près : ceux d'un envoi
interrompu, que la réconciliation quotidienne efface.

Puis l'api, et la synchro automatique rétablie (staging) — la racine en
dernier, réappliquée depuis la branche, rétablit celle des deux autres :

```sh
poste$ kubectl -n manage-our-home scale deploy/api --replicas=<réplicas de l'overlay>
poste$ kubectl -n manage-our-home rollout status deploy/api
poste$ kubectl apply -f platform/clusters/<env>/root.yaml
```

Se connecter avec un compte existant (par l'adresse du cluster, avant le
DNS : `curl --resolve <hôte>:443:<adresse de l'ingress>`, ou une ligne
dans le `hosts` du poste), ouvrir un événement qui a une pièce jointe et
la télécharger.

### 5. Le DNS, puis la coupure de Compose

En prod, l'enregistrement `A` de l'hôte vise maintenant le cluster ;
cert-manager obtient le certificat dès que Let's Encrypt atteint le
port 80 (`README.md`, « Ingress »). Au staging, `staging.home.arpa` vise
le serveur dans le DNS de la box ou les `hosts` (§ 4, « Le réseau
maison ») ; l'adresse qu'utilisait Compose ne sert plus.

Compose reste arrêté, **volumes gardés** : c'est le retour arrière. Après
quelques jours sans incident :

```sh
compose$ docker compose down          # jamais -v : les volumes sont le retour arrière
```

puis, une fois sûr, effacer les volumes (`docker volume rm
infra_postgres_data infra_minio_data`) et `infra/.env`, dont les clés
vivent désormais dans les Secrets scellés et dans leur sauvegarde.

### Retour arrière

**Avant la bascule du DNS**, rien n'a été écrit côté cluster : relancer
Compose, ses données sont intactes.

```sh
compose$ docker compose start
```

**Après**, des données ont pu être écrites dans le cluster : les ramener
dans Compose par le même chemin en sens inverse, api arrêtée des deux
côtés (§ 1), puis le DNS de nouveau vers Compose.

```sh
poste$ kubectl -n manage-our-home exec "$primary" -c postgres -- pg_dump -Fc -d manage_our_home > retour.dump
poste$ kubectl -n manage-our-home exec minio-0 -- tar -C /bitnami/minio/data -cf - manage-our-home > retour-bucket.tar
poste$ scp retour.dump retour-bucket.tar <utilisateur>@<hôte compose>:
compose$ docker compose up -d postgres
compose$ docker compose exec -T postgres pg_restore -U mhome -d manage_our_home --clean --if-exists --single-transaction --exit-on-error < ~/retour.dump
compose$ docker run --rm -i -v infra_minio_data:/data debian:13 sh -c 'rm -rf /data/manage-our-home && tar -C /data -xf -' < ~/retour-bucket.tar
compose$ docker compose up -d
```

## 7. Exercice de restauration

Une sauvegarde qu'on n'a jamais relue n'en est pas une
(`docs/v2-deployment.md` de `main`, item 9). La CI joue à chaque PR la
restauration à un instant (`README.md`, « Restaurer », job k3d) ; elle ne
dit rien des sauvegardes **réelles** : le bucket Scaleway, sa clé, ce qui
y est vraiment. Une fois par trimestre, et après tout changement de la
sauvegarde, sur chaque cluster :

1. **Une copie de la base, à côté** : un second Cluster, dans un
   namespace jetable, restauré depuis le bucket de l'environnement à son
   dernier état. Il **lit** le bucket sans rien y écrire : pas de
   `backup` dans sa définition, donc pas d'archivage, et le Cluster en
   service n'est pas touché.

   ```sh
   poste$ kubectl create namespace restore-drill
   poste$ kubectl -n manage-our-home get secret postgres-backup -o yaml \
     | yq e 'del(.metadata.namespace, .metadata.uid, .metadata.resourceVersion, .metadata.creationTimestamp, .metadata.ownerReferences, .metadata.labels, .metadata.annotations)' - \
     | kubectl -n restore-drill apply -f -
   poste$ kubectl -n manage-our-home get clusters.postgresql.cnpg.io postgres -o yaml \
     | yq e '{"apiVersion": .apiVersion, "kind": "Cluster",
              "metadata": {"name": "drill", "namespace": "restore-drill"},
              "spec": {"instances": 1, "imageName": .spec.imageName,
                       "storage": .spec.storage, "resources": .spec.resources,
                       "postgresql": .spec.postgresql,
                       "externalClusters": [{"name": "origin", "barmanObjectStore": .spec.backup.barmanObjectStore}],
                       "bootstrap": {"recovery": {"source": "origin", "database": "manage_our_home", "owner": "mhome"}}}}' - \
     | kubectl apply -f -
   poste$ kubectl -n restore-drill wait clusters.postgresql.cnpg.io/drill --for=condition=Ready --timeout=30m
   ```

   Le `serverName` de `externalClusters` est celui du Cluster en service
   (recopié de son `backup`) : on relit ce qu'il écrit. Il faut au moins
   une sauvegarde de base `completed` sous ce nom (§ 4, « La première
   sauvegarde de base ») : sans elle, le Job de restauration échoue en
   boucle sur `no target backup found` et le `wait` expire.
2. **Comparer** : les mêmes comptes que § 6, étape 4, sur `drill-1` et sur
   le Cluster en service, à quelques minutes d'écart près (les WAL des 5
   dernières minutes au plus). Noter la date, la durée de la restauration,
   les nombres, et la date de la sauvegarde de base relue
   (`kubectl -n restore-drill logs drill-1-full-recovery-<…>`, ou
   `kubectl -n manage-our-home get backups.postgresql.cnpg.io`).

   ```sh
   poste$ kubectl -n restore-drill exec drill-1 -c postgres -- psql -d manage_our_home -Atc \
     'SELECT (SELECT count(*) FROM users), (SELECT count(*) FROM events), (SELECT count(*) FROM event_attachments)'
   ```
3. **Le bucket des pièces jointes** n'a pas de sauvegarde
   (`docs/v2-deployment.md` de `main`, item 8, `README.md`, « MinIO ») :
   l'exercice se limite à la copie de `README.md`, « Restaurer », étape 3,
   qu'on relit — le nombre d'objets de l'étape 4 du § 6.
4. **Effacer** : `kubectl delete namespace restore-drill`. La copie
   contient toutes les données en clair ; le volume part avec le
   namespace.

Sur le serveur de test, la copie prend la mémoire d'un second Postgres
(sa requête est celle du Cluster recopié, 256 Mi) : la faire hors des
heures d'usage.

## Points ouverts

- **Chiffrement au repos des volumes** (`docs/v2-deployment.md` de
  `main`, item 20) : la procédure de cet item vise un hôte Compose (LUKS
  sous `/var/lib/docker`). Dans le cluster, les volumes de Postgres et de
  MinIO sont des dossiers `local-path` sous
  `/var/lib/rancher/k3s/storage`, et etcd (les Secrets déchiffrés par le
  contrôleur) sous `/var/lib/rancher/k3s/server/db`. Chiffrer le disque
  de `/var/lib/rancher/k3s` les couvrirait tous, mais le déverrouillage
  manuel à chaque démarrage que prévoit l'item s'oppose aux redémarrages
  automatiques de kured. Le choix (LUKS déverrouillé à la main et kured
  renoncé, clé sur un serveur de clés du réseau local, volume chiffré
  par l'hébergeur au VPS) demande un arbitrage ; il n'est pas tranché
  ici.
- **Les sauvegardes chez Scaleway** n'ont pas été jouées pour ce
  document : le refus de l'en-tête de chiffrement par requête et le
  chiffrement par défaut du bucket (`README.md`, « Sauvegardes ») restent
  à constater sur le vrai bucket, au premier déploiement.
- **kube-vip, MetalLB, kured et system-upgrade-controller** sont
  installés à la main à l'amorçage, hors d'Argo CD : leurs versions ne
  sont ni suivies par la CI ni vérifiées sur un rendu.
