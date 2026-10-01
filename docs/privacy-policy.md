# Politique de confidentialité — Manage Our Home

Dernière mise à jour : 2026-09-29.

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
| Âge | la déclaration d'avoir 15 ans ou plus, faite à l'inscription, et sa date — aucune date de naissance n'est demandée ni conservée | Exécution du contrat (les conditions générales réservent le service aux 15 ans et plus) |
| Connexion avec Google | identifiant de votre compte Google, email et nom de votre profil Google, jeton de rafraîchissement délivré par Google (chiffré) | Exécution du contrat (vous choisissez ce mode de connexion) |
| Vérification d'email et réinitialisation du mot de passe | jetons à usage unique envoyés par email, valables 24 h (vérification) ou 1 h (réinitialisation) | Exécution du contrat |
| Protection de la connexion | adresse IP (en IPv6, réduite à son préfixe /64) et email saisi à chaque tentative de connexion par mot de passe, gardés en mémoire du serveur seulement, jamais en base | Intérêt légitime (limiter les essais de mot de passe) |
| Invitations | adresse email de la personne invitée (si le membre qui invite la saisit), lien d'invitation valable 7 jours ; l'email envoyé nomme le groupe et le membre qui invite | Intérêt légitime (permettre à un membre d'inviter un proche dans son groupe) |
| Désactivation d'un compte | date à laquelle l'administrateur du service a désactivé le compte, date de l'email qui prévient de sa suppression ; si vous demandez la réactivation, la date de votre demande et le message facultatif que vous y joignez, et la date d'un refus | Intérêt légitime (exploitation et sécurité du service) |
| Agenda | événements, tâches, pièces jointes, membres assignés à un événement | Exécution du contrat |
| Rappels d'événements | délai choisi avant l'événement ; l'email de rappel porte le titre et la date de l'événement | Exécution du contrat |
| Stocks / recettes / liste de courses | articles, recettes, ingrédients | Exécution du contrat |
| Budget | dépenses saisies manuellement | Exécution du contrat |
| Messagerie | messages du fil familial (chiffrés au repos), date de votre dernière lecture du fil | Exécution du contrat |
| Import calendrier Google | URL de flux iCal (chiffrée), événements importés | Consentement explicite (vous fournissez volontairement l'URL) |
| Logs d'audit | actions sensibles : export, demande, annulation et exécution de la suppression d'un compte, suppression d'un groupe, transfert de propriété, changement de rôle, actions d'administration (les connexions ne sont pas journalisées) | Intérêt légitime (sécurité, traçabilité) |

## Le service et les mineurs

Le service n'est pas ouvert aux moins de 15 ans. C'est le seuil retenu par la
loi française pour qu'un mineur consente seul au traitement de ses données
(art. 8 du RGPD) ; en deçà, il faudrait recueillir l'accord du titulaire de
l'autorité parentale, et le service ne propose pas ce chemin. L'inscription
demande donc de déclarer avoir 15 ans ou plus, et les [conditions générales
d'utilisation](/terms-of-service) en font une condition d'accès.

Cette déclaration n'est pas vérifiée — aucune pièce d'identité, aucune date
de naissance : ce serait collecter bien plus de données que la vérification
n'en justifie. Si l'éditeur apprend qu'un compte a été ouvert par une
personne plus jeune, il le ferme et supprime ses données. Le titulaire de
l'autorité parentale peut le signaler à l'adresse de contact indiquée
ci-dessus.

Rien n'empêche en revanche un membre du foyer d'organiser, depuis son propre
compte, la vie d'enfants plus jeunes : ce sont alors ses données à lui qui
sont traitées, et le contenu qu'il dépose relève de sa responsabilité comme
tout autre contenu.

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
- **Mailjet** (Mailjet SAS, groupe Sinch), le service qui achemine les
  emails du site : vérification d'adresse, réinitialisation de mot de passe,
  invitation à un groupe, rappel d'événement et avertissement avant la
  suppression d'un compte désactivé — les cinq seuls emails que ce service
  envoie. Mailjet reçoit l'adresse du destinataire, l'objet et le
  corps de chacun : le corps d'un rappel reprend le titre et la date de
  l'événement, et le corps d'une invitation nomme le groupe et le membre qui
  invite. L'objet d'un rappel, lui, ne nomme aucun événement : c'est toujours
  « Rappel d'un événement à venir ». Un titre peut être sensible (un examen
  médical, un rendez-vous chez un professionnel), et l'objet est la partie
  d'un email qui s'affiche dans la liste des messages de votre boîte et que
  votre fournisseur de messagerie indexe ; le titre n'est donc que dans le
  corps, qui ne s'affiche qu'à l'ouverture du message — Mailjet et votre
  fournisseur de messagerie le reçoivent tout de même. C'est un
  sous-traitant du responsable de traitement, inscrit à son registre des
  traitements. Le cadre contractuel qui les lie au titre de l'article 28 du
  RGPD sera indiqué ici avant l'ouverture du service : [cadre contractuel du
  sous-traitant email — à renseigner avant la mise en ligne].

Aucune autre donnée ne quitte le serveur applicatif. Les suggestions de
recettes sont calculées sur le serveur par des règles fixes (ingrédients en
stock, repas récents, saison) : aucun modèle d'intelligence artificielle
n'est appelé, ni sur le serveur ni chez un tiers.

## Vos données quittent-elles l'Union européenne ?

Ce service n'est pas encore ouvert au public, et cette page n'affirmera pas
plus que ce qui est établi. Voici où en est chacun des trois cas.

- **Les emails** partent par Mailjet, dont le stockage des données des
  clients européens est situé dans l'Union européenne (centres en Allemagne
  et en Belgique). Cela ne suffit pas à conclure : le groupe auquel Mailjet
  appartient recourt aussi à des prestataires établis hors de l'Union
  européenne, notamment pour son support. Le détail des transferts et, s'il y
  en a, le mécanisme qui les encadre (articles 44 à 49 du RGPD) seront
  indiqués ici avant l'ouverture du service : [transferts hors UE du
  sous-traitant email — à renseigner avant la mise en ligne].
- **Google** reçoit des données dans deux cas seulement : si vous choisissez
  la connexion avec Google, et si un import calendrier est configuré dans
  votre groupe. Son infrastructure est mondiale, donc pour partie hors de
  l'Union européenne. Le mécanisme qui encadre ce transfert sera indiqué ici
  avant l'ouverture du service : [transferts hors UE de Google — à renseigner
  avant la mise en ligne]. Si vous ne vous connectez pas avec Google et
  qu'aucun import calendrier n'est configuré dans votre groupe, rien ne part
  vers Google.
- **Le reste ne quitte pas le serveur du service** : vos messages, votre
  agenda, vos listes, votre budget et vos pièces jointes n'y sont partagés
  avec aucun tiers. L'hébergeur de ce serveur est nommé dans les
  [mentions légales](/legal-notice).

## Combien de temps vos données sont-elles conservées ?

Le serveur n'efface rien à date fixe en dehors des cas indiqués
ci-dessous : quand une ligne dit qu'une donnée « reste », aucune
suppression automatique n'est en place aujourd'hui.

Les suppressions automatiques décrites ci-dessous sont l'œuvre d'un passage
de purge qui a lieu **toutes les heures, et seulement quand le service
fonctionne et que sa configuration permet ce passage**. Une donnée arrivée
au terme de sa durée est donc supprimée au premier passage qui suit : dans
l'heure lorsque ces deux conditions sont réunies, après le redémarrage du
service s'il a été interrompu, et après le rétablissement de sa
configuration si elle a suspendu la purge — le service peut alors
fonctionner sans que rien ne soit supprimé. Les durées ci-dessous sont des
durées de conservation, pas des délais de suppression garantis : nous ne
promettons pas un délai que l'indisponibilité du service ou la suspension
de la purge dépasserait.

- **Compte** (email, mot de passe haché, nom affiché, appartenance aux
  groupes et rôle) : tant que le compte existe, puis 30 jours de grâce
  après une demande de suppression, à l'issue desquels le compte est
  purgé : l'email et le nom sont remplacés, le mot de passe haché est
  effacé, et l'appartenance aux groupes et le rôle sont supprimés (voir
  ci-dessous).
- **Compte désactivé par l'administrateur du service** : la désactivation
  révoque les sessions, sans rien effacer ; l'administrateur peut
  réactiver le compte. Une connexion avec les bons identifiants n'ouvre
  plus qu'une page, qui explique la désactivation et permet d'en demander
  la réactivation — une demande en attente à la fois, avec un message
  facultatif ; avec de mauvais identifiants, le message d'erreur est le
  même que pour tout autre compte. La demande est conservée jusqu'à la
  décision de l'administrateur, réactivation ou refus, qui la supprime, ou
  jusqu'à la purge du compte. Un compte qui reste désactivé
  2 ans — la durée que recommande la CNIL pour un compte inactif — est
  purgé comme ci-dessous. Son titulaire en est prévenu par email
  à l'adresse du compte, 30 jours avant ; la purge n'a jamais lieu moins
  de 30 jours après cet email, et, s'il n'a pas pu partir, pas avant 2 ans
  et 30 jours de désactivation. Une demande de réactivation en attente
  suspend cette échéance et l'email qui la précède, si c'est la première
  depuis la désactivation. Après un refus, vous pouvez en faire une
  nouvelle, mais elle ne suspend plus rien ; et si l'email
  d'avertissement était déjà parti, un nouvel email part une fois
  l'échéance à 30 jours ou moins, et la purge n'a pas lieu moins de 30
  jours après lui. Dans tous les cas, elle n'a pas lieu moins de 30 jours
  après le refus, que cet email parte ou non. La date du refus est effacée à la réactivation, à une
  nouvelle désactivation et à la purge. Un compte dont la suppression avait été demandée
  avant sa désactivation est purgé au terme de ses 30 jours de grâce,
  comme tout autre, demande de réactivation en attente ou non.
- **Déclaration d'âge** : la déclaration et sa date restent tant que le
  compte existe, et sont effacées à la purge du compte.
- **Sessions de connexion** : une session vaut 30 jours au plus, et prend
  fin plus tôt si elle reste 7 jours sans activité. Sa trace (dates
  de création, de dernière activité, d'expiration et de révocation) est
  supprimée au premier passage de purge qui suit la fin de la session :
  expiration, inactivité, déconnexion, changement de mot de passe ou
  désactivation du compte. La session qu'ouvre ensuite la connexion à un
  compte désactivé, limitée à la page de demande de réactivation, prend
  aussi fin à la réactivation du compte.
- **Connexion avec Google** : l'identifiant, l'email et le nom de votre
  profil Google et le jeton de rafraîchissement sont conservés jusqu'à
  l'anonymisation du compte, qui les supprime.
- **Vérification d'email et réinitialisation du mot de passe** : un jeton
  de vérification est valable 24 heures, un jeton de réinitialisation
  1 heure, et chacun ne sert qu'une fois. Un jeton de réinitialisation est
  supprimé dès qu'il sert. Sinon, un jeton est supprimé au premier passage
  de purge qui suit sa durée de conservation : 48 heures après sa création
  pour la vérification, 1 heure pour la réinitialisation — ou à la purge du
  compte, si elle vient avant.
- **Invitations** : un lien d'invitation est valable 7 jours et ne sert
  qu'une fois. L'invitation, adresse email de la personne invitée comprise,
  est supprimée dès qu'elle est acceptée ; sinon, au premier passage de
  purge qui suit les 30 jours de sa création, ou plus tôt si le groupe est
  supprimé ou si le compte du membre qui l'a envoyée est purgé.
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
  l'assignation n'est pas retirée ; elle reste après votre départ du groupe,
  et est supprimée à la purge de votre compte.
- **Date de dernière lecture de la messagerie** : tant que le groupe
  existe ; elle reste après votre départ du groupe, et est supprimée à la
  purge de votre compte.
- **Import calendrier** : l'URL du flux et les événements importés restent
  jusqu'à la suppression de l'import par un administrateur ou le
  propriétaire du groupe, ou jusqu'à la suppression du groupe. Les
  événements importés restent après la suppression de l'import, sauf si
  leur suppression est demandée en même temps. La purge du compte du
  membre qui a configuré l'import supprime l'import, URL du flux comprise ;
  les événements importés restent dans le groupe.
- **Logs d'audit** : 6 mois glissants, durée recommandée par la CNIL pour
  les journaux ; une entrée plus ancienne est supprimée au premier passage
  de purge qui suit cette échéance. La purge de votre compte supprime
  aussitôt les entrées de vos propres actions ; celles qui concernent
  votre compte sans être de votre fait restent jusqu'à ce terme (voir
  « Suppression de votre compte » ci-dessous).
- **Export de vos données** : généré à la demande et renvoyé directement,
  il n'est pas conservé sur le serveur.

### Suppression de votre compte

- **Suppression de compte (droit à l'effacement, Art. 17)** : demandez la
  suppression via `POST /account/delete`. Un délai de grâce de 30 jours
  s'applique (annulable via `POST /account/delete/cancel`), après quoi
  votre compte est purgé au premier passage de purge qui suit.
- **Ce que la purge supprime** : vos identifiants de connexion (mot de
  passe haché, connexion avec Google), vos sessions, vos jetons de
  vérification et de réinitialisation, votre appartenance aux groupes et
  votre rôle, votre date de dernière lecture de la messagerie, vos
  assignations d'événements, les entrées des logs d'audit dont vous êtes
  l'auteur, les invitations que vous avez envoyées (adresses des personnes
  invitées comprises) et les imports calendrier que vous avez configurés
  (URL du flux comprise). Aucun moyen de connexion ne subsiste : ni mot de
  passe, ni connexion avec Google, ni session, et un jeton de vérification
  ou de réinitialisation qui viserait encore le compte est refusé. Se
  connecter ensuite avec le même compte Google crée un nouveau compte, sans
  lien avec l'ancien. Votre email et votre nom sont remplacés, votre
  déclaration d'âge est effacée : il ne reste de votre compte qu'un
  identifiant technique rattaché à aucune identité.
- **Ce qu'elle laisse en place** : le contenu que vous avez créé au sein
  d'un groupe familial — événements et pièces jointes, tâches cochées,
  messages, stocks, recettes et historique des repas, liste de courses,
  budget, et les groupes que vous avez créés — reste visible pour les
  autres membres de ce groupe, mais n'est plus rattaché à votre identité.
  C'est un choix intentionnel, cohérent avec le fonctionnement d'un espace
  familial partagé, et un engagement contractuel autant qu'une
  description : il figure aussi dans les [conditions générales
  d'utilisation](/terms-of-service).
- **Ce qui subsiste un temps** : les entrées des logs d'audit qui
  concernent votre compte sans être de votre fait — celle qui date la
  purge, celles des transferts de propriété de vos groupes qu'elle opère,
  celles où un autre membre ou l'administrateur du service a agi sur
  votre compte (changement de rôle, transfert de propriété, désactivation,
  réactivation, refus d'une demande de réactivation)
  et celles d'un transfert qui vous a désigné comme successeur à la purge
  d'un autre compte — jusqu'au premier passage de purge qui suit leurs
  6 mois ; elles ne désignent plus qu'un identifiant technique. Et une
  invitation qu'un membre aurait envoyée à votre adresse, jusqu'à son
  acceptation, à la suppression du groupe ou au premier passage de purge
  qui suit les 30 jours de son envoi (voir « Invitations » plus haut).
- **Quand elle a lieu** : la purge passe toutes les heures, aux mêmes
  conditions que les suppressions automatiques décrites plus haut — quand
  le service fonctionne et que sa configuration permet ce passage. Elle a
  donc lieu au premier passage qui suit la fin du délai de grâce, pas à
  une date garantie.
- Un compte ne peut pas être supprimé tant qu'il est propriétaire d'un
  groupe : transférez la propriété (ou supprimez le groupe) au préalable.
  Si vous devenez propriétaire d'un groupe pendant le délai de grâce, la
  purge transfère la propriété du groupe, dans cet ordre : à son
  administrateur le plus ancien, à défaut à son membre le plus ancien,
  puis, si tous les autres membres ont eux-mêmes demandé leur
  suppression, à l'administrateur puis au membre le plus ancien parmi eux.
  Un compte désactivé par l'administrateur du service n'en hérite jamais :
  s'il ne reste que de tels comptes, le groupe reste sans propriétaire.
  Si vous en étiez le seul membre, il reste, avec son contenu, sans
  membre.

## Vos droits

- **Droit d'accès et de portabilité (Art. 15/20)** : `GET /account/export`
  retourne, au format JSON, l'intégralité des données qui vous concernent :
  votre profil ; le contenu que vous avez créé dans chaque groupe (les
  recettes avec leurs ingrédients), y compris un groupe que vous avez
  quitté ou dont vous avez été retiré, tant que ce contenu y est conservé ;
  les événements qui vous sont assignés, y compris par un autre membre ;
  les rappels de vos événements et les envois qu'ils ont programmés ; vos
  pièces jointes ; les occurrences de tâches que vous avez cochées ; votre
  marqueur de lecture de la messagerie ; les invitations que vous avez
  émises et celles qu'un membre a adressées à votre adresse email (groupe,
  expéditeur, dates), tant qu'elles sont conservées — en attente, ou
  expirées et pas encore effacées (une invitation acceptée est supprimée,
  les autres le sont 30 jours après leur envoi) ; vos sessions ; votre
  identité Google liée ; vos vérifications d'adresse et réinitialisations
  de mot de passe ; les entrées du journal d'audit de vos propres actions
  et de celles qui visent votre compte, sans l'identité de leur auteur. Les
  pièces jointes y figurent par leurs métadonnées (nom, type, taille, date)
  et un lien de téléchargement valable 5 minutes : relancez l'export pour
  en obtenir un nouveau. N'y figurent ni le contenu créé par les autres
  membres, ni les secrets d'accès (identifiant de session, jeton
  d'invitation ou de vérification, jeton Google, URL de flux calendrier).
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
l'invitation pendant 30 jours après son envoi, puis est effacée au premier
passage de purge qui suit (ces passages ont lieu toutes les heures quand le
service fonctionne et que sa configuration les permet, et reprennent à son
redémarrage ou au rétablissement de cette configuration). Elle l'est
aussitôt si le lien est utilisé.

Aucun écran de ce service ne permet d'agir sur cette adresse. Si vous
ouvrez un compte avec cette même adresse, l'export de ce compte contient
les invitations qui lui restent adressées (une invitation acceptée est
supprimée) ; mais la suppression du compte ne retire pas l'adresse d'une
invitation. Pour accéder à votre adresse sans compte, la faire rectifier ou
effacer, ou vous opposer à son traitement, adressez la demande au
responsable de traitement.

## Sécurité

Les données sensibles (contenu des messages, jetons OAuth, URL de flux
calendrier) sont chiffrées dans la base de données (`pgcrypto`) ; les mots
de passe n'y sont conservés que hachés. L'isolation entre familles est
appliquée au niveau base de données (Row-Level Security), pas seulement au
niveau applicatif.
