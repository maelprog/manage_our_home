# Version Y — trajectoire microservices/Kubernetes (objectif formation)

Statut : décisions de direction actées le 2026-07-08 ; aucune implémentation
commencée. Ce document est le complément de `architecture.md` /
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
3. Kubernetes n'est introduit que quand il y a au moins un service réel à
   orchestrer séparément du monolithe (donc à partir de l'extraction
   d'Ollama), pas avant. Avant ça, Docker Compose reste l'outil de
   déploiement (voir `v2-deployment.md`).

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

## Ce qui a changé dans les specs existantes suite à cette discussion

- `architecture.md` : ajout d'une section "Trajectoire microservices/K8s
  (Version Y)" qui référence ce document, et clarification explicite que
  le choix "monolithe, pas de split préventif" (déjà présent) est
  maintenant assorti d'un plan de migration progressif documenté ici plutôt
  que laissé implicite.
- `v2-deployment.md` : aucune décision de v2 n'est remise en cause (VPS
  seul, Compose, pas de K8s pour le déploiement 10-15 familles) — Version Y
  est explicitement postérieure à v2, pas un remplacement.

## Stack cible Version Y (quand elle sera déclenchée)

| Couche | Choix | Statut | Rôle / justification |
|---|---|---|---|
| Conteneurisation | Docker | déjà acquis | — |
| Orchestration | Kubernetes (k3s pour démarrer) | à faire, déclenché par l'extraction d'Ollama | scaling, service discovery, self-healing ; k3s préféré à kubeadm complet pour rester léger à cette échelle |
| Ingress / passerelle | Traefik ou nginx-ingress | à faire | remplace Caddy pour le routage entrant en mode K8s-natif ; TLS termination |
| Message broker | Kafka via l'opérateur **Strimzi** (pas Kafka nu) | à faire, cas d'usage minimal d'abord | event-driven pour un cas réel et borné : event "fridge photo uploaded" consommé de façon async par le service vision. Explicitement **pas** un remplacement des appels HTTP synchrones existants (anti-pattern écarté) |
| Service mesh | Linkerd (préféré à Istio, plus léger) | à faire, en dernier | mTLS inter-services (répond à l'exigence RGPD de TLS interne déjà notée dans `architecture.md`), observabilité de trafic, retries/circuit breakers |
| Config/secrets | K8s Secrets + External Secrets Operator, relié à sops/age déjà utilisé | à faire | gestion de secrets à l'échelle cluster |
| Métriques | Prometheus + Grafana | à faire | observabilité du cluster — absent aujourd'hui, condition pour "voir" ce que fait K8s |
| Tracing distribué | OpenTelemetry + Jaeger ou Tempo | à faire | indispensable dès que plusieurs services (HTTP + Kafka) se parlent |
| Logs centralisés | Loki (léger, s'intègre à Grafana) — alternative EFK si besoin de plus de puissance de recherche | à faire | répond aussi à l'item #13 déjà identifié comme manquant dans `v2-deployment.md` |
| CI/CD vers le cluster | ArgoCD ou Flux (GitOps) | à faire | pattern standard entreprise : déploiement déclaratif, pas de `kubectl apply` manuel |
| Registry d'images | GitHub Container Registry (gratuit) ou Harbor self-hosted | à faire | prérequis pour que K8s puisse tirer les images buildées |
| Postgres sur K8s | Opérateur CloudNativePG ou Zalando postgres-operator (à évaluer, voir questions ouvertes) | à faire, si décidé | pattern entreprise pour stateful workloads : backups automatisés, failover |
| MinIO sur K8s | Opérateur MinIO officiel | à faire, si décidé | idem, stateful sur K8s |

### Séquencement recommandé (pour apprendre sans se noyer)

1. k3s + Docker + Ingress — migrer Ollama comme premier (et seul, au début)
   service K8s, sans Kafka ni mesh.
2. Prometheus/Grafana — observer ce qui existe avant d'ajouter de la
   complexité.
3. Kafka (via Strimzi), pour le cas d'usage minimal ci-dessus uniquement.
4. ArgoCD pour boucler en GitOps.
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
3. **Stateless par requête** : déjà le cas (pas d'état en mémoire du
   process hors connexion DB) — condition nécessaire pour que K8s puisse
   scaler des pods sans souci de session affinity.
4. **Config par variables d'environnement**, jamais de chemin/fichier local
   en dur — déjà la pratique (sops + env vars).

Aucune de ces règles ne change le code ou les choix déjà faits ; elles sont
listées ici comme garde-fous à vérifier à chaque nouvel epic.

## Questions ouvertes / décisions à prendre plus tard

Ces points ne sont **pas** bloquants pour continuer le développement v1/v2
actuel — ils devront être tranchés au moment de déclencher réellement la
Version Y (extraction d'Ollama), pas avant.

1. **Cluster K8s : self-hosted (k3s sur le VPS existant) ou managé
   (Scaleway Kapsule, OVH Managed Kubernetes, ~20-50€/mois) ?** Impact
   coût vs. impact pédagogique (gérer soi-même le control plane est plus
   formateur mais plus de charge opérationnelle). Non tranché.
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
   d'une architecture hybride moins "pure". Non tranché.
5. **Mesh : Linkerd est proposé (plus léger qu'Istio) mais pas comparé en
   détail.** À valider une fois qu'il y a au moins 2-3 services réels à
   mesher — prématuré de trancher avant.
6. **Registry : GitHub Container Registry (gratuit, simple) vs. Harbor
   self-hosted (plus formateur, plus de maintenance).** Non tranché,
   dépend de l'appétit à maintenir un service de plus.
7. **Déclencheur précis de la migration Ollama** : "quand l'epic
   fridge-scan est spec'é et implémenté" est le critère qualitatif retenu,
   mais aucune date/seuil quantitatif n'a été fixé. À clarifier si un
   objectif de calendrier de formation existe (ex. "je veux avoir touché à
   K8s d'ici telle date" indépendamment de l'avancement produit).

## Relation avec les autres docs

- `architecture.md` : stack et décisions v1, référence ce document pour la
  trajectoire long-terme.
- `v2-deployment.md` : déploiement 10-15 familles, antérieur et indépendant
  de cette trajectoire — non impacté.
- `v1-scope.md` : suivi des epics fonctionnels ; l'epic fridge-scan
  (déclencheur de la Version Y) y est listé "out of v1", pas encore spec'é.
