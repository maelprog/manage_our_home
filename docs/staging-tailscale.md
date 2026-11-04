# Accès au serveur de test par Tailscale

Le serveur de test (`mhome-serve`, staging) n'est pas joignable depuis
Internet : la box n'ouvre aucun port (#440, #443), et la connexion de la
maison peut être derrière un CGNAT. On y accède par Tailscale, un VPN dont
chaque appareil n'établit que des connexions **sortantes** : il traverse
le NAT sans redirection de port, en direct (WireGuard) quand c'est
possible, sinon par un relais de Tailscale qui ne voit que du trafic
chiffré de bout en bout (#684).

C'est un palier : le coordinateur de Tailscale est un service hébergé par
Tailscale Inc., hors de l'Union européenne. Headscale, coordinateur
auto-hébergé sur le VPS de production (#443), le remplacera, et avec lui
le sous-traitant (`docs/registre-traitements.md`).

Conventions : `poste$` sur le poste d'administration, `serveur$` sur
`mhome-serve`, `<ip-tailnet>` l'adresse `100.x.y.z` du serveur dans le
tailnet (`tailscale ip -4`), `<lan>` l'adresse du serveur sur le réseau de
la maison. Les intitulés des consoles changent : en cas d'écart, suivre la
console.

## Arbitrages (2026-11-04)

- Fournisseur d'identité du tailnet : **Google**. Chaque personne se
  connecte à Tailscale avec son propre compte Google.
- Transfert hors UE du coordinateur : **accepté pour le staging**, jusqu'à
  Headscale (#443). Sa base est au registre.
- Expiration des clés des appareils de la famille : **90 jours**.

## Ce qui est joignable, et par qui

La politique d'accès du tailnet est versionnée dans
`infra/tailscale/policy.json`. Tout est refusé sauf ce qu'elle accepte :

| Groupe | Joint sur le serveur (`tag:serveur`) | Pour |
|---|---|---|
| `group:famille` | 443 | l'appli en HTTPS |
| `group:admin` | 22, 443, 6443 | SSH, l'appli, l'API Kubernetes |

- **Argo CD** n'a pas de port à lui : son interface ne tourne qu'à la
  demande et ne s'atteint que par `kubectl port-forward`
  (`platform/README.md` de la branche de déploiement), donc par l'API
  Kubernetes, donc par le seul groupe `admin`.
- **Rien d'autre** : aucune règle ne vise une adresse du réseau de la
  maison, ni un appareil de la famille depuis un autre. Le fichier ne
  contient ni `ssh` (pas de Tailscale SSH : sshd reste celui du système),
  ni `autoApprovers` (aucun *subnet router* ni *exit node* ne s'approuve
  seul), ni `grants`, ni `nodeAttrs`.
- **Les groupes sont vides dans le dépôt**, qui est public : leurs membres
  (des adresses Gmail) viennent du secret `TS_ACL_GROUPS` au moment de
  valider ou d'appliquer, au format
  `{"group:famille": ["…"], "group:admin": ["…"]}`, exactement ces deux
  clés.
- Le bloc `tests` du fichier décrit ce tableau. Le coordinateur refuse une
  politique dont un test échoue ; il les exécute quand le workflow valide
  la politique de `main` (voir « Qui applique la politique »).
- **La politique est épinglée.** La crate `infra/tailscale` (test de la
  CI, job `test`, sur chaque pull request) n'interprète pas le langage de
  politique : elle compare `policy.json`, clé par clé, à la politique
  écrite en entier dans son test (`PINNED`, `infra/tailscale/src/lib.rs`),
  et échoue sur toute clé ajoutée (`ssh`, `grants`, `nodeAttrs`,
  `autoApprovers`, ou n'importe quelle autre), retirée ou modifiée,
  `tagOwners` et `tests` compris. L'ordre des clés et la mise en page ne
  comptent pas. Elle vérifie aussi qu'aucun `@` n'est écrit dans le
  fichier. **Modifier l'accès, c'est donc modifier aussi `PINNED`** dans
  la même pull request, et c'est cette modification-là qui se relit à la
  main : rien ne vérifie qu'un nouvel épinglage correspond au tableau
  ci-dessus.

**Rôle dans le tailnet et groupe sont indépendants.** Le rôle (*Owner*,
*Admin*, *Member*) dit qui administre le tailnet ; le groupe dit ce qu'un
appareil peut joindre. Un compte *Member* peut figurer dans `group:admin` :
c'est le cas du poste de développement, connecté au tailnet sous un compte
*Member*. Il joint alors l'API Kubernetes, mais ne peut ni modifier la
politique, ni approuver un appareil, ni créer une clé : seul le compte
administrateur le peut, ou un identifiant d'API créé par lui.

## Qui applique la politique

Le workflow `.github/workflows/tailscale-policy.yml` :

- **valide** la politique de `main` auprès du coordinateur, tests compris,
  après chaque modification de `infra/tailscale/` fusionnée (ou à la main,
  *Run workflow* sans cocher `apply`). Aucune pull request ne lance ce
  workflow : son job exécuterait le `policy.sh` de la branche, qui
  pourrait envoyer l'identifiant n'importe où. Une pull request n'a que la
  vérification hors ligne (l'épinglage) du job `test` de la CI. Sans les secrets, le job
  passe avec un avertissement ;
- **applique** la politique de `main` au tailnet à la main seulement :
  onglet Actions → *Tailscale policy* → *Run workflow*, case `apply` cochée.
  Comme un changement de la branche `deploy`, toucher à l'infrastructure
  reste un geste de l'utilisateur. L'application remplace la politique du
  tailnet : une modification faite dans la console est écrasée à la
  suivante. Elle échoue si la politique a changé entre sa lecture et son
  remplacement (`If-Match`).

Hors CI, le même script, depuis un poste qui a `jq` et `curl` :

```
poste$ read -rs TS_API_KEY && export TS_API_KEY
poste$ read -r TS_ACL_GROUPS && export TS_ACL_GROUPS
poste$ infra/tailscale/policy.sh validate
poste$ infra/tailscale/policy.sh apply
poste$ unset TS_API_KEY TS_ACL_GROUPS
```

**Où ranger les secrets : l'environnement `tailscale`, pas le dépôt.**
Settings → *Environments* → *New environment* `tailscale` →
*Deployment branches and tags* : *Selected branches and tags*, la seule
branche `main`. Puis, dans cet environnement, les secrets
`TS_ACL_GROUPS` et l'identifiant ci-dessous. C'est cette règle qui fait
la frontière : un job lancé depuis une autre référence (une branche, une
pull request), même par un workflow modifié pour réclamer
l'environnement, n'en reçoit aucun secret. Un secret rangé au niveau du
dépôt, lui, serait lisible par tout workflow d'une branche du dépôt :
n'en ranger aucun là. GitHub crée l'environnement sans règle si le
workflow tourne avant ce réglage ; il ne contient alors aucun secret, et
le job passe avec un avertissement.

**L'identifiant.** Deux possibilités, rangées dans l'environnement
`tailscale`, jamais dans un fichier ni une issue :

- **Client OAuth (recommandé)** : `TS_OAUTH_CLIENT_ID` et
  `TS_OAUTH_CLIENT_SECRET`. Console → *Settings* → *Trust credentials* →
  client OAuth, portée `policy_file` (lecture, validation et écriture de la
  politique ; la documentation de Tailscale, consultée le 2026-11-04, lui
  associe `devices:core:read` et `devices:posture_attributes`). Le secret
  n'expire pas : il se révoque dans la console. Chaque exécution en tire un
  jeton valable une heure.
- **Clé API** : `TS_API_KEY`, créée par le compte administrateur. Elle
  expire (90 jours au plus) : la renouveler avant son terme (console →
  *Settings* → *Keys* → *Generate access token*, puis remplacer le secret),
  et noter la date d'expiration là où l'on note les autres échéances. Une
  clé expirée fait échouer le job `validate` (HTTP 401), pas en silence.

Si les deux sont présents, le client OAuth est utilisé.

## Réglages du tailnet (console, une fois)

1. **Device management → Key expiry : 90 jours.** Au terme, l'appareil
   perd l'accès jusqu'à ce que son titulaire se reconnecte avec son compte
   Google.
2. **Device management → Device approval** activé : un nouvel appareil
   reste en attente jusqu'à l'approbation de l'administrateur.
3. **Access controls** : la politique de `main`, appliquée par le workflow
   ci-dessus avant d'inscrire le premier appareil de la famille (la
   politique par défaut d'un tailnet accepte tout).
4. Ne pas activer **Funnel**, le **partage de machines** avec un autre
   tailnet, ni de **subnet router** ou d'**exit node** : chacun ouvrirait
   un chemin que la politique ne décrit pas.
5. Le compte Google administrateur porte une **double authentification** :
   le compromettre, c'est compromettre le tailnet.

## Installer le serveur

Installer le paquet Tailscale officiel (dépôt APT de Tailscale, ou script
`tailscale.com/install.sh` lu avant d'être lancé). Puis, dans la console :
*Settings* → *Keys* → *Generate auth key* : **à usage unique** (*Reusable*
décoché), non éphémère, tag `tag:serveur`, expiration d'un jour.

```
serveur$ read -rs TS_AUTHKEY
serveur$ sudo tailscale up --auth-key="$TS_AUTHKEY" --hostname=mhome-serve
serveur$ unset TS_AUTHKEY
serveur$ tailscale ip -4                     # <ip-tailnet>
```

- La clé ne sert qu'une fois : fuitée après usage, elle n'ouvre rien.
- Un appareil inscrit avec un tag n'expire pas par défaut : c'est voulu
  pour le serveur, qui sortirait sinon du tailnet tous les 90 jours.
- Pas de `--ssh`, pas de `--advertise-routes`, pas d'`--advertise-exit-node`.

**L'appli (443).** Au staging (un nœud), ServiceLB publie les ports 80 et
443 de Traefik sur toutes les interfaces du nœud
(`docs/install-k3s.md` de la branche de déploiement) : `<ip-tailnet>:443`
répond sans autre réglage.

**L'API Kubernetes (6443) et Argo CD, sur l'interface Tailscale seule.**
k3s écoute sur toutes les interfaces, et l'y lier à `<ip-tailnet>` ferait
dépendre son démarrage de celui de Tailscale. C'est donc le pare-feu qui
ferme 6443 partout ailleurs. Le certificat de l'API doit aussi nommer
l'adresse du tailnet :

```
serveur$ sudo mkdir -p /etc/rancher/k3s/config.yaml.d
serveur$ printf 'tls-san+:\n  - <ip-tailnet>\n' | sudo tee /etc/rancher/k3s/config.yaml.d/tailscale.yaml
serveur$ sudo systemctl restart k3s
serveur$ sudo ufw allow in on tailscale0 to any port 6443 proto tcp
serveur$ sudo ufw status numbered            # repérer « 6443/tcp ALLOW <lan> »
serveur$ sudo ufw delete <numéro>
poste$   kubectl config set-cluster <cluster> --server=https://<ip-tailnet>:6443
poste$   kubectl get nodes
```

k3s lit `/etc/rancher/k3s/config.yaml.d/*.yaml` après `config.yaml` ; le
`+` de `tls-san+` ajoute l'adresse à la liste du fichier principal au lieu
de la remplacer. Sur l'interface `tailscale0`, c'est la politique du
tailnet qui filtre (tailscaled écarte ce qu'elle refuse avant de le
remettre au système) ; la règle ufw ne fait que l'admettre. Les pods joignent
l'API par le réseau des pods, que la règle `10.42.0.0/16` d'ufw laisse
passer. Le jour où le staging aura plusieurs nœuds, ils joindront l'API par
le LAN (`<vip>` de kube-vip) : rouvrir 6443 à leurs seules adresses, pas à
tout le réseau.

**SSH (22).** La politique le réserve au groupe `admin` par le tailnet.
La règle `22/tcp` d'ufw reste ouverte sur le LAN : c'est la voie de secours
si Tailscale tombe, et le LAN n'est pas joignable de l'extérieur.

## Le nom du staging

La zone publique fait résoudre le nom du staging vers `<ip-tailnet>`
(enregistrement de #683) : l'adresse n'est joignable que depuis le tailnet,
et la publier n'ouvre rien. À la maison, deux cas : le VPN reste allumé et
Tailscale prend le chemin direct sur le LAN ; ou une entrée locale du DNS
de la box (si elle le permet, #683) donne `<lan>` et l'appli répond sans
VPN. Certains résolveurs ou box filtrent les réponses qui pointent vers une
adresse privée ou partagée (protection contre le *DNS rebinding*) : si le
nom ne résout pas, essayer un autre réseau avant de chercher ailleurs.

## Ajouter un appareil

1. La personne installe l'application **Tailscale officielle** (App Store,
   Play Store, `tailscale.com/download`).
2. L'administrateur l'invite (console → *Users* → *Invite users*) ; elle
   se connecte avec **son** compte Google, jamais un compte partagé : la
   révocation se fait par personne.
3. L'administrateur approuve l'appareil (*Machines*).
4. Son adresse entre dans `group:famille` (ou `group:admin`) du secret
   `TS_ACL_GROUPS`, puis la politique est réappliquée (workflow, `apply`).
   Tant qu'elle n'y est pas, l'appareil ne joint rien.

**VPN « à la demande, pour le seul domaine ».** Ni iOS ni Android ne
déclenchent le VPN de Tailscale pour un nom de domaine arbitraire : iOS
(*VPN On Demand*) ne reconnaît que les noms en `*.ts.net`, et seulement si
la règle de l'interface est *Do Nothing* ; Android n'a que le VPN
permanent (*Always-on VPN*). Le même effet s'obtient autrement : Tailscale
ne fait passer dans le tunnel que les adresses du tailnet
(`100.64.0.0/10`) ; le reste du trafic sort normalement. On laisse donc le
VPN allumé en permanence (iOS : *VPN On Demand* avec la règle *Always* ;
Android : *Always-on VPN* dans les réglages du système), sans *exit node*.

## Révoquer un appareil

1. Console → *Machines* → l'appareil → **Remove** : l'appareil sort du
   tailnet aussitôt.
2. Si c'est la personne qui part (ou si son compte Google est compromis),
   retirer aussi son adresse du groupe dans `TS_ACL_GROUPS`, réappliquer,
   et la retirer du tailnet (*Users*) : sinon elle peut réinscrire un autre
   appareil.
3. Sans rien de tout cela, un appareil perd l'accès au plus tard à
   l'**expiration de sa clé, 90 jours** après sa dernière authentification :
   c'est la fenêtre maximale d'exposition d'un appareil perdu dont personne
   n'a signalé la perte.

## Vérifier

```
dehors$  nmap -Pn -p 22,80,443,6443 <IP publique de la box>   # tout filtered, sans VPN
famille$ curl -sS -o /dev/null -w '%{http_code}\n' https://<nom du staging>/   # 200 ou 3xx, certificat valide
famille$ curl -sk --max-time 5 https://<ip-tailnet>:6443/livez   # échoue : délai dépassé
admin$   kubectl get nodes                                       # répond
```

`dehors$` est un appareil hors de la maison et hors du tailnet (un
téléphone en données mobiles, VPN coupé, par exemple avec une application
de scan de ports). Un appareil retiré refait le test `famille$` : il doit
échouer.

## Données personnelles

Le coordinateur reçoit, pour chaque appareil : le compte Google qui l'a
inscrit, son nom, son système, son adresse IP publique, sa clé publique et
les heures de connexion ; jamais le contenu du trafic. Tailscale Inc. est
déclaré comme sous-traitant au registre, avec le transfert hors UE et sa
base, et la durée de conservation (`docs/registre-traitements.md`).
