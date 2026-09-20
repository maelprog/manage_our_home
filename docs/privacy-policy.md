# Politique de confidentialité — Manage Our Home

Dernière mise à jour : 2026-09-20.

## Qui est responsable de vos données ?

Manage Our Home est un projet auto-hébergé, développé et exploité par une
personne physique, [nom du responsable de traitement — à renseigner avant la
mise en ligne], qui porte l'ensemble des rôles RGPD nécessaires :

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

**Pour le joindre** : [adresse de contact — à renseigner avant la mise en
ligne]. C'est l'adresse à laquelle adresser toute demande d'exercice de vos
droits que les écrans en libre-service ne couvrent pas — rectification hors
interface, opposition, limitation, directives après décès — ainsi que toute
question sur la présente politique.

## Quelles données sont collectées, et pourquoi

Le tableau ci-dessous donne, pour chaque catégorie de données, ce qui est
collecté et la base légale du traitement. La durée de conservation de
chaque catégorie est détaillée plus bas, dans « Combien de temps vos
données sont-elles conservées ? ».

| Catégorie | Exemples | Base légale |
|---|---|---|
| Compte | email, mot de passe (haché), nom affiché | Exécution du contrat (fournir le service) |
| Connexion avec Google | identifiant de votre compte Google, email et nom de votre profil Google, jeton de rafraîchissement délivré par Google (chiffré) | Exécution du contrat (vous choisissez ce mode de connexion) |
| Vérification d'email et réinitialisation du mot de passe | jetons à usage unique, valables 24 h, envoyés par email | Exécution du contrat |
| Protection de la connexion | adresse IP (en IPv6, réduite à son préfixe /64) et email saisi à chaque tentative de connexion par mot de passe, gardés en mémoire du serveur seulement, jamais en base | Intérêt légitime (limiter les essais de mot de passe) |
| Invitations | adresse email de la personne invitée (si le membre qui invite la saisit), lien d'invitation valable 7 jours ; l'email envoyé nomme le groupe et le membre qui invite | Intérêt légitime (permettre à un membre d'inviter un proche dans son groupe) |
| Agenda | événements, tâches, pièces jointes, membres assignés à un événement | Exécution du contrat |
| Rappels d'événements | délai choisi avant l'événement ; l'email de rappel porte le titre et la date de l'événement | Exécution du contrat |
| Stocks / recettes / liste de courses | articles, recettes, ingrédients | Exécution du contrat |
| Budget | dépenses saisies manuellement | Exécution du contrat |
| Messagerie | messages du fil familial (chiffrés au repos), date de votre dernière lecture du fil | Exécution du contrat |
| Import calendrier Google | URL de flux iCal (chiffrée), événements importés | Consentement explicite (vous fournissez volontairement l'URL) |
| Logs d'audit | actions sensibles : export, demande, annulation et exécution de la suppression d'un compte, suppression d'un groupe, transfert de propriété, changement de rôle, actions d'administration (les connexions ne sont pas journalisées) | Intérêt légitime (sécurité, traçabilité) |

## Avec qui vos données sont-elles partagées ?

Aucun tiers commercial. Le service est self-hosted : aucune donnée n'est
vendue ni partagée à des fins publicitaires. Les seuls flux sortants
possibles sont :

- **Google** (connexion) : uniquement si vous choisissez « Se connecter
  avec Google ». Votre navigateur passe alors par Google, puis le serveur
  échange auprès de Google le code d'autorisation obtenu et lit votre
  profil (identifiant, email, nom). Google sait donc que vous vous
  connectez à ce service.
- **Hébergeur du flux calendrier** (import calendrier, en pratique Google
  Agenda) : uniquement si un administrateur ou le propriétaire du groupe
  configure un import avec une URL de flux iCal privée fournie
  volontairement. Le serveur télécharge le flux à cette URL chaque fois
  qu'un membre du groupe lance un import ; aucun import ne tourne en
  arrière-plan.
- **Fournisseur d'envoi d'email transactionnel** (vérification d'email,
  réinitialisation de mot de passe, invitation à un groupe, rappel
  d'événement) : un relais SMTP basé dans l'Union européenne, sous-traitant
  du responsable de traitement, inscrit à son registre des traitements.

Aucune autre donnée ne quitte le serveur applicatif. Les suggestions de
recettes sont calculées sur le serveur par des règles fixes (ingrédients en
stock, repas récents, saison) : aucun modèle d'intelligence artificielle
n'est appelé, ni sur le serveur ni chez un tiers.

## Combien de temps vos données sont-elles conservées ?

Le serveur n'efface rien à date fixe en dehors des cas indiqués
ci-dessous : quand une ligne dit qu'une donnée « reste », aucune
suppression automatique n'est en place aujourd'hui.

- **Compte** (email, mot de passe haché, nom affiché, appartenance aux
  groupes et rôle) : tant que le compte existe, puis 30 jours de grâce
  après une demande de suppression, à l'issue desquels le compte est
  anonymisé (voir ci-dessous).
- **Sessions de connexion** : une session vaut 30 jours au plus, et prend
  fin plus tôt si elle reste 7 jours sans activité. Sa trace (dates
  de création, de dernière activité, d'expiration et de révocation) reste
  après l'expiration ou la déconnexion, jusqu'à l'anonymisation du compte.
- **Connexion avec Google** : l'identifiant, l'email et le nom de votre
  profil Google et le jeton de rafraîchissement sont conservés jusqu'à
  l'anonymisation du compte, qui les supprime.
- **Vérification d'email et réinitialisation du mot de passe** : un jeton
  est valable 24 heures et ne sert qu'une fois. Sa trace (jeton, compte
  concerné, dates) reste ensuite en base sans limite de durée, y compris
  après l'anonymisation du compte.
- **Invitations** : un lien d'invitation est valable 7 jours et ne sert
  qu'une fois. L'invitation, adresse email de la personne invitée comprise,
  reste ensuite jusqu'à la suppression du groupe.
- **Protection de la connexion** : gardée en mémoire du serveur, jamais en
  base, et perdue à chaque redémarrage du serveur. Une connexion réussie
  efface aussitôt les tentatives du même couple (adresse, email). Sinon,
  les tentatives cessent de compter au bout de 15 minutes (ou à la fin d'un
  blocage de 15 minutes), mais rien ne les efface à heure fixe : elles ne
  sont retirées de la mémoire qu'à l'arrivée d'une tentative avec un autre
  email depuis la même adresse, quand le serveur suit déjà 10 000 couples,
  ou au redémarrage. Sans nouvelle tentative, elles restent donc en mémoire
  jusqu'au redémarrage du serveur.
- **Agenda, stocks, recettes, liste de courses, budget, messagerie** : tant
  que le contenu n'est pas supprimé, et au plus tant que le groupe existe —
  la suppression d'un groupe supprime son contenu. Quand le compte de son
  auteur est anonymisé, le contenu reste dans le groupe sans être rattaché
  à son identité. Le fichier d'une pièce jointe que le serveur a reçu sans
  pouvoir l'enregistrer est supprimé par un balayage quotidien moins de
  48 heures après son dépôt, tant que le serveur tourne et que ce balayage
  est activé dans sa configuration.
- **Rappels d'événements** : jusqu'à la suppression du rappel ou de
  l'événement ; l'historique des envois (heure, statut, tentatives) part
  avec eux.
- **Assignations d'événements** : tant que l'événement existe et que
  l'assignation n'est pas retirée ; elle reste après votre départ du groupe
  et après l'anonymisation de votre compte.
- **Date de dernière lecture de la messagerie** : tant que le groupe
  existe ; elle reste après votre départ du groupe et après
  l'anonymisation de votre compte.
- **Import calendrier** : l'URL du flux et les événements importés restent
  jusqu'à la suppression de l'import par un administrateur ou le
  propriétaire du groupe, ou jusqu'à la suppression du groupe. Les
  événements importés restent après la suppression de l'import, sauf si
  leur suppression est demandée en même temps. L'anonymisation d'un compte
  ne supprime ni l'import ni ses événements.
- **Logs d'audit** : sans limite de durée, le journal n'est jamais purgé.
- **Export de vos données** : généré à la demande et renvoyé directement,
  il n'est pas conservé sur le serveur.

### Suppression de votre compte

- **Suppression de compte (droit à l'effacement, Art. 17)** : demandez la
  suppression via `POST /account/delete`. Un délai de grâce de 30 jours
  s'applique (annulable via `POST /account/delete/cancel`), après quoi un
  job de purge anonymise définitivement votre compte (identifiants de
  connexion et sessions supprimés, email et nom remplacés). Le contenu que
  vous avez créé au sein d'un groupe familial (messages, événements, etc.)
  reste visible pour les autres membres de ce groupe, mais n'est plus
  rattaché à votre identité — comportement documenté et intentionnel,
  cohérent avec le fonctionnement d'un espace familial partagé. C'est un
  engagement contractuel autant qu'une description : il figure aussi dans
  les [conditions générales d'utilisation](/terms-of-service).
- L'anonymisation ne supprime pas les traces de jetons de vérification et
  de réinitialisation, les invitations que vous avez émises, votre date de
  dernière lecture de la messagerie ni vos assignations d'événements :
  elles restent rattachées au compte anonymisé.
- Un compte ne peut pas être supprimé tant qu'il est seul propriétaire
  d'un groupe : transférez la propriété (ou supprimez le groupe) au
  préalable.

## Vos droits

- **Droit d'accès et de portabilité (Art. 15/20)** : `GET /account/export`
  retourne l'intégralité des données que vous avez créées, au format JSON.
- **Droit à l'effacement (Art. 17)** : voir « Suppression de votre
  compte » ci-dessus.
- **Droit de rectification** : modifiable directement depuis les paramètres
  du compte / du contenu concerné.
- **Droit à la limitation (Art. 18)** : vous pouvez demander que vos
  données soient conservées sans être utilisées, notamment le temps de
  vérifier leur exactitude ou le bien-fondé d'une opposition, ou plutôt que
  d'être effacées. Aucun écran ne le propose : adressez la demande au
  responsable de traitement.
- **Droit d'opposition (Art. 21)** : vous pouvez vous opposer, pour des
  raisons tenant à votre situation particulière, aux traitements fondés sur
  l'intérêt légitime (invitations, protection de la connexion, logs
  d'audit). Le traitement cesse, sauf motifs légitimes et impérieux qui
  prévalent sur vos intérêts, ou nécessité pour la constatation,
  l'exercice ou la défense de droits en justice. Adressez la demande au
  responsable de traitement.
- **Retrait du consentement (Art. 7)** : l'import calendrier repose sur le
  consentement de la personne qui fournit l'URL du flux. Un administrateur
  ou le propriétaire du groupe peut supprimer l'import à tout moment ; le
  retrait ne remet pas en cause les imports déjà faits.
- **Directives après le décès** (art. 85 de la loi Informatique et
  Libertés) : vous pouvez définir des directives sur la conservation,
  l'effacement et la communication de vos données après votre décès, et
  les adresser au responsable de traitement.
- **Réclamation auprès de la CNIL (Art. 77)** : si vous estimez que le
  traitement de vos données ne respecte pas la réglementation, vous pouvez
  adresser une réclamation à la Commission nationale de l'informatique et
  des libertés : [cnil.fr/fr/adresser-une-plainte](https://www.cnil.fr/fr/adresser-une-plainte).
- **Contact** : pour toute question ou exercice de droit non couvert par les
  endpoints en libre-service ci-dessus, écrivez au responsable de traitement
  à l'adresse donnée en en-tête de ce document (« Pour le joindre »).

## Si vous avez reçu une invitation sans avoir de compte

Un membre d'un groupe peut saisir votre adresse email pour vous inviter.
Vos données ne sont alors pas collectées auprès de vous : l'email
d'invitation porte donc lui-même l'information exigée par l'article 14 du
RGPD — qui est responsable du traitement et comment le joindre, quel membre
a communiqué votre adresse, pourquoi elle est traitée, sur quelle base
légale, combien de temps elle est conservée, un lien vers la présente
politique et le droit de saisir la CNIL.

Ignorer cet email suffit à ne pas rejoindre le groupe : le lien cesse de
fonctionner au bout de 7 jours. Votre adresse, elle, reste enregistrée avec
l'invitation jusqu'à la suppression du groupe.

Aucun écran de ce service ne permet d'agir sur cette adresse. Créer un
compte depuis le lien reçu ouvre des droits sur les données de ce compte,
pas sur l'invitation : la suppression d'un compte ne retire pas l'adresse
d'une invitation, et l'export de compte ne la contient pas. Pour accéder à
votre adresse, la faire rectifier ou effacer, ou vous opposer à son
traitement, adressez la demande au responsable de traitement.

## Sécurité

Les données sensibles (contenu des messages, jetons OAuth, URL de flux
calendrier) sont chiffrées dans la base de données (`pgcrypto`) ; les mots
de passe n'y sont conservés que hachés. L'isolation entre familles est
appliquée au niveau base de données (Row-Level Security), pas seulement au
niveau applicatif.
