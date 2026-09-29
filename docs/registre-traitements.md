# Registre des traitements — Manage Our Home

Registre tenu au titre de l'article 30 du RGPD. Dernière mise à jour :
2026-09-28, relu contre les migrations `apps/api/migrations/0001` à `0016`
et la table des routes de `apps/api/src/lib.rs`. Un registre des
traitements est requis dès qu'un traitement de données personnelles est
effectué, y compris à petite échelle — voir `docs/architecture.md` §10.

**Responsable de traitement** : le porteur du projet, personne physique —
[nom du responsable de traitement — à renseigner avant la mise en ligne],
joignable à [adresse de contact — à renseigner avant la mise en ligne] (voir
`docs/architecture.md`, "Questions résolues" #3, et
`docs/privacy-policy.md`). Ces deux valeurs sont des placeholders jusqu'à
l'ouverture publique : voir `docs/v2-deployment.md` #16.

**Sous-traitants (destinataires)** :
- Aucun tiers commercial pour le stockage/traitement du contenu applicatif
  (self-hosted : Postgres et MinIO tournent sur le serveur exploité par le
  responsable de traitement). Le conteneur `ollama` déclaré dans
  `infra/docker-compose.yml` ne reçoit aucune donnée : aucun code de
  `apps/` ne l'appelle, et les suggestions de recettes sont un tri par
  règles (`apps/api/src/recipes/suggestions.rs`).
- **Mailjet** (Mailjet SAS, groupe Sinch), relais SMTP transactionnel —
  les cinq seuls emails que le service envoie : vérification d'adresse,
  réinitialisation de mot de passe, invitation à un groupe, rappel
  d'événement et avertissement avant la purge d'un compte désactivé (#256). Reçoit l'adresse du destinataire, l'objet et le corps de
  chacun (l'objet d'un rappel recopie le titre de l'événement ; le corps
  d'une invitation nomme le groupe et le membre qui invite). Retenu le
  2026-09-19 parmi les deux candidats étudiés (Brevo, Mailjet) pour son
  hébergement dans l'Union européenne, ses certifications ISO 27001 et
  SOC 2 et un périmètre contractuel limité à l'envoi transactionnel
  (`docs/architecture.md`). Le fournisseur porte un accord de traitement
  des données (DPA, art. 28) dans son cadre contractuel : il s'impose par
  l'acceptation des conditions, sans signature séparée, et la liste de ses
  sous-traitants ultérieurs est publiée à une URL publique plutôt
  qu'annexée au contrat. Le cadre exactement opposable, et la date à
  laquelle il l'est devenu, restent à établir :
  [cadre contractuel du sous-traitant email — à renseigner avant la mise
  en ligne]. À ce jour aucun compte d'envoi n'est ouvert et aucune donnée
  personnelle ne lui a été transmise ; l'établir est bloquant avant la
  mise en ligne (`docs/v2-deployment.md` #18).
- Google, pour deux flux distincts, chacun déclenché seulement par
  l'utilisateur :
  - la **connexion avec Google** (`/auth/google/start`,
    `/auth/google/callback`, portées `openid email profile`) — un mode de
    connexion au choix, à côté du mot de passe ;
  - l'**import calendrier** (Epic #9), si un administrateur ou le
    propriétaire du groupe fournit une URL de flux iCal privée.

**Mesures de sécurité communes** : chiffrement au repos via `pgcrypto` pour
les colonnes sensibles, isolation multi-tenant appliquée au niveau base de
données (Row-Level Security, `FORCE ROW LEVEL SECURITY` sur chaque table
tenant-scoped), TLS en transit, `cargo audit` en CI, logs d'audit sur les
actions sensibles (`audit_log`).

## Transferts hors de l'Union européenne

Les flux sortants sont ceux nommés ci-dessus, et aucun autre. Le mécanisme de
transfert (art. 44-49) se juge destinataire par destinataire, et **aucun n'est
arrêté à ce jour** : le service n'est pas ouvert, aucun compte d'envoi n'est
créé, et la dernière colonne porte donc un placeholder plutôt qu'une
affirmation. Ce qui est vérifié est écrit tel quel, avec sa source et sa date ;
le reste attend la mise en ligne (`docs/v2-deployment.md` #18).

| Destinataire | Ce qui sort | Ce qui est vérifié | Mécanisme (art. 44-49) |
|---|---|---|---|
| Mailjet (relais SMTP) | adresse du destinataire, objet et corps de l'email | Le stockage du flux email est dans l'UE : la liste des sous-traitants ultérieurs publiée par le groupe (`sinch.com/legal/data-protection-agreement-sub-processors/`, consultée le 2026-09-21) donne Google Cloud France SARL, centres en Allemagne et en Belgique, pour les clients européens. **Cela ne suffit pas à conclure** : la même liste nomme des entités établies hors UE pour des fonctions de support (Atlassian Corporation, San Francisco — suivi des tickets et gestion d'incidents), et le DPA du groupe réserve des transferts intra-groupe à l'échelle mondiale | [transferts hors UE du sous-traitant email — à renseigner avant la mise en ligne] |
| Google (connexion avec Google) | la demande d'autorisation, l'échange du code par le serveur, la lecture du profil (`sub`, email, nom) | Google Ireland Limited pour les utilisateurs de l'UE, sur une infrastructure mondiale dont une partie est aux États-Unis. Google déclare que « Google LLC, y compris ses filiales américaines détenues à 100 % (sauf exclusion explicite) » adhère aux principes du cadre de confidentialité des données — la réserve laisse ouverte l'entité qui traite effectivement, et la liste officielle du cadre n'a pas pu être consultée le 2026-09-21, le site répondant en erreur. Rien n'est donc vérifié ici au-delà de la déclaration | [transferts hors UE de Google — à renseigner avant la mise en ligne] |
| Hébergeur du flux iCal (Google en pratique) | l'URL de flux fournie et la requête du serveur vers cette URL | Le code accepte toute URL `http`/`https` et ne la contraint pas à l'UE : la localisation dépend de ce que le membre a collé | Celui de la ligne Google ci-dessus tant que le flux est hébergé par Google ; à rappeler à qui configure un import dans le cas contraire |
| Postgres, MinIO, Ollama | rien : ils tournent sur le serveur du responsable de traitement | Aucun code de `apps/` n'appelle un tiers pour ces trois services | Sans objet : pas de tiers, pas de transfert |

**Ce que cette section ne garantit pas.** La localisation du relais est une
garantie d'exploitation, pas une propriété du code : l'hôte vient de la
variable d'environnement `SMTP_HOST` (`apps/api/src/main.rs`), que rien ne
contraint à pointer sur Mailjet ni sur l'UE. Le contrôle correspondant est
porté par `docs/v2-deployment.md` #18, à faire avant la première mise en
ligne.

## Catégories de traitement (une par epic)

| # | Épic | Données traitées | Finalité | Base légale | Durée de conservation | Destinataires |
|---|---|---|---|---|---|---|
| 1 | Auth + Groupes | email, mot de passe (haché argon2), nom affiché, déclaration d'avoir au moins 15 ans et sa date (`users.age_declared_at` ; aucune date de naissance — art. 8 RGPD, #137), appartenance aux groupes et rôle, sessions (création, dernière activité, expiration, révocation) | Authentification, gestion de compte, isolation familiale | Exécution du contrat | Compte actif + 30j de grâce après demande de suppression, puis purge du compte (voir « Droit à l'effacement » ci-dessous) : la ligne `users` est anonymisée, déclaration d'âge effacée comprise, et l'appartenance aux groupes et le rôle sont supprimés ; une session vaut 30 jours au plus et prend fin après 7 jours sans activité ; sa ligne est supprimée par la purge de conservation horaire (`apps/api/src/jobs/retention_purge.rs`, #138) dès que la session a pris fin — révocation (déconnexion, changement de mot de passe), `expires_at` dépassé, ou `last_seen_at` plus vieux que 7 jours, le même critère que l'extracteur de session (`last_seen_at` n'est réécrit qu'une fois par heure) — ou que l'état du compte la refuse pour de bon : session ordinaire d'un compte désactivé, session restreinte (`sessions.restricted`, #289) d'un compte qui ne l'est plus ; et au plus tard à la purge du compte. Un compte désactivé par le superadmin est purgé après 2 ans de désactivation (voir ligne 8) ; un compte dont la suppression a été demandée l'est au terme de ses 30 jours de grâce, désactivé ou non (#139) | Aucun tiers |
| 1a | Connexion avec Google | identifiant Google (`sub`), email et nom du profil Google (le nom sert de nom affiché si le compte est créé à cette occasion), jeton de rafraîchissement chiffré via `pgcrypto` quand Google en délivre un — aucun code ne le relit aujourd'hui (`oauth_identities`) | Authentification par un fournisseur d'identité, au choix de l'utilisateur | Exécution du contrat | Jusqu'à la purge du compte, qui supprime la ligne `oauth_identities` | Google (fournisseur d'identité : reçoit la demande d'autorisation, échange le code, sert le profil) |
| 1b | Jetons de vérification d'email et de réinitialisation du mot de passe | jeton, compte concerné, dates de création, d'expiration et de consommation (`email_verification_tokens`, `password_reset_tokens`) | Prouver la possession de l'adresse ; réinitialiser un mot de passe oublié | Exécution du contrat | Valables 24 h (vérification) et 1 h (réinitialisation), à usage unique ; un jeton de réinitialisation est supprimé à l'usage ; la purge de conservation horaire (#138) supprime un jeton de vérification 48 h après sa création, consommé ou non, et un jeton de réinitialisation inutilisé 1 h après ; la purge du compte supprime ceux qui restent | Mailjet (le lien porteur du jeton part par email) |
| 1c | Invitations à un groupe | adresse email de la personne invitée, facultative (une invitation peut n'être qu'un lien) et stockée en clair, jeton, auteur, dates (`invitations`) ; la colonne `consumed_by`, qui nommait le membre ayant accepté, n'est plus jamais renseignée depuis #138 — l'acceptation supprime la ligne | Faire entrer une personne dans un groupe ; la personne invitée est un tiers qui n'a pas encore de compte | Intérêt légitime (du membre qui invite un proche) | Valable 7 jours, à usage unique ; la ligne, adresse comprise, est supprimée à l'acceptation, sinon par la purge de conservation horaire 30 jours après sa création (#138), ou avant, avec le groupe s'il est supprimé ou à la purge du compte du membre qui l'a émise (#139) | Mailjet (si une adresse est saisie : email portant le nom du groupe, le nom affiché du membre qui invite, le lien, et la notice d'information de l'art. 14 — identité et contact du responsable, finalité, base légale, durée de conservation, source de l'adresse, lien vers la politique, droit de réclamation ; #134) |
| 1d | Protection de la connexion | adresse IP du client (en IPv6, réduite à son /64) et email saisi, à chaque tentative de connexion par mot de passe (`apps/api/src/auth/throttle.rs`) | Limiter les essais de mot de passe par couple (adresse, email) | Intérêt légitime (sécurité du service) | En mémoire du processus `api` seulement, jamais en base ; perdu au redémarrage, oublié dès une connexion réussie ; une entrée cesse de compter 15 min après sa première tentative (ou à la fin du blocage de 15 min) mais n'est retirée qu'à la prochaine tentative d'un autre email depuis la même adresse, quand la table atteint `MAX_TRACKED` (10 000 couples) ou au redémarrage — sans trafic, elle reste jusqu'au redémarrage | Aucun tiers |
| 2 | Agenda | événements, tâches et membre ayant coché une tâche, pièces jointes (photos/documents) | Planification familiale | Exécution du contrat | Tant que l'événement/le compte existe ; supprimé avec le groupe ou anonymisé (`created_by`) à la purge du compte auteur ; un fichier de pièce jointe orphelin (sans ligne en base) est supprimé par un balayage quotidien moins de 48 h après son écriture (fenêtre de 24 h + intervalle de 24 h) tant que l'API tourne, et à condition que `ADMIN_DATABASE_URL` désigne un rôle `BYPASSRLS` — sinon le balayage refuse de tourner et l'orphelin reste | Aucun tiers |
| 2a | Rappels d'événements par email | délai avant l'événement (`event_reminders`) ; file d'envoi par occurrence : heure d'envoi, statut, tentatives, dernière erreur de transport (`scheduled_notifications`) ; l'email porte le titre et la date de l'événement | Prévenir avant un événement | Exécution du contrat | Un rappel vit jusqu'à sa suppression ou celle de l'événement ; les lignes de la file restent après envoi (statut `sent` ou `failed`) et partent avec le rappel ou l'événement | Mailjet ; l'email part à l'adresse du **créateur de l'événement**, quel que soit le membre qui a posé le rappel (`apps/api/src/jobs/scheduled_notifications.rs`) |
| 2b | Assignations d'événements | événement, membre assigné, date (`event_assignees`) ; posées par un membre, par défaut le créateur de l'événement, et pour un événement importé le membre qui a lancé l'import | Indiquer pour qui est un événement | Exécution du contrat | Tant que l'événement existe et que l'assignation n'est pas retirée ; elle survit au départ du groupe, pas à la purge du compte, qui la supprime (#139) | Membres du groupe |
| 3 | Stocks | articles du garde-manger/frigo, quantités, seuils | Gestion de l'inventaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 4 | Recettes | recettes, ingrédients, historique des repas | Suggestions de repas (algorithme local, pas d'IA tierce) | Exécution du contrat | Idem #2 | Aucun tiers |
| 5 | Liste de courses | articles à acheter, source (manuel/recette/stock bas) | Liste de courses partagée | Exécution du contrat | Idem #2 | Aucun tiers |
| 6 | Budget | dépenses saisies manuellement (montant, nom, date) | Suivi du budget alimentaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7 | Messagerie | contenu des messages (chiffré au repos via `pgcrypto`) | Communication au sein du groupe familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7a | État de lecture de la messagerie | un horodatage par (groupe, membre), avancé quand le membre ouvre la messagerie (`message_read_state`) | Compter les messages non lus de ce membre | Exécution du contrat | Tant que le groupe existe ; survit au départ du groupe, pas à la purge du compte, qui la supprime (#139) | Aucun tiers |
| 8 | User admin (superadmin) | liste des groupes/utilisateurs à l'échelle globale ; désactivation et réactivation d'un compte, date de désactivation (`users.deactivated_at`) et date de l'email d'avertissement (`users.deactivation_notice_sent_at`) ; demande de réactivation du titulaire, sa date et son message facultatif de 1000 caractères au plus (`account_reactivation_requests`, #289), date du dernier refus (`users.reactivation_refused_at`) | Support technique/maintenance de la plateforme | Intérêt légitime (exploitation du service) | La désactivation n'efface rien et dure jusqu'à la réactivation, qui efface les deux dates. La demande de réactivation — une en attente à la fois — est supprimée par la décision du superadmin (réactivation ou refus) ou par la purge du compte ; tant qu'elle est en attente, la purge à 2 ans et son email d'avertissement sont suspendus, si c'est la première depuis la désactivation. Un refus date `reactivation_refused_at` : une nouvelle demande reste possible mais ne suspend plus rien (arbitrage du responsable de traitement du 2026-09-29) ; il efface aussi la date de l'email, si bien que, si l'avertissement était déjà parti, un nouveau part une fois l'échéance à 30 jours ou moins, et la purge a lieu 30 jours après lui au plus tôt ; elle n'a de toute façon jamais lieu moins de 30 jours après le refus, que ce nouvel email parte ou non (#296). Chaque nouveau refus en fait autant. `reactivation_refused_at` est effacé par la réactivation, une nouvelle désactivation et la purge. Sans réactivation, le compte est purgé (voir « Droit à l'effacement » ci-dessous) après 2 ans de désactivation — durée recommandée par la CNIL pour un compte inactif, arbitrage du responsable de traitement du 2026-09-28 (#256) ; son titulaire est averti par email 30 jours avant (de nouveau après chaque refus qui suit un avertissement, dans le cas ci-dessus), et la purge n'a jamais lieu moins de 30 jours après cet email ; s'il n'a pas pu partir, elle a lieu au plus tôt 2 ans et 30 jours après la désactivation | Mailjet (l'email d'avertissement : adresse du compte, dates de désactivation et de purge, identité et contact du responsable) |
| 9 | Import calendrier Google | URL de flux iCal privée (chiffrée), libellé, date du dernier import, événements importés et leur identifiant externe | Miroir en lecture seule d'un agenda Google externe | Consentement explicite (l'utilisateur fournit volontairement l'URL) | Jusqu'à la suppression de l'import par un administrateur ou le propriétaire du groupe, ou du groupe ; les événements importés restent après la suppression de l'import sauf si leur suppression est demandée avec lui ; la purge du compte du membre qui a configuré l'import supprime l'import, URL de flux comprise, et laisse ses événements dans le groupe (#139) | Hébergeur du flux (Google en pratique ; le code accepte toute URL `http`/`https`), interrogé par le serveur à chaque import lancé par un membre — aucune synchronisation en arrière-plan |
| — | Logs d'audit (transverse) | horodatage, acteur, action, cible, métadonnées (identifiants et compteurs) ; actions journalisées : export, demande et annulation de suppression de compte, demande de réactivation d'un compte désactivé, purge, suppression de groupe, transfert de propriété, changement de rôle, consultation des listes, désactivation et réactivation d'un compte et refus d'une demande de réactivation par le superadmin — les connexions ne le sont pas | Traçabilité de sécurité, obligations RGPD (preuve des actions d'export/suppression) | Intérêt légitime | 6 mois glissants (recommandation de la CNIL pour les journaux, délibération n° 2021-122), appliqués par la purge de conservation horaire (#138) ; la purge d'un compte supprime aussitôt les entrées dont il est l'acteur ; restent, pour la même durée, celles qui le concernent sans être de son fait (voir « Droit à l'effacement » ci-dessous) (#139). Le minimum d'un an du décret n° 2021-1362 a été écarté : il vise les hébergeurs et les services de communication au public, pas une application familiale portée par une personne physique — arbitrage du responsable de traitement du 2026-09-19, à revoir si le service change de nature | Aucun tiers |
| 12 | RGPD (export/suppression) | export à la demande (Art. 20), demande/annulation de suppression (Art. 17) | Exercice des droits RGPD | Obligation légale | L'export n'est pas persisté côté serveur (généré à la demande, retourné directement) | Aucun tiers |

## Droit à l'effacement — modalités de purge

Décrit en détail dans `docs/privacy-policy.md` et implémenté par
`apps/api/src/jobs/account_purge.rs` : à l'expiration du délai de grâce de
30 jours suivant `POST /account/delete` — que le compte ait été désactivé
depuis ou non (#139), et qu'une demande de réactivation soit en attente ou
non — ou après 2 ans de désactivation par le superadmin sans première
demande de réactivation en attente (ligne 8, #256, #289), le job de purge,
en une
transaction par compte :

1. Supprime les lignes que le schéma rattache au compte par
   `ON DELETE CASCADE` — la ligne `users` n'étant qu'anonymisée, aucune
   cascade ne se déclenche d'elle-même : identités OAuth
   (`oauth_identities`), sessions, jetons de vérification et de
   réinitialisation, appartenance aux groupes et rôle (`group_members`),
   état de lecture de la messagerie (`message_read_state`), assignations
   d'événements (`event_assignees`), demande de réactivation en attente
   (`account_reactivation_requests`, #289).
2. Supprime la part personnelle des autres tables : les entrées
   `audit_log` dont le compte est l'acteur, les invitations qu'il a
   émises (adresses de tiers comprises) et les imports calendrier qu'il a
   configurés (`calendar_imports`, URL de flux comprise — un porteur
   d'accès à un agenda externe ; les événements importés restent).
3. Anonymise la ligne `users` : email et nom remplacés, mot de passe
   haché et date de déclaration d'âge effacés, `deleted_at` renseigné.
4. Écrit, sans acteur, une entrée `audit_log` `account_purged` qui
   date la purge, et une entrée `ownership_transferred` par groupe dont
   il transfère la propriété.

Arbitrage du responsable de traitement du 2026-09-19 (#139) : est
supprimé tout ce qui n'est pas du contenu partagé avec le groupe. Le
contenu partagé reste, rattaché à la ligne anonymisée : événements, pièces
jointes et leurs fichiers, tâches cochées
(`event_occurrence_completions`), messages, stocks, recettes, historique
des repas, liste de courses, budget, et les groupes créés par le compte.
Cette conservation est portée par les conditions générales
(`docs/terms-of-service.md`, « Vos contenus »). Restent aussi, jusqu'au
terme de leurs 6 mois, les entrées `audit_log` qui concernent le compte
sans être de son fait : `account_purged` et les `ownership_transferred`
écrites par sa purge, celles d'un autre acteur qui le désignent
(changement de rôle, transfert de propriété, action du superadmin), et
les `ownership_transferred` qui le désignent comme successeur à la purge
d'un autre compte. Elles ne désignent plus qu'un identifiant anonymisé.
Reste enfin une invitation adressée à l'adresse du compte, jusqu'à son
acceptation, la suppression du groupe ou le terme de ses 30 jours
(ligne 1c).

Le job tourne une fois par heure, dès le démarrage de l'API, aux mêmes
conditions que la purge de conservation ci-dessous : cinq des tables qu'il
vide (`group_members`, `message_read_state`, `event_assignees`,
`invitations`, `calendar_imports`) sont sous une politique RLS forcée, et
sur un rôle sans `BYPASSRLS` il refuse de tourner plutôt que de marquer un
compte purgé en laissant ces lignes en place.

Un compte peut devenir propriétaire d'un groupe pendant son délai de grâce
(en en créant un, ou parce qu'on lui en a transféré la propriété). La purge
transfère alors la propriété, dans la même transaction, et l'inscrit au
journal (`ownership_transferred`, sans acteur). Ordre de succession, le
plus ancien d'abord dans chaque rang (`joined_at`, puis `user_id`) :
administrateurs actifs, membres actifs, administrateurs dont la
suppression est demandée (`deletion_requested_at` — ils peuvent encore
l'annuler), membres dont la suppression est demandée. Un membre désactivé
par le support (`deactivated_at`) n'hérite jamais : s'il ne reste que de tels
membres, le groupe reste sans propriétaire et ils gardent leur
appartenance. Un groupe dont le compte était le seul membre reste, avec
son contenu, sans membre — arbitrage du responsable de traitement du
2026-09-28.

Un compte purgé ne peut plus se connecter par aucune voie : la connexion
par mot de passe et l'extracteur de session refusent une ligne
`deleted_at` ; ses sessions, son identité Google et ses jetons sont
supprimés ; la vérification d'email et la réinitialisation du mot de passe
refusent un jeton d'un compte `deleted_at` ; la connexion avec Google ne
retrouve ni l'identité ni l'email, et crée un compte nouveau, distinct.

Un compte désactivé par le support (`deactivated_at`) n'obtient plus de
session ordinaire (#289). Avec de mauvais identifiants, la connexion par
mot de passe répond comme pour un identifiant inconnu. Avec les bons — mot
de passe sur une adresse vérifiée, ou profil Google vérifié, qui retrouve
le compte par son identité ou son email sans rien lui écrire (#194) —
elle ouvre une session restreinte (`sessions.restricted`) : l'extracteur
de session la refuse partout (403 `account_deactivated`), sauf sur les
deux routes du compte désactivé (`/account/deactivated` et sa demande de
réactivation) et la déconnexion (`POST /auth/logout`). La vérification
d'email, la
réinitialisation du mot de passe (ses jetons subsistent jusqu'à la purge
de conservation), la demande de lien de réinitialisation et le renvoi de
vérification le refusent comme un identifiant inconnu. La demande de
réactivation (`POST /account/deactivated/reactivation-request`, journal
`account_reactivation_requested`) est acceptée ou refusée par le
superadmin : la réactivation (`POST /admin/users/:id/reactivate`, journal
`admin.user.reactivate`, qui indique si elle répondait à une demande) lui
rend la connexion ; les sessions révoquées à la désactivation le restent,
et la session restreinte est révoquée. Le refus
(`POST /admin/users/:id/reactivation-request/refuse`, journal
`admin.user.reactivation_refuse`) laisse le compte désactivé et date
`reactivation_refused_at` (ligne 8).

## Durées de conservation — purge horaire

`apps/api/src/jobs/retention_purge.rs` (#138) applique, une fois par heure
et dès le démarrage de l'API, les durées du tableau ci-dessus, décidées par
le responsable de traitement le 2026-09-19 :

| Table | Supprimé |
|---|---|
| `audit_log` | entrée de plus de 6 mois (`occurred_at`) |
| `email_verification_tokens` | jeton créé il y a plus de 48 h |
| `password_reset_tokens` | jeton créé il y a plus de 1 h (un jeton utilisé l'est déjà à l'usage) |
| `invitations` | invitation créée il y a plus de 30 jours (une invitation acceptée l'est déjà à l'acceptation) |
| `sessions` | session révoquée, expirée, inactive depuis plus de 7 jours, d'un compte purgé, ordinaire d'un compte désactivé, ou restreinte d'un compte qui ne l'est plus (#289) |

Une ligne vit jusqu'à une heure de plus que sa durée **tant que la passe
tourne**, et la passe ne tourne qu'à deux conditions : l'API est démarrée,
et `ADMIN_DATABASE_URL` (à défaut, `DATABASE_URL`) désigne un rôle
`BYPASSRLS`. `invitations` est sous une politique RLS forcée : sur un autre
rôle, la passe refuse de tourner (erreur journalisée à chaque heure) plutôt
que de ne rien supprimer en se disant réussie. Une interruption de l'API
diffère les suppressions jusqu'au premier passage après le redémarrage ; un
rôle sans `BYPASSRLS` — le recours que `docs/v2-deployment.md` (item 9)
indique avant une restauration — les suspend, API debout, jusqu'à ce que la
configuration soit rétablie. Aucune de ces deux situations n'a de durée
bornée : aucun plafond de suppression n'est donc promis, ni ici ni dans les
documents publics (`docs/privacy-policy.md`, notice de l'art. 14). Ce qui
est publié, c'est la durée de conservation et la fréquence de la passe.

Le contenu créé par l'utilisateur au sein des groupes (événements, messages,
etc.) n'est **pas** supprimé — il reste attribué à l'utilisateur anonymisé,
un choix documenté (le contenu appartient fonctionnellement au groupe
familial partagé, pas uniquement à son auteur). Un utilisateur ne peut pas
demander sa suppression tant qu'il est propriétaire d'un groupe, que ce
groupe ait ou non d'autres membres (transfert de propriété ou suppression
du groupe requis au préalable) — appliqué dans
`apps/api/src/auth/mod.rs::delete_account`.
