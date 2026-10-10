# Version Y — trajectoire microservices/Kubernetes (objectif formation)

Statut : décisions de direction actées le 2026-07-08, révisées le 2026-10-10
(arbitrages utilisateur, epic #441 « déploiement continu GitOps sur k3s ») :
Kubernetes n'attend plus l'extraction d'Ollama, et les questions ouvertes 1, 4
et 6 sont tranchées. Ce document est le complément de `architecture.md` /
`v2-deployment.md` pour tout ce qui concerne la trajectoire long-terme vers
une architecture microservices orchestrée par Kubernetes.

**Objectif explicite de cette trajectoire : se former à une architecture
d'entreprise (microservices + Kubernetes + event-driven), pas répondre à un
besoin de charge réel.** À l'échelle produit actuelle (v1 local, v2 à
10-15 familles), rien ne justifierait ce virage sur des critères de charge
seuls — voir la section "Pourquoi pas maintenant" ci-dessous. La décision est
assumée comme pédagogique et documentée comme telle pour ne pas être
confondue plus tard avec une nécessité produit.

## Décision de fond : monolith-first, migration progressive

**Décidé (2026-07-08) :** on ne bascule pas vers les microservices d'un
bloc. La stratégie retenue est "monolith first" (Martin Fowler) :

1. Le monolithe modulaire actuel (`apps/api`, un crate Axum, modules Auth/
   Agenda/Stocks/Recipes/Grocery list/Budget) reste la plateforme de
   référence pour v1 et v2 (déploiement 10-15 familles).
2. Chaque extraction de service se fait **une par une, sur justification
   concrète**, pas en bloc préventif. Le premier et pour l'instant seul
   candidat identifié : **Ollama** (fridge-scan/vision, futur epic),
   parce que c'est le seul composant avec une contrainte matérielle
   différente du reste (GPU-bound, latence en secondes) — voir
   `architecture.md` § stack.
3. **Révisé le 2026-10-10 (epic #441) :** Kubernetes est retenu dès
   maintenant, pour déployer le monolithe lui-même, sans attendre
   l'extraction d'Ollama. La décision du 2026-07-08 (Kubernetes seulement à
   partir de cette extraction, Docker Compose pour v1/v2) est levée par
   l'utilisateur. Le déploiement se fera sur **deux clusters k3s
   auto-gérés** : le serveur maison pour le staging (et l'usage familial non
   public), un VPS tiers pour la prod. Docker Compose
   (`infra/docker-compose.yml`) reste la pile de développement local.
   L'extraction de services, elle, reste soumise au point 2 : déployer le
   monolithe sur Kubernetes n'est pas le découper.

### Pourquoi pas maintenant (critères de charge)

Rappel des seuils discutés (détail dans la conversation source, pas
reproduit en entier ici) :

- Microservices se justifient par l'**organisation** (plusieurs équipes,
  cycles de release indépendants — seuil typique ~8-10+ ingénieurs) ou par
  un **composant à profil de charge radicalement différent** du reste. Un
  seul mainteneur, un seul composant hors norme (Ollama) : ça ne justifie
  pas un découpage complet, seulement ce composant-là.
- Kubernetes a un **coût fixe d'exploitation** (control plane, RBAC,
  ingress, secrets, observabilité) indépendant du nombre de services —
  il ne se rentabilise que quand Docker Compose devient concrètement
  ingérable (dizaines de services, plusieurs environnements). À l'échelle
  de ce projet, ce coût fixe dépasse largement le bénéfice tant qu'on
  n'a qu'un ou deux services à orchestrer.

Ces deux points restent vrais indépendamment de l'objectif de formation —
la formation justifie de payer ce coût volontairement, elle ne l'annule pas.
C'est ce que fait l'arbitrage du 2026-10-10 pour Kubernetes : son coût fixe
est payé dès maintenant, sans critère de charge ; le découpage en
microservices, lui, reste conditionné comme ci-dessus.

## Ce qui a changé dans les specs existantes suite à cette discussion

- `architecture.md` : ajout d'une section "Trajectoire microservices/K8s
  (Version Y)" qui référence ce document, et clarification explicite que
  le choix "monolithe, pas de split préventif" (déjà présent) est
  maintenant assorti d'un plan de migration progressif documenté ici plutôt
  que laissé implicite.
- `v2-deployment.md` (2026-07-08) : aucune décision de v2 n'était remise en
  cause (VPS seul, Compose, pas de K8s pour le déploiement 10-15 familles).
  **Révisé le 2026-10-10 (epic #441)** : le déploiement v2 se fera sur k3s
  (prod sur un VPS tiers, staging sur le serveur maison) ; les items 1, 11,
  12, 13 et 15 de `v2-deployment.md` sont requalifiés en conséquence.

## Stack cible Version Y (Kubernetes et sa chaîne de livraison décidés le 2026-10-10, le reste à son déclenchement)

| Couche | Choix | Statut | Rôle / justification |
|---|---|---|---|
| Conteneurisation | Docker | déjà acquis | — |
| Orchestration | Kubernetes (k3s auto-géré) | à faire, décidé le 2026-10-10 (#441, installation #438) | deux clusters : serveur maison (staging, multi-nœuds prévu, etcd embarqué dès le premier nœud), VPS tiers (prod) ; scaling, service discovery, self-healing ; k3s préféré à kubeadm complet pour rester léger à cette échelle |
| Ingress / passerelle | Traefik (fourni par k3s) + cert-manager | à faire (#434) | remplace Caddy pour le routage entrant en mode K8s-natif ; TLS termination |
| Message broker | Kafka via l'opérateur **Strimzi** (pas Kafka nu) | à faire, cas d'usage minimal d'abord | event-driven pour un cas réel et borné : event "fridge photo uploaded" consommé de façon async par le service vision. Explicitement **pas** un remplacement des appels HTTP synchrones existants (anti-pattern écarté) |
| Service mesh | Linkerd (préféré à Istio, plus léger) | à faire, en dernier | mTLS inter-services (répond à l'exigence RGPD de TLS interne déjà notée dans `architecture.md`), observabilité de trafic, retries/circuit breakers |
| Config/secrets | Sealed Secrets (arbitrage du 2026-10-10, à la place d'External Secrets Operator relié à sops/age) | à faire (#432) | secrets scellés versionnés sur la branche `deploy`, que seul le contrôleur du cluster ouvre |
| Métriques | Prometheus + Grafana (kube-prometheus-stack) | à faire (#437) | observabilité du cluster — absent aujourd'hui, condition pour "voir" ce que fait K8s |
| Tracing distribué | OpenTelemetry + Jaeger ou Tempo | à faire | indispensable dès que plusieurs services (HTTP + Kafka) se parlent |
| Logs centralisés | Loki (léger, s'intègre à Grafana) — alternative EFK si besoin de plus de puissance de recherche | à faire (#437) | répond aussi à l'item #13 déjà identifié comme manquant dans `v2-deployment.md` |
| CI/CD vers le cluster | Argo CD (GitOps, arbitrage du 2026-10-10) | à faire (#431, #432, #436) | pattern standard entreprise : déploiement déclaratif, pas de `kubectl apply` manuel. Manifests sur une branche `deploy` du monorepo, un dossier par environnement ; un Argo CD par cluster ; staging suit `main`, prod suit les tags, **sans synchro automatique** ; tout changement sur `deploy` passe par une PR mergée à la main par l'utilisateur |
| Registry d'images | GitHub Container Registry (tranché le 2026-10-10, question 6) | à faire (#430) | images scannées, signées, avec SBOM ; prérequis pour que K8s puisse tirer les images buildées |
| Postgres sur K8s | Opérateur CloudNativePG (tranché le 2026-10-10, question 4) | à faire (#433) | pattern entreprise pour stateful workloads : backups automatisés, failover |
| MinIO sur K8s | MinIO en StatefulSet dans le cluster (tranché le 2026-10-10, question 4) | à faire (#435) | idem, stateful sur K8s |

### Séquencement recommandé (pour apprendre sans se noyer)

Révisé le 2026-10-10 (epic #441). Le séquencement du 2026-07-08 commençait
par migrer Ollama comme premier service K8s et plaçait ArgoCD en quatrième
étape ; c'est désormais le monolithe qui ouvre la marche, en GitOps dès le
départ :

1. Appli prête pour plusieurs réplicas (#424 à #429) : sondes de santé,
   migrations hors du serveur, état partagé entre réplicas (voir la règle
   « stateless » ci-dessous).
2. Chaîne de livraison : images GHCR signées (#430), branche `deploy` (#431),
   plateforme Argo CD, Sealed Secrets, cert-manager, CloudNativePG et Kyverno
   (#432), Postgres (#433), ingress (#434), MinIO et Ollama dans le cluster
   (#435), promotion GitOps (#436).
3. Prometheus/Grafana/Loki (#437) — observer ce qui existe avant d'ajouter
   de la complexité.
4. Kafka (via Strimzi), pour le cas d'usage minimal ci-dessus uniquement.
5. Linkerd/mTLS en dernier — couche la plus subtile à débugger sans
   l'intuition acquise sur le reste.

## Discipline à respecter dès maintenant dans le monolithe (coût quasi nul, payé aujourd'hui)

Pour que l'extraction de futurs services reste peu coûteuse le moment venu,
sans rien changer à l'architecture v1/v2 actuelle :

1. **Pas d'accès SQL cross-module** : un module (ex. Recipes) ne doit pas
   `JOIN` directement les tables d'un autre module (ex. Stocks) dans la
   même transaction. Passer par l'API/fonctions du module concerné. Déjà
   globalement respecté (voir le pattern `missing_ingredients` structuré
   émis par Recipes plutôt qu'un JOIN direct dans Grocery list) — à
   surveiller pour les futurs epics.
2. **Frontières de module nettes** : continuer le découpage actuel
   (`src/agenda/`, `src/stocks/`, etc.) avec des points d'entrée explicites,
   pas de couplage caché par accès direct aux structs internes d'un autre
   module.
3. **Stateless par requête** : condition nécessaire pour que K8s puisse
   scaler des pods sans souci de session affinity. **Pas encore le cas**
   (constat du 2026-10-10, qui corrige le « déjà le cas » du 2026-07-08) :
   le process garde de l'état en mémoire, ou lance des tâches qui supposent
   qu'il est seul. #426 à #429 y remédient :
   - #426 : les tâches de fond tournent dans chaque process (une
     notification partirait une fois par réplica) ;
   - #427 : le frein anti-force-brute du login compte en mémoire ;
   - #428 : les limites de débit de l'import de recettes, des appels à Open
     Food Facts et des rapports CSP comptent en mémoire ;
   - #429 : le hub WebSocket de la messagerie est local au process.
4. **Config par variables d'environnement**, jamais de chemin/fichier local
   en dur — déjà la pratique (env vars, secrets chiffrés par sops sous
   Compose ; sur les clusters k3s, Sealed Secrets décidé le 2026-10-10,
   #432).

Ces règles sont listées ici comme garde-fous à vérifier à chaque nouvel
epic ; la règle 3 demande en plus le code de #426 à #429.

## Questions ouvertes / décisions à prendre plus tard

Les questions 1, 4 et 6 sont tranchées par les arbitrages du 2026-10-10
(epic #441). Les autres ne bloquent pas le déploiement sur k3s : elles
portent sur l'extraction d'Ollama, Kafka et le mesh, et seront tranchées au
moment de déclencher cette extraction, pas avant.

1. **Cluster K8s : self-hosted (k3s sur le VPS existant) ou managé
   (Scaleway Kapsule, OVH Managed Kubernetes, ~20-50€/mois) ?** Impact
   coût vs. impact pédagogique (gérer soi-même le control plane est plus
   formateur mais plus de charge opérationnelle). **Tranché le 2026-10-10 :
   k3s auto-géré**, sur deux clusters — le serveur maison pour le staging
   (multi-nœuds prévu, etcd embarqué dès le premier nœud), un VPS tiers
   pour la prod (l'hébergeur tiers préserve l'anonymat LCEN de l'éditeur,
   #379 ; conséquences RGPD/LCEN : #440).
2. **GPU pour Ollama : cloud GPU dédié (~80-250€/mois) vs. second VPS
   CPU-only dédié (~15-25€/mois, lent) vs. GPU à la demande/scale-to-zero ?**
   Dépend du volume réel d'usage du futur epic fridge-scan, qui n'existe pas
   encore. Non tranché — à revisiter quand l'epic sera spec'é.
3. **Kafka : cas d'usage minimal exact à choisir.** "Event fridge photo
   uploaded → service vision" est la proposition de départ, mais aucun epic
   fridge-scan n'est encore spec'é (`v1-scope.md` le liste "out of v1"). Le
   cas d'usage Kafka dépend donc du spec de cet epic, pas encore fait.
4. **Postgres/MinIO sur K8s : opérateur dès le début de la Version Y, ou
   garder ces deux stateful services en dehors du cluster (VM/Compose
   classique) et ne mettre que les services stateless (Ollama, futurs
   services) sous K8s ?** Les deux sont défendables ; la deuxième option
   réduit le risque (stateful sur K8s est réputé plus délicat) au prix
   d'une architecture hybride moins "pure". **Tranché le 2026-10-10 :
   dans le cluster** — Postgres sous l'opérateur CloudNativePG (#433), MinIO
   en StatefulSet (#435).
5. **Mesh : Linkerd est proposé (plus léger qu'Istio) mais pas comparé en
   détail.** À valider une fois qu'il y a au moins 2-3 services réels à
   mesher — prématuré de trancher avant.
6. **Registry : GitHub Container Registry (gratuit, simple) vs. Harbor
   self-hosted (plus formateur, plus de maintenance).** Dépendait de
   l'appétit à maintenir un service de plus. **Tranché le 2026-10-10 :
   GitHub Container Registry** (#430).
7. **Déclencheur précis de l'extraction d'Ollama** en service séparé :
   "quand l'epic fridge-scan est spec'é et implémenté" est le critère
   qualitatif retenu, mais aucune date/seuil quantitatif n'a été fixé. Ce
   critère ne déclenche plus Kubernetes (arbitrage du 2026-10-10) : Ollama
   est prévu dans le cluster à côté du monolithe (#435), seule son
   extraction en service propre reste conditionnée. À clarifier si un
   objectif de calendrier de formation existe (ex. "je veux avoir touché à
   K8s d'ici telle date" indépendamment de l'avancement produit).

## Relation avec les autres docs

- `architecture.md` : stack et décisions v1, référence ce document pour la
  trajectoire long-terme.
- `v2-deployment.md` : déploiement 10-15 familles. Indépendant de cette
  trajectoire jusqu'au 2026-10-10 ; depuis, il est prévu sur k3s (epic #441)
  et ses items 1, 11, 12, 13 et 15 renvoient ici.
- `v1-scope.md` : suivi des epics fonctionnels ; l'epic fridge-scan
  (déclencheur de l'extraction d'Ollama) y est listé "out of v1", pas
  encore spec'é.
