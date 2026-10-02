# Analyse d'impact relative à la protection des données — Manage Our Home

AIPD au titre de l'article 35 du RGPD, suivant la trame de la CNIL :
contexte, principes fondamentaux, risques, validation. Rédigée le
2026-10-02 (#143), relue contre `docs/registre-traitements.md` (mis à jour
le 2026-09-28) et le code de `main` à cette date.

**Statut : projet.** L'analyse est rédigée ; la conclusion (dernière
section) est une **proposition**, qui ne vaut décision qu'une fois relue et
signée par le responsable de traitement. Arbitrage du 2026-10-02 : l'AIPD
est menée **avant l'ouverture publique**.

## 1. Faut-il une AIPD ?

La CNIL retient neuf critères, tirés des lignes directrices du CEPD ; un
traitement qui en réunit au moins deux requiert en principe une AIPD.

| Critère | Ici | Pourquoi |
|---|---|---|
| Évaluation ou notation | non | Les suggestions de recettes sont un tri par règles sur le contenu du groupe (`apps/api/src/recipes/suggestions.rs`), pas une évaluation de personnes |
| Décision automatique avec effet juridique | non | Aucune |
| Surveillance systématique | non | Aucune journalisation des connexions, aucune géolocalisation |
| Données sensibles ou à caractère hautement personnel | **oui** | Aucun champ ne les nomme, mais le texte libre (`events.title`, `events.description`, `messages.content`, recettes, liste de courses) et les pièces jointes (PNG, JPEG, WebP, **PDF**, 20 Mio au plus) en recevront : un rendez-vous médical, une ordonnance, un régime révélant une conviction. La messagerie est une correspondance privée |
| Collecte à grande échelle | pas aujourd'hui | Cible de premier déploiement : 10 à 15 familles (`docs/v2-deployment.md`). Le critère se réévalue si le service est ouvert largement |
| Croisement de données | non | Seul l'import iCal fait entrer des données d'ailleurs, à l'initiative d'un membre, sans croisement |
| Personnes vulnérables | **oui** | Des mineurs de 15 à 17 ans peuvent ouvrir un compte (arbitrage du 2026-09-19, #137), et le contenu d'un foyer parle d'enfants plus jeunes |
| Usage innovant | non | Application web classique ; aucun système d'IA |
| Exclusion du bénéfice d'un droit ou d'un contrat | non | Aucune |

**Deux critères sont réunis.** Ni la liste des traitements pour lesquels une
AIPD est requise (délibération CNIL n° 2018-327) ni celle des traitements
qui en sont dispensés (délibération n° 2019-118), consultées le 2026-10-02,
ne nomment un type de traitement qui corresponde à celui-ci ; la seconde
rappelle qu'en cas de doute, l'AIPD est recommandée.

## 2. Contexte

**Le traitement.** Une application de gestion du foyer — agenda partagé,
stocks, recettes, liste de courses, budget, messagerie — organisée en
groupes familiaux étanches les uns aux autres. Le détail des données, des
finalités, des bases légales, des durées et des destinataires, catégorie par
catégorie, est dans `docs/registre-traitements.md` ; cette analyse ne le
recopie pas.

**Le responsable de traitement** est une personne physique, seule à
développer et à exploiter le service, sans délégué à la protection des
données. Il n'est pas requis (art. 37(1)) : le responsable n'est pas un
organisme public, ses activités de base ne consistent ni en un suivi
régulier et systématique des personnes à grande échelle, ni en un
traitement **à grande échelle** de catégories particulières de données —
celles qui arrivent ici le font par la bande, pas comme objet du service,
et le service n'est pas à grande échelle (section 1).

**Les supports.**
- Un serveur exploité par le responsable : `api`, `web`, Postgres et MinIO
  (pièces jointes) dans la pile Docker Compose de `infra/`.
- Mailjet, sous-traitant pour les emails transactionnels (adresse, objet,
  corps).
- Google, pour la connexion avec Google et, le cas échéant, l'hébergement
  d'un flux iCal importé.
- Les services de notification des navigateurs, qui reçoivent un message
  vide à chaque rappel par notification.
- Le navigateur de chaque membre.

## 3. Principes fondamentaux

| Principe | Où il est tenu | Ce qui reste ouvert |
|---|---|---|
| Finalités déterminées | Une par catégorie, registre | — |
| Bases légales | Registre ; exécution du contrat pour l'essentiel | — |
| Minimisation | Pas de date de naissance (déclaration d'âge seule), pas de téléphone, pas d'adresse, pas d'IP ni d'user-agent en base | — |
| Durées de conservation | Purge du compte, purge horaire de conservation (#138, #139, #256) | — |
| Information | `docs/privacy-policy.md`, notice de l'art. 14 dans l'email d'invitation | Identité et contact du responsable encore à renseigner (`docs/v2-deployment.md` #16) |
| Droits d'accès et de portabilité | `GET /account/export` | — |
| Effacement | `POST /account/delete`, purge à 30 jours ; le contenu partagé reste au groupe, anonymisé (CGU, « Vos contenus ») | — |
| Rectification, opposition, limitation | Écrans de compte, ou adresse de contact | Même réserve que l'information |
| Sous-traitance (art. 28) | Mailjet, cadre contractuel du fournisseur | Version opposable à établir (`docs/v2-deployment.md` #18) |
| Transferts hors UE | Aucun voulu | Mécanismes à établir pour Mailjet, Google et les services de notification (#18) |

## 4. Risques

Échelle de la CNIL : négligeable, limitée, importante, maximale. Les
**sources de risque** retenues : un attaquant externe visant le serveur ou
un compte ; un membre d'un groupe qui outrepasse ses droits ; le
responsable lui-même (accès total au serveur et aux clés) ; une erreur
d'exploitation ; un sous-traitant.

### Accès illégitime aux données

**Impacts.** Révélation d'une donnée de santé, d'une correspondance privée,
de l'emploi du temps d'un foyer (absences comprises), de données de
mineurs. **Gravité : importante.**

**Mesures existantes.** Isolation entre familles par la base (Row-Level
Security forcée sur chaque table rattachée à un groupe) ; trois colonnes
chiffrées par `pgcrypto`, chacune avec sa clé (`messages.content`, jeton
de rafraîchissement Google, URL de flux iCal) ; mots de passe hachés
argon2 ; cookie de session `HttpOnly`, `Secure`, `SameSite=Lax`, préfixé
`__Host-` derrière `SECURE_COOKIES` (#224), session de 30 jours au plus et
close après 7 jours sans activité ; jeton de session stocké par sa seule
empreinte SHA-256 (`sessions.token_hash`, #222) ; limitation des
essais de mot de passe ; politique de sécurité du contenu ; `cargo audit`
en CI ; journal d'audit des actions sensibles.

**Ce qui reste exposé.**
- Les pièces jointes et tout le texte libre hors messagerie ne sont pas
  chiffrés au repos par l'application.
- Trois jetons porteurs sont stockés en clair, et une copie de la base
  suffit à s'en servir (celui de session ne l'est plus depuis #222) : un
  jeton d'invitation
  (`invitations.token`, valable 7 jours) fait entrer n'importe quel compte
  dans le groupe, l'adresse invitée n'étant pas vérifiée à l'acceptation ;
  un jeton de réinitialisation (`password_reset_tokens.token`, 1 h) donne
  le compte ; un jeton de vérification (`email_verification_tokens.token`,
  24 h) valide une adresse sans la posséder.
- Les clés de chiffrement sont des variables d'environnement du processus
  `api` : qui prend le serveur prend les clés.
- TLS n'est pas encore configuré pour la production
  (`docs/v2-deployment.md` #2), ni les secrets via sops (#15).

**Vraisemblance : limitée**, une fois TLS en place ; **importante** sans lui.

### Modification non désirée des données

**Impacts.** Un rendez-vous déplacé ou supprimé, un message altéré : des
conséquences pratiques pour le foyer, rarement durables. **Gravité :
limitée.**

**Mesures existantes.** Les mêmes contrôles d'accès ; permissions par rôle
dans le groupe ; journal d'audit des actions d'administration.
**Vraisemblance : limitée.**

### Disparition des données

**Impacts.** Perte de l'agenda, des messages et des pièces jointes d'un
foyer, sans recours. **Gravité : importante.**

**Mesures existantes.** Aucune sauvegarde n'est en place : Postgres et
MinIO chiffrés et restauration éprouvée sont deux items bloquants non faits
(`docs/v2-deployment.md` #8 et #9). **Vraisemblance : importante** tant
qu'ils ne le sont pas.

## 5. Plan d'action

Mesures à mettre en place **avant l'ouverture publique**, déjà portées par
le suivi de déploiement :

1. Sauvegardes chiffrées de Postgres et de MinIO, restauration éprouvée au
   même point (`docs/v2-deployment.md` #8, #9).
2. TLS en production (#2) et secrets via sops (#15).
3. Supervision et journaux consultables (#12, #13), sans quoi une violation
   peut passer inaperçue.
4. Identité du responsable, contact, sous-traitant et transferts (#16, #18).

Mesures **proposées** par cette analyse, à arbitrer :

5. Chiffrer le volume qui porte les données de MinIO et de Postgres sur le
   serveur, ce qui couvre les pièces jointes et le texte libre en cas de vol
   du support (pas en cas d'intrusion sur le serveur en marche).
6. Ne stocker qu'une empreinte de chacun des trois jetons porteurs encore
   en clair (invitation, réinitialisation, vérification), pour qu'une copie
   de la base ne suffise plus à prendre un compte ou une place dans un
   groupe. Le quatrième, celui de session, n'est plus stocké que par son
   empreinte (#222) : une copie de la base ne donne plus de session.

La procédure en cas de violation et le registre des violations sont posés
par la même issue (`docs/procedure-violation.md`,
`docs/registre-violations.md`).

**Avis des personnes concernées** (art. 35(9)) : non recueilli ; le
service n'est pas ouvert.

## 6. Données de connexion — décret n° 2021-1362

Aucune donnée de connexion n'est conservée : ni IP ni user-agent en base ;
l'adresse IP d'une tentative de connexion ne vit qu'en mémoire, le temps de
la limitation des essais (registre, ligne 1d). Le minimum d'un an du décret
n° 2021-1362 a été **écarté** : il vise les hébergeurs et les services de
communication au public, pas une application familiale portée par une
personne physique — arbitrage du responsable de traitement du 2026-09-19
(#138), à revoir si le service change de nature. Il n'appelle donc aucune
mesure ici.

## 7. Conclusion

**Proposition, non signée.** L'AIPD est **requise** (deux critères sur neuf)
et **menée** par le présent document. Les risques résiduels sont acceptables
**à la condition** que les mesures 1 à 4 du plan d'action soient en place
avant l'ouverture publique : sans sauvegarde, la disparition des données
reste d'une gravité et d'une vraisemblance importantes, et sans TLS l'accès
illégitime aussi. Les mesures 5 et 6 réduisent le risque résiduel sans en
être la condition. L'analyse est à reprendre si le service s'ouvre à une
échelle qui rend le critère de grande échelle rempli, s'il ajoute une
fonction qui touche à un autre critère, ou après toute violation.

Décision du responsable de traitement : [conclusion de l'AIPD, date et
signature — à renseigner avant la mise en ligne]
