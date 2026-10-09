# Politique de confidentialité — Manage Our Home

Dernière mise à jour : 2026-10-09.

## Qui est responsable de vos données ?

Manage Our Home est un projet auto-hébergé, développé et exploité par une
personne physique, [nom du responsable de traitement — à renseigner avant la
mise en ligne], qui porte l'ensemble des rôles RGPD nécessaires :

- **Responsable de traitement (data controller)** : responsable du registre
  des traitements, de la présente politique, et de la base légale de chaque
  catégorie de données ; en cas de violation de données, c'est lui qui la
  constate, la qualifie, et décide de sa notification à la CNIL, dans les
  72 heures qui suivent sa prise de connaissance.
- **Contact vie privée / DPO de fait** : à l'échelle actuelle (déploiement
  familial/home-lab), un contact documenté suffit ; pas de DPO formel requis
  tant que le volume et la nature des traitements ne l'imposent pas
  légalement.
- **Administrateur technique** : responsable de la sécurité applicative, des
  migrations DB, de la rotation des secrets, et de la réponse technique à
  incident.

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
| Âge | la déclaration d'avoir 15 ans ou plus, faite à l'inscription (à la première connexion pour un compte ouvert avec Google), et sa date — aucune date de naissance n'est demandée ni conservée | Exécution du contrat (les conditions générales réservent le service aux 15 ans et plus) |
| Conditions d'utilisation | la version des conditions générales que vous avez acceptée — à l'inscription, ou à la première connexion pour un compte ouvert avec Google — ou dont vous avez indiqué avoir pris connaissance après un changement, et sa date | Exécution du contrat (garder trace des conditions qui régissent votre usage du service) |
| Connexion avec Google | identifiant de votre compte Google, email et nom de votre profil Google, jeton de rafraîchissement délivré par Google (chiffré) | Exécution du contrat (vous choisissez ce mode de connexion) |
| Vérification d'email et réinitialisation du mot de passe | jetons à usage unique envoyés par email, valables 24 h (vérification) ou 1 h (réinitialisation) | Exécution du contrat |
| Protection de la connexion | adresse IP (en IPv6, réduite à son préfixe /64) et email saisi à chaque tentative de connexion par mot de passe, gardés en mémoire du serveur seulement, jamais en base | Intérêt légitime (limiter les essais de mot de passe) |
| Invitations | adresse email de la personne invitée (si le membre qui invite la saisit), lien d'invitation valable 7 jours ; l'email envoyé nomme le groupe et le membre qui invite | Intérêt légitime (permettre à un membre d'inviter un proche dans son groupe) |
| Désactivation d'un compte | date à laquelle l'administrateur du service a désactivé le compte, date de l'email qui prévient de sa suppression ; si vous demandez la réactivation, la date de votre demande et le message facultatif que vous y joignez, et la date du premier refus | Intérêt légitime (exploitation et sécurité du service) |
| Agenda | événements, tâches, pièces jointes, membres assignés à un événement | Exécution du contrat |
| Rappels d'événements | délai choisi avant l'événement ; la façon dont vos rappels vous parviennent (notification, email ou les deux) ; pour chaque appareil où vous activez les notifications, l'adresse d'abonnement que le service de notification de votre navigateur lui attribue et ses dates d'abonnement, de dernier renouvellement et de dernier envoi réussi, et le nombre et la date de début de ses envois échoués d'affilée (50 appareils au plus par compte). L'email de rappel porte le titre et la date de l'événement ; la notification n'affiche que « Rappel d'un événement à venir » | Exécution du contrat |
| Stocks / recettes / liste de courses | articles (et leur code-barres quand il a été scanné ou saisi), recettes (et l'adresse de la page d'où une recette a été importée), ingrédients ; la photo prise pour scanner un code-barres, lue puis oubliée (voir ci-dessous) | Exécution du contrat |
| Budget | dépenses saisies manuellement | Exécution du contrat |
| Messagerie | messages du fil familial (chiffrés au repos), date de votre dernière lecture du fil | Exécution du contrat |
| Import calendrier Google | URL de flux iCal (chiffrée), événements importés | Consentement explicite (vous fournissez volontairement l'URL) |
| Logs d'audit | actions sensibles : export, demande, annulation et exécution de la suppression d'un compte, suppression d'un groupe, transfert de propriété, changement de rôle, actions d'administration (les connexions ne sont pas journalisées) | Intérêt légitime (sécurité, traçabilité) |

## Le service et les mineurs

Le service n'est pas ouvert aux moins de 15 ans. C'est le seuil retenu par la
loi française pour qu'un mineur consente seul au traitement de ses données
(art. 8 du RGPD) ; en deçà, il faudrait recueillir l'accord du titulaire de
l'autorité parentale, et le service ne propose pas ce chemin. L'inscription
demande donc de déclarer avoir 15 ans ou plus — un compte ouvert avec Google
fait cette déclaration à sa première connexion, avant tout accès au service —,
et les [conditions générales d'utilisation](/terms-of-service) en font une
condition d'accès.

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
- **Scaleway** (Scaleway SAS, Paris), dont le service d'emails
  transactionnels achemine les emails du site : vérification d'adresse, réinitialisation de mot de passe,
  invitation à un groupe, rappel d'événement, avertissement avant la
  suppression d'un compte désactivé et avis de la propriété d'un groupe
  qui vous revient sans que vous l'ayez demandée — les six seuls emails que
  ce service envoie. Scaleway reçoit l'adresse du destinataire, l'objet et le
  corps de chacun : le corps d'un rappel reprend le titre et la date de
  l'événement, le corps d'une invitation nomme le groupe et le membre qui
  invite, et le corps d'un avis de propriété nomme le groupe et dit pourquoi
  il vous revient. L'objet d'un rappel, lui, ne nomme aucun événement : c'est toujours
  « Rappel d'un événement à venir ». Un titre peut être sensible (un examen
  médical, un rendez-vous chez un professionnel) : il ne figure donc pas
  dans l'objet, la ligne que la liste des messages affiche en premier. Il
  reste en revanche dans le corps, au début de sa première ligne, si bien
  que l'aperçu que la plupart des messageries et des notifications
  affichent sous l'objet le montre ; et Scaleway comme votre fournisseur de
  messagerie le reçoivent avec le corps. Scaleway est un
  sous-traitant du responsable de traitement, inscrit à son registre des
  traitements. Le cadre contractuel qui les lie au titre de l'article 28 du
  RGPD sera indiqué ici avant l'ouverture du service : [cadre contractuel du
  sous-traitant email — à renseigner avant la mise en ligne].
- **Le service de notification de votre navigateur**, uniquement si vos
  rappels vous parviennent par notification et que vous les avez activées
  sur un appareil : celui de Google pour Chrome et la plupart des
  navigateurs qui en dérivent, de Mozilla pour Firefox, d'Apple pour
  Safari, de Microsoft pour Edge. Ce n'est pas le service qui le choisit,
  c'est votre navigateur. À chaque rappel, le serveur lui envoie un message
  **vide** à l'adresse d'abonnement de votre appareil : ni le titre de
  l'événement, ni sa date, ni aucun identifiant de l'événement ou de votre
  compte. Ce service de notification apprend donc que ce serveur a envoyé
  un message à cet appareil, à cette heure. Votre appareil affiche alors
  toujours le même texte, « Rappel d'un événement à venir » — rien sur
  l'écran verrouillé ne dit de quel événement il s'agit — et le titre ne se
  lit qu'en ouvrant l'agenda. Pas de repli : sans appareil abonné, un
  rappel par notification n'est pas envoyé du tout, et le site vous en
  avertit là où vous programmez vos rappels.
- **Open Food Facts**, base publique de produits alimentaires, quand vous
  scannez ou saisissez le code-barres d'un article de stock : le serveur lui
  demande la fiche du produit, avec ce code pour seule information — ni
  votre adresse IP, ni cookie, ni identifiant de votre compte ou de votre
  famille, ni la photo du code si vous l'avez photographié. Open Food Facts
  voit l'adresse du serveur, pas la vôtre, et ne
  peut pas savoir qui a scanné quoi. Le code d'une étiquette de pesée du
  magasin ne lui est pas envoyé, ni celui d'un article que votre famille a
  déjà en stock. Ce n'est pas une donnée personnelle qui sort : ce flux est
  cité ici pour être complet.
- **Le site d'une recette que vous importez**, quand vous collez l'adresse
  d'une page de recette : le serveur télécharge cette page, une fois, au
  moment de l'import — sans votre adresse IP, sans cookie, sans identifiant
  de votre compte ou de votre famille. Le site voit l'adresse du serveur,
  pas la vôtre. Le serveur ne garde rien de la page : la recette qu'il y lit
  vous est proposée pour relecture, et seule l'adresse de la page est
  conservée, avec la recette, si vous l'enregistrez. Ce flux est cité ici
  pour être complet.

Aucune autre donnée ne quitte le serveur applicatif. Les suggestions de
recettes sont calculées sur le serveur par des règles fixes (ingrédients en
stock, repas récents, saison) : aucun modèle d'intelligence artificielle
n'est appelé, ni sur le serveur ni chez un tiers.

## Vos données quittent-elles l'Union européenne ?

Ce service n'est pas encore ouvert au public, et cette page n'affirmera pas
plus que ce qui est établi. Voici où en est chacun des quatre cas.

- **Le relais des emails reste dans l'Union européenne.** Les emails
  partent par Scaleway, qui déclare héberger et traiter l'ensemble des
  données de son service d'emails transactionnels dans l'Union européenne,
  sans recourir à un sous-traitant établi hors de l'Union pour ce service
  (déclaration consultée le 4 octobre 2026). La même déclaration précise
  que, pour l'ensemble de ses services, Scaleway peut, dans de rares cas
  exceptionnels, travailler avec des partenaires américains ou canadiens,
  sous un mécanisme d'adéquation reconnu (elle cite le cadre de protection
  des données UE–États-Unis) et sous les clauses contractuelles types de
  son accord de traitement des données. Cette garantie ne couvre que le
  relais : une fois l'email remis à votre fournisseur de messagerie, il est
  conservé là où celui-ci héberge votre boîte, qui peut se trouver hors de
  l'Union européenne.
- **Google** reçoit des données dans deux cas seulement : si vous choisissez
  la connexion avec Google, et si un import calendrier est configuré dans
  votre groupe. Son infrastructure est mondiale, donc pour partie hors de
  l'Union européenne. Le mécanisme qui encadre ce transfert sera indiqué ici
  avant l'ouverture du service : [transferts hors UE de Google — à renseigner
  avant la mise en ligne]. Si vous ne vous connectez pas avec Google et
  qu'aucun import calendrier n'est configuré dans votre groupe, rien ne part
  vers Google.
- **Les notifications de rappel** passent par le service de notification
  de votre navigateur (Google, Mozilla, Apple ou Microsoft), dont
  l'infrastructure est pour partie hors de l'Union européenne. Ce qu'il
  reçoit se limite à l'adresse d'abonnement de votre appareil et à l'heure
  d'un message vide (voir plus haut). Le mécanisme qui encadre ce transfert
  sera indiqué ici avant l'ouverture du service : [transferts hors UE des
  services de notification — à renseigner avant la mise en ligne]. Si vos
  rappels vous parviennent par email seulement, rien ne part vers eux.
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
  depuis la désactivation. Après le premier refus, vous pouvez en faire
  une nouvelle, mais elle ne suspend plus rien ; et si l'email
  d'avertissement était déjà parti, un nouvel email part une fois
  l'échéance à 30 jours ou moins, et la purge n'a pas lieu moins de 30
  jours après lui. Dans tous les cas, elle n'a pas lieu moins de 30 jours
  après le premier refus, que cet email parte ou non. Ce report n'a lieu
  qu'une fois : un refus suivant n'envoie pas de nouvel email et ne
  repousse plus la purge. La date du premier refus est effacée à la
  réactivation, à une nouvelle désactivation et à la purge. Un compte
  dont la suppression avait été demandée avant sa désactivation est
  purgé au terme de ses 30 jours de grâce, comme tout autre, demande de
  réactivation en attente ou non.
- **Déclaration d'âge** : la déclaration et sa date restent tant que le
  compte existe, et sont effacées à la purge du compte.
- **Acceptation des conditions d'utilisation** : la version acceptée et sa
  date restent tant que le compte existe ; la prise de connaissance d'une
  nouvelle version les remplace, et la purge du compte les efface.
- **Sessions de connexion** : une session vaut 30 jours au plus, et prend
  fin plus tôt si elle reste 7 jours sans activité. La dernière activité
  n'est enregistrée qu'une fois par heure au plus : la fin peut donc
  survenir jusqu'à une heure avant ces 7 jours. Sa trace (dates
  de création, de dernière activité, d'expiration et de révocation) est
  supprimée au premier passage de purge qui suit la fin de la session :
  expiration, inactivité, déconnexion, changement de mot de passe ou
  désactivation du compte. La session qu'ouvre ensuite la connexion à un
  compte désactivé, limitée à la page de demande de réactivation, prend
  aussi fin à la réactivation du compte. La page « Sessions actives » de
  votre compte liste vos sessions en cours, avec leurs seules dates, et
  vous permet de déconnecter l'une d'elles ou toutes à la fois.
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
- **Photo d'un code-barres** (scan d'un article de stock) : pas conservée
  du tout. Elle peut montrer l'intérieur de votre logement ; le serveur la
  lit en mémoire le temps de trouver le code, puis l'oublie avec la
  requête : elle n'est écrite nulle part, ni dans un fichier, ni en base,
  ni dans les journaux, et n'est transmise à personne. Seul le contenu du
  code continue, comme si vous l'aviez tapé : ses chiffres, ou, pour un
  code 2D GS1 (DataMatrix, QR), son texte — numéro du produit et, selon le
  code, date de péremption, numéro de lot ou de série. Seuls le numéro du
  produit et la date sont utilisés. Un autre code 2D présent sur la photo
  (QR d'une marque, d'un réseau Wi-Fi…) est ignoré.
- **Agenda, stocks, recettes, liste de courses, budget, messagerie** : tant
  que le contenu n'est pas supprimé, et au plus tant que le groupe existe —
  la suppression d'un groupe supprime son contenu. Quand le compte de son
  auteur est anonymisé, le contenu reste dans le groupe sans être rattaché
  à son identité. Le fichier d'une pièce jointe que le serveur a reçu sans
  pouvoir l'enregistrer est supprimé par un balayage quotidien moins de
  48 heures après son dépôt, tant que le serveur tourne et que sa
  configuration permet ce balayage, aux mêmes conditions que le passage de
  purge.
- **Rappels d'événements** : jusqu'à la suppression du rappel ou de
  l'événement ; l'historique des envois (heure, statut, tentatives) part
  avec eux.
- **Appareils abonnés aux notifications** : jusqu'à ce que le service de
  notification du navigateur signale l'abonnement expiré ou retiré (il est
  alors supprimé au premier rappel qui l'essaie), que chaque envoi y
  échoue pendant au moins 7 jours, à raison d'au moins 20 échecs
  d'affilée, que vous désabonniez vos appareils depuis « Notifications de
  rappel », ou à la purge de votre compte. Un compte garde 50 appareils au
  plus : le 51e remplace celui qui a servi le moins récemment (dernier
  abonnement renouvelé ou dernier envoi réussi).
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
  passe haché, connexion avec Google), vos sessions, vos appareils abonnés
  aux notifications, vos jetons de
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
  déclaration d'âge et votre acceptation des conditions d'utilisation sont
  effacées : il ne reste de votre compte qu'un
  identifiant technique rattaché à aucune identité.
- **Ce qu'elle laisse en place** : le contenu que vous avez créé au sein
  d'un groupe familial — événements et pièces jointes, tâches cochées,
  messages, stocks, recettes et historique des repas, liste de courses,
  budget, et les groupes que vous avez créés — reste visible pour les
  autres membres de ce groupe, mais n'est plus rattaché à votre identité.
  C'est un choix intentionnel, cohérent avec le fonctionnement d'un espace
  familial partagé, et un engagement contractuel autant qu'une
  description : il figure aussi dans les [conditions générales
  d'utilisation](/terms-of-service). Seule exception : un groupe dont
  vous étiez le dernier membre est supprimé avec tout son contenu, plus
  personne ne pouvant le voir.
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
  s'il ne reste que de tels comptes, le groupe reste sans propriétaire,
  jusqu'à ce que l'administrateur du service réactive l'un d'eux — la
  propriété est alors attribuée de nouveau, dans le même ordre — ou
  désigne un propriétaire parmi les membres actifs. Si vous en étiez le
  seul membre, il est supprimé avec tout son contenu : plus personne ne
  pourrait le voir.
- Le membre qui devient ainsi propriétaire d'un groupe sans l'avoir
  demandé en est averti : un avis s'affiche sur sa page d'accueil jusqu'à
  ce qu'il en prenne connaissance, et un email lui est envoyé, quelles que
  soient ses préférences de rappel. Sont conservées, avec son appartenance
  au groupe et jusqu'à ce qu'elle prenne fin, la date et le motif de cette
  attribution, la date de l'email et celle de sa prise de connaissance ;
  elles figurent dans l'export de vos données.

## Vos droits

- **Droit d'accès et de portabilité (Art. 15/20)** : `GET /account/export`
  retourne, au format JSON, l'intégralité des données qui vous concernent :
  votre profil, déclaration d'âge et version acceptée des conditions
  d'utilisation comprises, avec leurs dates ; le contenu que vous avez créé dans chaque groupe (les
  recettes avec leurs ingrédients), y compris un groupe que vous avez
  quitté ou dont vous avez été retiré, tant que ce contenu y est conservé ;
  les événements qui vous sont assignés, y compris par un autre membre ;
  les rappels de vos événements et les envois qu'ils ont programmés, la
  façon dont ils vous parviennent et vos appareils abonnés aux
  notifications ; vos
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
  l'intérêt légitime (invitations, protection de la connexion,
  désactivation d'un compte, logs d'audit). Le traitement cesse, sauf
  motifs légitimes et impérieux qui prévalent sur vos intérêts, ou
  nécessité pour la constatation, l'exercice ou la défense de droits en
  justice. Adressez la demande au responsable de traitement.
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
redémarrage, ou au premier passage horaire qui suit le rétablissement de
cette configuration). Elle l'est aussitôt si le lien est utilisé.

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

Si une donnée personnelle vous concernant venait à fuir sans être chiffrée,
vous en seriez prévenu par email, à l'adresse de votre compte, même si le
risque est jugé faible : le service s'impose ce seuil, plus bas que celui du
RGPD, qui n'exige de prévenir les personnes qu'en cas de risque élevé.
L'email dit ce qui s'est passé, quelles données sont en cause, ce qui a été
fait et ce que vous pouvez faire.
