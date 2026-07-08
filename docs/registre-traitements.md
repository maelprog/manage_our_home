# Registre des traitements — Manage Our Home

Registre tenu au titre de l'article 30 du RGPD. Dernière mise à jour :
2026-07-08 (Epic #12). Un registre des traitements est requis dès qu'un
traitement de données personnelles est effectué, y compris à petite échelle
— voir `docs/architecture.md` §10.

**Responsable de traitement** : `placeholder_name` (voir
`docs/architecture.md`, "Questions résolues" #3, et
`docs/privacy-policy.md`).

**Sous-traitants (destinataires)** :
- Aucun tiers commercial pour le stockage/traitement du contenu applicatif
  (self-hosted : Postgres, MinIO, Ollama tournent sur le serveur exploité
  par le responsable de traitement).
- Fournisseur SMTP transactionnel basé UE (Brevo ou Mailjet) — emails de
  vérification/réinitialisation uniquement, DPA à documenter au moment du
  choix définitif du fournisseur (`docs/architecture.md`).
- Google — uniquement pour l'import calendrier (Epic #9), et uniquement si
  l'utilisateur fournit volontairement une URL de flux iCal privée. Aucun
  compte Google n'est requis pour utiliser le reste du service.

**Mesures de sécurité communes** : chiffrement au repos via `pgcrypto` pour
les colonnes sensibles, isolation multi-tenant appliquée au niveau base de
données (Row-Level Security, `FORCE ROW LEVEL SECURITY` sur chaque table
tenant-scoped), TLS en transit, `cargo audit` en CI, logs d'audit sur les
actions sensibles (`audit_log`).

## Catégories de traitement (une par epic)

| # | Épic | Données traitées | Finalité | Base légale | Durée de conservation | Destinataires |
|---|---|---|---|---|---|---|
| 1 | Auth + Groupes | email, mot de passe (haché bcrypt/argon2), nom affiché, appartenance aux groupes, sessions | Authentification, gestion de compte, isolation familiale | Exécution du contrat | Compte actif + 30j de grâce après demande de suppression, puis anonymisation définitive | Aucun tiers |
| 2 | Agenda | événements, tâches, pièces jointes (photos/documents), rappels | Planification familiale | Exécution du contrat | Tant que l'événement/le compte existe ; supprimé avec le groupe ou anonymisé (`created_by`) à la purge du compte auteur | Aucun tiers |
| 3 | Stocks | articles du garde-manger/frigo, quantités, seuils | Gestion de l'inventaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 4 | Recettes | recettes, ingrédients, historique des repas | Suggestions de repas (algorithme local, pas d'IA tierce) | Exécution du contrat | Idem #2 | Aucun tiers |
| 5 | Liste de courses | articles à acheter, source (manuel/recette/stock bas) | Liste de courses partagée | Exécution du contrat | Idem #2 | Aucun tiers |
| 6 | Budget | dépenses saisies manuellement (montant, nom, date) | Suivi du budget alimentaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7 | Messagerie | contenu des messages (chiffré au repos via `pgcrypto`) | Communication au sein du groupe familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 8 | User admin (superadmin) | liste des groupes/utilisateurs à l'échelle globale, action de désactivation | Support technique/maintenance de la plateforme | Intérêt légitime (exploitation du service) | Durée de vie du compte concerné | Aucun tiers |
| 9 | Import calendrier Google | URL de flux iCal privée (chiffrée), événements importés | Miroir en lecture seule d'un agenda Google externe | Consentement explicite (l'utilisateur fournit volontairement l'URL) | Jusqu'à suppression de l'import par l'utilisateur, ou purge du compte | Google (accès en lecture au flux, initié uniquement par l'utilisateur) |
| — | Logs d'audit (transverse) | horodatage, acteur, action, cible, métadonnées | Traçabilité de sécurité, obligations RGPD (preuve des actions d'export/suppression) | Intérêt légitime | Non défini en v1 (à réévaluer — candidat à une purge périodique si le volume le justifie) | Aucun tiers |
| 12 | RGPD (export/suppression) | export à la demande (Art. 20), demande/annulation de suppression (Art. 17) | Exercice des droits RGPD | Obligation légale | L'export n'est pas persisté côté serveur (généré à la demande, retourné directement) | Aucun tiers |

## Droit à l'effacement — modalités de purge

Décrit en détail dans `docs/privacy-policy.md` et implémenté par
`apps/api/src/jobs/account_purge.rs` : à l'expiration du délai de grâce de
30 jours suivant `POST /account/delete`, le job de purge :
1. Supprime les identités OAuth et sessions actives de l'utilisateur.
2. Anonymise la ligne `users` (email/nom remplacés, `deleted_at` renseigné).
3. Écrit une entrée `audit_log` pour la purge.

Le contenu créé par l'utilisateur au sein des groupes (événements, messages,
etc.) n'est **pas** supprimé — il reste attribué à l'utilisateur anonymisé,
un choix documenté (le contenu appartient fonctionnellement au groupe
familial partagé, pas uniquement à son auteur). Un utilisateur ne peut pas
demander sa suppression tant qu'il est seul propriétaire d'un groupe ayant
d'autres membres (transfert de propriété ou suppression du groupe requis au
préalable) — appliqué dans `apps/api/src/auth/mod.rs::delete_account`.
