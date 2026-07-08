# Politique de confidentialité — Manage Our Home

Dernière mise à jour : 2026-07-08 (Epic #12).

## Qui est responsable de vos données ?

Manage Our Home est un projet auto-hébergé, développé et exploité par
`placeholder_name`, qui porte l'ensemble des rôles RGPD nécessaires
(voir `docs/architecture.md`, "Questions résolues" #3) :

- **Responsable de traitement (data controller)** : responsable du registre
  des traitements, de la présente politique, et de la base légale de chaque
  catégorie de données.
- **Contact vie privée / DPO de fait** : à l'échelle actuelle (déploiement
  familial/home-lab), un contact documenté suffit ; pas de DPO formel requis
  tant que le volume et la nature des traitements ne l'imposent pas
  légalement.
- **Administrateur technique** : responsable de la sécurité applicative, des
  migrations DB, de la rotation des secrets, et de la réponse à incident
  (notification CNIL sous 72h en cas de violation de données).

## Quelles données sont collectées, et pourquoi

Voir `docs/registre-traitements.md` pour le détail complet par catégorie de
donnée (base légale, finalité, durée de conservation, destinataires). En
résumé :

| Catégorie | Exemples | Base légale |
|---|---|---|
| Compte | email, mot de passe (haché), nom affiché | Exécution du contrat (fournir le service) |
| Agenda | événements, tâches, pièces jointes | Exécution du contrat |
| Stocks / recettes / liste de courses | articles, recettes, ingrédients | Exécution du contrat |
| Budget | dépenses saisies manuellement | Exécution du contrat |
| Messagerie | messages du fil familial (chiffrés au repos) | Exécution du contrat |
| Import calendrier Google | URL de flux iCal (chiffrée), événements importés | Consentement explicite (vous fournissez volontairement l'URL) |
| Logs d'audit | actions sensibles (connexion, suppression, export) | Intérêt légitime (sécurité, traçabilité) |

## Avec qui vos données sont-elles partagées ?

Aucun tiers commercial. Le service est self-hosted : aucune donnée n'est
vendue ni partagée à des fins publicitaires. Les seuls flux sortants
possibles sont :

- **Google** (import calendrier) : uniquement si vous configurez
  volontairement un import via une URL de flux iCal privée que vous
  fournissez vous-même — aucun accès n'est initié sans cette action
  explicite de votre part.
- **Fournisseur d'envoi d'email transactionnel** (vérification d'email,
  réinitialisation de mot de passe) : un relais SMTP basé en UE
  (voir `docs/architecture.md`), sous-traitant documenté au registre des
  traitements.

Aucune autre donnée ne quitte le serveur applicatif (les modèles IA de
suggestion de recettes / OCR sont exécutés localement via Ollama, jamais
envoyés à un tiers).

## Combien de temps vos données sont-elles conservées ?

- **Compte actif** : tant que le compte existe.
- **Suppression de compte (droit à l'effacement, Art. 17)** : demandez la
  suppression via `POST /account/delete`. Un délai de grâce de 30 jours
  s'applique (annulable via `POST /account/delete/cancel`), après quoi un
  job de purge anonymise définitivement votre compte (identifiants de
  connexion supprimés, ligne `users` anonymisée). Le contenu que vous avez
  créé au sein d'un groupe familial (messages, événements, etc.) reste
  visible pour les autres membres de ce groupe, mais n'est plus rattaché à
  votre identité — comportement documenté et intentionnel, cohérent avec le
  fonctionnement d'un espace familial partagé.
- Un compte ne peut pas être supprimé tant qu'il est seul propriétaire
  d'un groupe : transférez la propriété (ou supprimez le groupe) au
  préalable.

## Vos droits

- **Droit d'accès et de portabilité (Art. 15/20)** : `GET /account/export`
  retourne l'intégralité des données que vous avez créées, au format JSON.
- **Droit à l'effacement (Art. 17)** : voir ci-dessus.
- **Droit de rectification** : modifiable directement depuis les paramètres
  du compte / du contenu concerné.
- **Contact** : pour toute question ou exercice de droit non couvert par les
  endpoints en libre-service ci-dessus, contactez le responsable de
  traitement (voir en-tête de ce document).

## Sécurité

Les données sensibles (contenu des messages, jetons OAuth, URL de flux
calendrier) sont chiffrées au repos (`pgcrypto`), en plus du chiffrement au
niveau disque. L'isolation entre familles est appliquée au niveau base de
données (Row-Level Security), pas seulement au niveau applicatif. Voir
`docs/architecture.md` pour le détail des mesures techniques (Art. 32).
