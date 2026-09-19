# Registre des traitements — Manage Our Home

Registre tenu au titre de l'article 30 du RGPD. Dernière mise à jour :
2026-09-19, relu contre les migrations `apps/api/migrations/0001` à `0014`
et la table des routes de `apps/api/src/lib.rs`. Un registre des
traitements est requis dès qu'un traitement de données personnelles est
effectué, y compris à petite échelle — voir `docs/architecture.md` §10.

**Responsable de traitement** : `placeholder_name` (voir
`docs/architecture.md`, "Questions résolues" #3, et
`docs/privacy-policy.md`).

**Sous-traitants (destinataires)** :
- Aucun tiers commercial pour le stockage/traitement du contenu applicatif
  (self-hosted : Postgres et MinIO tournent sur le serveur exploité par le
  responsable de traitement). Le conteneur `ollama` déclaré dans
  `infra/docker-compose.yml` ne reçoit aucune donnée : aucun code de
  `apps/` ne l'appelle, et les suggestions de recettes sont un tri par
  règles (`apps/api/src/recipes/suggestions.rs`).
- Fournisseur SMTP transactionnel basé UE (Brevo ou Mailjet) — emails de
  vérification d'adresse, de réinitialisation de mot de passe, d'invitation
  à un groupe et de rappel d'événement ; DPA à documenter au moment du
  choix définitif du fournisseur (`docs/architecture.md`).
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

## Catégories de traitement (une par epic)

| # | Épic | Données traitées | Finalité | Base légale | Durée de conservation | Destinataires |
|---|---|---|---|---|---|---|
| 1 | Auth + Groupes | email, mot de passe (haché argon2), nom affiché, appartenance aux groupes et rôle, sessions (création, dernière activité, expiration, révocation) | Authentification, gestion de compte, isolation familiale | Exécution du contrat | Compte actif + 30j de grâce après demande de suppression, puis anonymisation définitive ; une session vaut 30 jours, sa ligne reste après expiration ou déconnexion et n'est supprimée qu'à la purge du compte | Aucun tiers |
| 1a | Connexion avec Google | identifiant Google (`sub`), email et nom du profil Google (le nom sert de nom affiché si le compte est créé à cette occasion), jeton de rafraîchissement chiffré via `pgcrypto` quand Google en délivre un — aucun code ne le relit aujourd'hui (`oauth_identities`) | Authentification par un fournisseur d'identité, au choix de l'utilisateur | Exécution du contrat | Jusqu'à la purge du compte, qui supprime la ligne `oauth_identities` | Google (fournisseur d'identité : reçoit la demande d'autorisation, échange le code, sert le profil) |
| 1b | Jetons de vérification d'email et de réinitialisation du mot de passe | jeton, compte concerné, dates de création, d'expiration et de consommation (`email_verification_tokens`, `password_reset_tokens`) | Prouver la possession de l'adresse ; réinitialiser un mot de passe oublié | Exécution du contrat | Valables 24 h, à usage unique ; la ligne reste après consommation ou expiration, sans limite de durée — aucune purge n'existe (#138) | Relais SMTP (le lien porteur du jeton part par email) |
| 1c | Invitations à un groupe | adresse email de la personne invitée, facultative (une invitation peut n'être qu'un lien) et stockée en clair, jeton, auteur, dates, membre qui l'a acceptée (`invitations`) | Faire entrer une personne dans un groupe ; la personne invitée est un tiers qui n'a pas encore de compte | Intérêt légitime (du membre qui invite un proche) | Valable 7 jours, à usage unique ; la ligne, adresse comprise, reste après acceptation ou expiration jusqu'à la suppression du groupe — aucune purge n'existe (#138) | Relais SMTP (email portant le nom du groupe et le lien, si une adresse est saisie) |
| 1d | Protection de la connexion | adresse IP du client (en IPv6, réduite à son /64) et email saisi, à chaque tentative de connexion par mot de passe (`apps/api/src/auth/throttle.rs`) | Limiter les essais de mot de passe par couple (adresse, email) | Intérêt légitime (sécurité du service) | En mémoire du processus `api` seulement, jamais en base ; perdu au redémarrage, oublié dès une connexion réussie | Aucun tiers |
| 2 | Agenda | événements, tâches et membre ayant coché une tâche, pièces jointes (photos/documents) | Planification familiale | Exécution du contrat | Tant que l'événement/le compte existe ; supprimé avec le groupe ou anonymisé (`created_by`) à la purge du compte auteur ; un fichier de pièce jointe orphelin (sans ligne en base) est supprimé par un balayage quotidien moins de 48 h après son écriture (fenêtre de 24 h + intervalle de 24 h) tant que l'API tourne, et à condition que `ADMIN_DATABASE_URL` désigne un rôle `BYPASSRLS` — sinon le balayage refuse de tourner et l'orphelin reste | Aucun tiers |
| 2a | Rappels d'événements par email | délai avant l'événement (`event_reminders`) ; file d'envoi par occurrence : heure d'envoi, statut, tentatives, dernière erreur de transport (`scheduled_notifications`) ; l'email porte le titre et la date de l'événement | Prévenir avant un événement | Exécution du contrat | Un rappel vit jusqu'à sa suppression ou celle de l'événement ; les lignes de la file restent après envoi (statut `sent` ou `failed`) et partent avec le rappel ou l'événement | Relais SMTP ; l'email part à l'adresse du **créateur de l'événement**, quel que soit le membre qui a posé le rappel (`apps/api/src/jobs/scheduled_notifications.rs`) |
| 2b | Assignations d'événements | événement, membre assigné, date (`event_assignees`) ; posées par un membre, par défaut le créateur de l'événement, et pour un événement importé le membre qui a lancé l'import | Indiquer pour qui est un événement | Exécution du contrat | Tant que l'événement existe et que l'assignation n'est pas retirée ; elle survit au départ du groupe et à la purge du compte (le compte est anonymisé, pas supprimé) | Membres du groupe |
| 3 | Stocks | articles du garde-manger/frigo, quantités, seuils | Gestion de l'inventaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 4 | Recettes | recettes, ingrédients, historique des repas | Suggestions de repas (algorithme local, pas d'IA tierce) | Exécution du contrat | Idem #2 | Aucun tiers |
| 5 | Liste de courses | articles à acheter, source (manuel/recette/stock bas) | Liste de courses partagée | Exécution du contrat | Idem #2 | Aucun tiers |
| 6 | Budget | dépenses saisies manuellement (montant, nom, date) | Suivi du budget alimentaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7 | Messagerie | contenu des messages (chiffré au repos via `pgcrypto`) | Communication au sein du groupe familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7a | État de lecture de la messagerie | un horodatage par (groupe, membre), avancé quand le membre ouvre la messagerie (`message_read_state`) | Compter les messages non lus de ce membre | Exécution du contrat | Tant que le groupe existe ; survit au départ du groupe et à la purge du compte (le compte est anonymisé, pas supprimé) | Aucun tiers |
| 8 | User admin (superadmin) | liste des groupes/utilisateurs à l'échelle globale, action de désactivation | Support technique/maintenance de la plateforme | Intérêt légitime (exploitation du service) | Durée de vie du compte concerné | Aucun tiers |
| 9 | Import calendrier Google | URL de flux iCal privée (chiffrée), libellé, date du dernier import, événements importés et leur identifiant externe | Miroir en lecture seule d'un agenda Google externe | Consentement explicite (l'utilisateur fournit volontairement l'URL) | Jusqu'à la suppression de l'import par un administrateur ou le propriétaire du groupe, ou du groupe ; les événements importés restent après la suppression de l'import sauf si leur suppression est demandée avec lui ; la purge du compte ne supprime ni l'import ni ses événements | Hébergeur du flux (Google en pratique ; le code accepte toute URL `http`/`https`), interrogé par le serveur à chaque import lancé par un membre — aucune synchronisation en arrière-plan |
| — | Logs d'audit (transverse) | horodatage, acteur, action, cible, métadonnées (identifiants et compteurs) ; actions journalisées : export, demande et annulation de suppression de compte, purge, suppression de groupe, transfert de propriété, changement de rôle, consultation des listes et désactivation d'un compte par le superadmin — les connexions ne le sont pas | Traçabilité de sécurité, obligations RGPD (preuve des actions d'export/suppression) | Intérêt légitime | Aucune durée : la table n'est jamais purgée (#138) | Aucun tiers |
| 12 | RGPD (export/suppression) | export à la demande (Art. 20), demande/annulation de suppression (Art. 17) | Exercice des droits RGPD | Obligation légale | L'export n'est pas persisté côté serveur (généré à la demande, retourné directement) | Aucun tiers |

## Droit à l'effacement — modalités de purge

Décrit en détail dans `docs/privacy-policy.md` et implémenté par
`apps/api/src/jobs/account_purge.rs` : à l'expiration du délai de grâce de
30 jours suivant `POST /account/delete`, le job de purge :
1. Supprime les identités OAuth et toutes les sessions de l'utilisateur.
2. Anonymise la ligne `users` (email/nom remplacés, `deleted_at` renseigné).
3. Écrit une entrée `audit_log` pour la purge.

Le job ne supprime ni les jetons de vérification et de réinitialisation, ni
les invitations émises, ni l'état de lecture de la messagerie, ni les
assignations d'événements : ces lignes restent rattachées à l'utilisateur
anonymisé (#138, #139).

Le contenu créé par l'utilisateur au sein des groupes (événements, messages,
etc.) n'est **pas** supprimé — il reste attribué à l'utilisateur anonymisé,
un choix documenté (le contenu appartient fonctionnellement au groupe
familial partagé, pas uniquement à son auteur). Un utilisateur ne peut pas
demander sa suppression tant qu'il est seul propriétaire d'un groupe ayant
d'autres membres (transfert de propriété ou suppression du groupe requis au
préalable) — appliqué dans `apps/api/src/auth/mod.rs::delete_account`.
