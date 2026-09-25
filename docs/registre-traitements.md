# Registre des traitements — Manage Our Home

Registre tenu au titre de l'article 30 du RGPD. Dernière mise à jour :
2026-09-22, relu contre les migrations `apps/api/migrations/0001` à `0015`
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
  les quatre seuls emails que le service envoie : vérification d'adresse,
  réinitialisation de mot de passe, invitation à un groupe et rappel
  d'événement. Reçoit l'adresse du destinataire, l'objet et le corps de
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
| 1 | Auth + Groupes | email, mot de passe (haché argon2), nom affiché, déclaration d'avoir au moins 15 ans et sa date (`users.age_declared_at` ; aucune date de naissance — art. 8 RGPD, #137), appartenance aux groupes et rôle, sessions (création, dernière activité, expiration, révocation) | Authentification, gestion de compte, isolation familiale | Exécution du contrat | Compte actif + 30j de grâce après demande de suppression, puis anonymisation définitive — la déclaration d'âge survit à l'anonymisation, réduite à une date rattachée à aucune identité ; une session vaut 30 jours au plus et prend fin après 7 jours sans activité ; sa ligne est supprimée par la purge de conservation horaire (`apps/api/src/jobs/retention_purge.rs`, #138) dès que la session a pris fin — révocation (déconnexion, changement de mot de passe), `expires_at` dépassé, ou `last_seen_at` plus vieux que 7 jours, le même critère que l'extracteur de session (`last_seen_at` n'est réécrit qu'une fois par heure) — ou que le compte est désactivé, et au plus tard à la purge du compte | Aucun tiers |
| 1a | Connexion avec Google | identifiant Google (`sub`), email et nom du profil Google (le nom sert de nom affiché si le compte est créé à cette occasion), jeton de rafraîchissement chiffré via `pgcrypto` quand Google en délivre un — aucun code ne le relit aujourd'hui (`oauth_identities`) | Authentification par un fournisseur d'identité, au choix de l'utilisateur | Exécution du contrat | Jusqu'à la purge du compte, qui supprime la ligne `oauth_identities` | Google (fournisseur d'identité : reçoit la demande d'autorisation, échange le code, sert le profil) |
| 1b | Jetons de vérification d'email et de réinitialisation du mot de passe | jeton, compte concerné, dates de création, d'expiration et de consommation (`email_verification_tokens`, `password_reset_tokens`) | Prouver la possession de l'adresse ; réinitialiser un mot de passe oublié | Exécution du contrat | Valables 24 h (vérification) et 1 h (réinitialisation), à usage unique ; un jeton de réinitialisation est supprimé à l'usage ; la purge de conservation horaire (#138) supprime un jeton de vérification 48 h après sa création, consommé ou non, et un jeton de réinitialisation inutilisé 1 h après | Mailjet (le lien porteur du jeton part par email) |
| 1c | Invitations à un groupe | adresse email de la personne invitée, facultative (une invitation peut n'être qu'un lien) et stockée en clair, jeton, auteur, dates (`invitations`) ; la colonne `consumed_by`, qui nommait le membre ayant accepté, n'est plus jamais renseignée depuis #138 — l'acceptation supprime la ligne | Faire entrer une personne dans un groupe ; la personne invitée est un tiers qui n'a pas encore de compte | Intérêt légitime (du membre qui invite un proche) | Valable 7 jours, à usage unique ; la ligne, adresse comprise, est supprimée à l'acceptation, sinon par la purge de conservation horaire 30 jours après sa création (#138), ou avec le groupe s'il est supprimé avant | Mailjet (si une adresse est saisie : email portant le nom du groupe, le nom affiché du membre qui invite, le lien, et la notice d'information de l'art. 14 — identité et contact du responsable, finalité, base légale, durée de conservation, source de l'adresse, lien vers la politique, droit de réclamation ; #134) |
| 1d | Protection de la connexion | adresse IP du client (en IPv6, réduite à son /64) et email saisi, à chaque tentative de connexion par mot de passe (`apps/api/src/auth/throttle.rs`) | Limiter les essais de mot de passe par couple (adresse, email) | Intérêt légitime (sécurité du service) | En mémoire du processus `api` seulement, jamais en base ; perdu au redémarrage, oublié dès une connexion réussie ; une entrée cesse de compter 15 min après sa première tentative (ou à la fin du blocage de 15 min) mais n'est retirée qu'à la prochaine tentative d'un autre email depuis la même adresse, quand la table atteint `MAX_TRACKED` (10 000 couples) ou au redémarrage — sans trafic, elle reste jusqu'au redémarrage | Aucun tiers |
| 2 | Agenda | événements, tâches et membre ayant coché une tâche, pièces jointes (photos/documents) | Planification familiale | Exécution du contrat | Tant que l'événement/le compte existe ; supprimé avec le groupe ou anonymisé (`created_by`) à la purge du compte auteur ; un fichier de pièce jointe orphelin (sans ligne en base) est supprimé par un balayage quotidien moins de 48 h après son écriture (fenêtre de 24 h + intervalle de 24 h) tant que l'API tourne, et à condition que `ADMIN_DATABASE_URL` désigne un rôle `BYPASSRLS` — sinon le balayage refuse de tourner et l'orphelin reste | Aucun tiers |
| 2a | Rappels d'événements par email | délai avant l'événement (`event_reminders`) ; file d'envoi par occurrence : heure d'envoi, statut, tentatives, dernière erreur de transport (`scheduled_notifications`) ; l'email porte le titre et la date de l'événement | Prévenir avant un événement | Exécution du contrat | Un rappel vit jusqu'à sa suppression ou celle de l'événement ; les lignes de la file restent après envoi (statut `sent` ou `failed`) et partent avec le rappel ou l'événement | Mailjet ; l'email part à l'adresse du **créateur de l'événement**, quel que soit le membre qui a posé le rappel (`apps/api/src/jobs/scheduled_notifications.rs`) |
| 2b | Assignations d'événements | événement, membre assigné, date (`event_assignees`) ; posées par un membre, par défaut le créateur de l'événement, et pour un événement importé le membre qui a lancé l'import | Indiquer pour qui est un événement | Exécution du contrat | Tant que l'événement existe et que l'assignation n'est pas retirée ; elle survit au départ du groupe et à la purge du compte (le compte est anonymisé, pas supprimé) | Membres du groupe |
| 3 | Stocks | articles du garde-manger/frigo, quantités, seuils | Gestion de l'inventaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 4 | Recettes | recettes, ingrédients, historique des repas | Suggestions de repas (algorithme local, pas d'IA tierce) | Exécution du contrat | Idem #2 | Aucun tiers |
| 5 | Liste de courses | articles à acheter, source (manuel/recette/stock bas) | Liste de courses partagée | Exécution du contrat | Idem #2 | Aucun tiers |
| 6 | Budget | dépenses saisies manuellement (montant, nom, date) | Suivi du budget alimentaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7 | Messagerie | contenu des messages (chiffré au repos via `pgcrypto`) | Communication au sein du groupe familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7a | État de lecture de la messagerie | un horodatage par (groupe, membre), avancé quand le membre ouvre la messagerie (`message_read_state`) | Compter les messages non lus de ce membre | Exécution du contrat | Tant que le groupe existe ; survit au départ du groupe et à la purge du compte (le compte est anonymisé, pas supprimé) | Aucun tiers |
| 8 | User admin (superadmin) | liste des groupes/utilisateurs à l'échelle globale, action de désactivation | Support technique/maintenance de la plateforme | Intérêt légitime (exploitation du service) | Durée de vie du compte concerné | Aucun tiers |
| 9 | Import calendrier Google | URL de flux iCal privée (chiffrée), libellé, date du dernier import, événements importés et leur identifiant externe | Miroir en lecture seule d'un agenda Google externe | Consentement explicite (l'utilisateur fournit volontairement l'URL) | Jusqu'à la suppression de l'import par un administrateur ou le propriétaire du groupe, ou du groupe ; les événements importés restent après la suppression de l'import sauf si leur suppression est demandée avec lui ; la purge du compte ne supprime ni l'import ni ses événements | Hébergeur du flux (Google en pratique ; le code accepte toute URL `http`/`https`), interrogé par le serveur à chaque import lancé par un membre — aucune synchronisation en arrière-plan |
| — | Logs d'audit (transverse) | horodatage, acteur, action, cible, métadonnées (identifiants et compteurs) ; actions journalisées : export, demande et annulation de suppression de compte, purge, suppression de groupe, transfert de propriété, changement de rôle, consultation des listes et désactivation d'un compte par le superadmin — les connexions ne le sont pas | Traçabilité de sécurité, obligations RGPD (preuve des actions d'export/suppression) | Intérêt légitime | 6 mois glissants (recommandation de la CNIL pour les journaux, délibération n° 2021-122), appliqués par la purge de conservation horaire (#138). Le minimum d'un an du décret n° 2021-1362 a été écarté : il vise les hébergeurs et les services de communication au public, pas une application familiale portée par une personne physique — arbitrage du responsable de traitement du 2026-09-19, à revoir si le service change de nature | Aucun tiers |
| 12 | RGPD (export/suppression) | export à la demande (Art. 20), demande/annulation de suppression (Art. 17) | Exercice des droits RGPD | Obligation légale | L'export n'est pas persisté côté serveur (généré à la demande, retourné directement) | Aucun tiers |

## Droit à l'effacement — modalités de purge

Décrit en détail dans `docs/privacy-policy.md` et implémenté par
`apps/api/src/jobs/account_purge.rs` : à l'expiration du délai de grâce de
30 jours suivant `POST /account/delete`, le job de purge :
1. Supprime les identités OAuth et toutes les sessions de l'utilisateur.
2. Anonymise la ligne `users` (email/nom remplacés, `deleted_at` renseigné).
3. Écrit une entrée `audit_log` pour la purge.

Le job ne supprime ni l'état de lecture de la messagerie, ni les
assignations d'événements : ces lignes restent rattachées à l'utilisateur
anonymisé (#139). Les jetons de vérification et de réinitialisation et les
invitations émises ne sont pas supprimés par ce job, mais par la purge de
conservation ci-dessous, au terme de leur propre durée.

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
| `sessions` | session révoquée, expirée, inactive depuis plus de 7 jours, ou d'un compte désactivé |

Une ligne vit donc une heure de plus que sa durée **tant que l'API tourne** :
la passe ne tourne pas sans elle, et une interruption diffère d'autant les
suppressions, qui reprennent au premier passage après le redémarrage. Aucun
plafond de suppression n'est donc promis, ni ici ni dans les documents
publics (`docs/privacy-policy.md`, notice de l'art. 14) : ce qui est publié,
c'est la durée de conservation et la fréquence de la passe. La passe tourne
sur `ADMIN_DATABASE_URL` : `invitations` est sous une politique RLS forcée,
et sans `BYPASSRLS` elle refuse de tourner (erreur journalisée à chaque
heure) plutôt que de ne rien supprimer en se disant réussie.

Le contenu créé par l'utilisateur au sein des groupes (événements, messages,
etc.) n'est **pas** supprimé — il reste attribué à l'utilisateur anonymisé,
un choix documenté (le contenu appartient fonctionnellement au groupe
familial partagé, pas uniquement à son auteur). Un utilisateur ne peut pas
demander sa suppression tant qu'il est seul propriétaire d'un groupe ayant
d'autres membres (transfert de propriété ou suppression du groupe requis au
préalable) — appliqué dans `apps/api/src/auth/mod.rs::delete_account`.
