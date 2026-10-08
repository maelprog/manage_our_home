# Registre des traitements — Manage Our Home

Registre tenu au titre de l'article 30 du RGPD. Dernière mise à jour :
2026-10-08 (code-barres d'un article de stock, photo du code lue sans être conservée, Open Food Facts, #402) ;
2026-10-05 (fin d'une session inactive et jetons de réinitialisation, #327 ;
jetons stockés par leur empreinte, #335) ;
2026-10-04 (sous-traitant email, #328 ; acceptation des CGU, #319 ;
propriété d'un groupe héritée ou désignée et groupe sans membre, #323) ; relu le 2026-09-28 contre les
migrations `apps/api/migrations/0001` à `0016` et la table des routes de
`apps/api/src/lib.rs`. Un registre des traitements est requis dès qu'un
traitement de données personnelles est effectué, y compris à petite échelle
— voir `docs/architecture.md` §10.

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
- **Scaleway** (Scaleway SAS, Paris), relais SMTP transactionnel (Scaleway
  Transactional Email) — les six seuls emails que le service envoie :
  vérification d'adresse, réinitialisation de mot de passe, invitation à un
  groupe, rappel d'événement, avertissement avant la purge d'un compte
  désactivé (#256) et avis de propriété d'un groupe reçue sans l'avoir
  demandée (#323). Reçoit l'adresse du destinataire, l'objet et le corps de
  chacun (le corps d'un rappel recopie le titre et la date de l'événement, son objet
  est neutre — « Rappel d'un événement à venir » ; le corps
  d'une invitation nomme le groupe et le membre qui invite ; le corps d'un
  avis de propriété nomme le groupe et dit pourquoi la propriété revient
  au destinataire, son objet ne nomme aucun groupe). Retenu par
  l'arbitrage du 2026-10-02 (#328), qui remplace celui du 2026-09-19 (#136,
  Mailjet) : Mailjet recourt à des sous-traitants ultérieurs établis hors UE
  (#316), Scaleway déclare traiter le flux entièrement dans l'UE (voir
  « Transferts hors de l'Union européenne » ci-dessous) ; l'auto-hébergement
  a été écarté pour la délivrabilité et la charge d'exploitation
  (`docs/architecture.md`). Scaleway porte un accord de traitement des
  données (DPA, art. 28) dans son cadre contractuel, accepté depuis la
  console du compte ; la liste de ses sous-traitants ultérieurs est publiée
  à une URL publique (`scaleway.com/en/subprocessorlist/`) plutôt
  qu'annexée au contrat. La version du DPA acceptée, la date de son
  acceptation et le compte d'envoi qu'elle couvre restent à établir :
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
- Le **service de notification du navigateur** de chaque membre qui reçoit
  ses rappels par notification (#306) — Google (FCM, pour Chrome et la
  plupart des navigateurs qui en dérivent), Mozilla (Firefox), Apple
  (Safari), Microsoft (Edge). Destinataire de fait : c'est le navigateur du
  membre qui le choisit, pas le responsable de traitement, et aucun contrat
  ne les lie. Reçoit, à chaque rappel, un message **vide** (aucune charge
  utile, `apps/api/src/notifications/push.rs`) à l'adresse d'abonnement de
  l'appareil, signé de la clé VAPID du serveur ; il apprend donc qu'un
  message part de ce serveur vers cet appareil, à cette heure, et rien de
  l'événement. L'appareil affiche toujours « Rappel d'un événement à venir ».
- **Open Food Facts** (#402) n'est destinataire d'aucune donnée
  personnelle, et n'appelle donc pas de ligne au tableau ci-dessous. Quand
  un membre scanne ou saisit le code-barres d'un article, c'est le serveur
  qui interroge Open Food Facts (`apps/api/src/stocks/openfoodfacts.rs`),
  avec le code seul : ni photo, ni cookie, ni identifiant de compte ou de famille, et
  l'adresse IP vue est celle du serveur, pas celle du membre. Il n'est pas
  interrogé pour une étiquette de pesée du magasin, pour un code qu'un
  article de la famille porte déjà, ni pour un code dont la fiche est en
  cache (`off_products`, table de fiches publiques rattachée à aucune
  famille ni à aucun compte). Mentionné pour que la liste des flux sortants
  soit complète.

**Mesures de sécurité communes** : chiffrement au repos via `pgcrypto` pour
les colonnes sensibles, isolation multi-tenant appliquée au niveau base de
données (Row-Level Security, `FORCE ROW LEVEL SECURITY` sur chaque table
tenant-scoped), TLS en transit entre le navigateur et le serveur,
`cargo audit` en CI, logs d'audit sur les actions sensibles (`audit_log`).
Le TLS est terminé par Caddy, frontal de la pile (`infra/Caddyfile`, #141) :
le nom de domaine public placé dans `SITE_ADDRESS` (par
`infra/generate-env.sh`) lui fait obtenir et renouveler seul un certificat,
répondre sur le port 443 et ne faire du port 80 qu'une redirection vers
443 ; l'en-tête `Strict-Transport-Security` interdit ensuite le HTTP en
clair au navigateur pendant un an. Il faut pour cela que le nom résolve
vers l'hôte et que les ports 80 et 443 y soient ouverts depuis Internet
(README, « Running it for real »). Sans `SITE_ADDRESS`, Caddy écoute en
HTTP en clair sur `:80` : c'est la pile locale de développement, jamais
un déploiement public. Derrière Caddy, le trafic vers `apps/web` et
`apps/api` reste en HTTP sur le réseau Docker interne à l'hôte.

## Transferts hors de l'Union européenne

Les flux sortants sont ceux nommés ci-dessus, et aucun autre. Le mécanisme de
transfert (art. 44-49) se juge destinataire par destinataire. Le relais email
n'en demande aucun : Scaleway déclare ne transférer aucune donnée du service
hors de l'UE. Pour les autres destinataires tiers, **aucun mécanisme n'est
arrêté à ce jour** : le service n'est pas ouvert, et la dernière colonne porte
donc un placeholder plutôt qu'une affirmation. Ce qui est vérifié est écrit tel
quel, avec sa source et sa date ; le reste attend la mise en ligne
(`docs/v2-deployment.md` #18).

| Destinataire | Ce qui sort | Ce qui est vérifié | Mécanisme (art. 44-49) |
|---|---|---|---|
| Scaleway (relais SMTP, Scaleway Transactional Email) | adresse du destinataire, objet et corps de l'email | Aucun transfert hors UE. La FAQ du service (`scaleway.com/en/docs/transactional-email/faq/`, revue par Scaleway le 2025-09-24, consultée le 2026-10-04) répond : « no personal data is transferred outside the EU […] All data is hosted and processed entirely within the European Union », et, à la question des sous-traitants hors UE : « No. The entire Transactional Email (TEM) technical stack is fully managed by Scaleway, within the EU ». La même réponse ajoute que, pour l'ensemble de ses services, Scaleway peut, « in rare, exceptional cases », recourir à des partenaires américains ou canadiens, sous les clauses contractuelles types de l'article 11 de son DPA. La liste des sous-traitants ultérieurs (`scaleway.com/en/subprocessorlist/`, revue en juillet 2025, consultée le 2026-10-04) ne nomme aucun sous-traitant pour ce service — son seul sous-traitant canadien est rattaché à un autre produit (Dedibox VPS) — et tous les centres de données qu'elle cite traitent dans l'UE | Sans objet : aucun transfert hors UE |
| Google (connexion avec Google) | la demande d'autorisation, l'échange du code par le serveur, la lecture du profil (`sub`, email, nom) | Google Ireland Limited pour les utilisateurs de l'UE, sur une infrastructure mondiale dont une partie est aux États-Unis. Google déclare que « Google LLC, y compris ses filiales américaines détenues à 100 % (sauf exclusion explicite) » adhère aux principes du cadre de confidentialité des données — la réserve laisse ouverte l'entité qui traite effectivement, et la liste officielle du cadre n'a pas pu être consultée le 2026-09-21, le site répondant en erreur. Rien n'est donc vérifié ici au-delà de la déclaration | [transferts hors UE de Google — à renseigner avant la mise en ligne] |
| Services de notification des navigateurs (Google, Mozilla, Apple, Microsoft) | l'adresse d'abonnement d'un appareil (`push_subscriptions.endpoint`) et, à chaque rappel, une requête sans charge utile signée de la clé VAPID du serveur | Infrastructures mondiales, pour partie hors UE ; l'adresse d'abonnement nomme le service (`fcm.googleapis.com`, `updates.push.services.mozilla.com`, `*.push.apple.com`, `*.notify.windows.com` — les seuls hôtes que `notifications::push::validate_endpoint` accepte). Rien d'autre n'est vérifié | [transferts hors UE des services de notification — à renseigner avant la mise en ligne] |
| Hébergeur du flux iCal (Google en pratique) | l'URL de flux fournie et la requête du serveur vers cette URL | Le code accepte toute URL `http`/`https` et ne la contraint pas à l'UE : la localisation dépend de ce que le membre a collé | Celui de la ligne Google ci-dessus tant que le flux est hébergé par Google ; à rappeler à qui configure un import dans le cas contraire |
| Postgres, MinIO, Ollama | rien : ils tournent sur le serveur du responsable de traitement | Aucun code de `apps/` n'appelle un tiers pour ces trois services | Sans objet : pas de tiers, pas de transfert |

**Ce que cette section ne garantit pas.** La localisation du relais est une
garantie d'exploitation, pas une propriété du code : l'hôte vient de la
variable d'environnement `SMTP_HOST` (`apps/api/src/main.rs`), que rien ne
contraint à pointer sur Scaleway (`smtp.tem.scaleway.com`) ni sur l'UE. Le contrôle correspondant est
porté par `docs/v2-deployment.md` #18, à faire avant la première mise en
ligne.

## Catégories de traitement (une par epic)

| # | Épic | Données traitées | Finalité | Base légale | Durée de conservation | Destinataires |
|---|---|---|---|---|---|---|
| 1 | Auth + Groupes | email, mot de passe (haché argon2), nom affiché, déclaration d'avoir au moins 15 ans et sa date (`users.age_declared_at` ; aucune date de naissance — art. 8 RGPD, #137), version des CGU acceptée et sa date (`users.terms_accepted_version` / `terms_accepted_at`, #319 : à l'inscription, à la première connexion d'un compte ouvert avec Google, ou à la prise de connaissance d'une nouvelle version, qui remplace la précédente), appartenance aux groupes et rôle — avec, pour un membre devenu propriétaire sans l'avoir demandé (héritier de la purge, membre réactivé d'un groupe sans propriétaire, ou désigné par le superadmin), la date et le motif, la date de l'email qui le lui annonce et celle où il a pris connaissance de l'avis affiché à la connexion (`group_members.ownership_*`, #323) —, sessions (création, dernière activité, expiration, révocation) | Authentification, gestion de compte, isolation familiale | Exécution du contrat | Compte actif + 30j de grâce après demande de suppression, puis purge du compte (voir « Droit à l'effacement » ci-dessous) : la ligne `users` est anonymisée, déclaration d'âge et acceptation des CGU effacées comprises, et l'appartenance aux groupes et le rôle sont supprimés, avec les dates de l'avis de propriété (qui partent aussi avec l'appartenance quand le membre quitte le groupe) ; une session vaut 30 jours au plus et prend fin après 7 jours sans activité, jusqu'à 1 h plus tôt puisque `last_seen_at` n'est réécrit qu'une fois par heure ; sa ligne est supprimée par la purge de conservation horaire (`apps/api/src/jobs/retention_purge.rs`, #138) dès que la session a pris fin — révocation (déconnexion, changement de mot de passe), `expires_at` dépassé, ou `last_seen_at` plus vieux que 7 jours, le même critère que l'extracteur de session (`last_seen_at` n'est réécrit qu'une fois par heure) — ou que l'état du compte la refuse pour de bon : session ordinaire d'un compte désactivé, session restreinte (`sessions.restricted`, #289) d'un compte qui ne l'est plus ; et au plus tard à la purge du compte. Un compte désactivé par le superadmin est purgé après 2 ans de désactivation (voir ligne 8) ; un compte dont la suppression a été demandée l'est au terme de ses 30 jours de grâce, désactivé ou non (#139) | Scaleway (l'avis de propriété : adresse du membre, nom du groupe, motif) |
| 1a | Connexion avec Google | identifiant Google (`sub`), email et nom du profil Google (le nom sert de nom affiché si le compte est créé à cette occasion), jeton de rafraîchissement chiffré via `pgcrypto` quand Google en délivre un — aucun code ne le relit aujourd'hui (`oauth_identities`) | Authentification par un fournisseur d'identité, au choix de l'utilisateur | Exécution du contrat | Jusqu'à la purge du compte, qui supprime la ligne `oauth_identities` | Google (fournisseur d'identité : reçoit la demande d'autorisation, échange le code, sert le profil) |
| 1b | Jetons de vérification d'email et de réinitialisation du mot de passe | empreinte SHA-256 du jeton (le jeton lui-même n'est conservé nulle part, #335), compte concerné, dates de création et d'expiration (`email_verification_tokens`, `password_reset_tokens`), date de consommation d'un jeton de vérification (`email_verification_tokens.consumed_at`) ; la colonne `password_reset_tokens.consumed_at` n'est plus écrite depuis #286 — un jeton de réinitialisation est supprimé à l'usage | Prouver la possession de l'adresse ; réinitialiser un mot de passe oublié | Exécution du contrat | Valables 24 h (vérification) et 1 h (réinitialisation), à usage unique ; un jeton de réinitialisation est supprimé à l'usage ; la purge de conservation horaire (#138) supprime un jeton de vérification 48 h après sa création, consommé ou non, et un jeton de réinitialisation inutilisé 1 h après ; la purge du compte supprime ceux qui restent | Scaleway (le lien porteur du jeton part par email) |
| 1c | Invitations à un groupe | adresse email de la personne invitée, facultative (une invitation peut n'être qu'un lien) et stockée en clair, empreinte SHA-256 du jeton (le jeton lui-même n'est conservé nulle part, #335), auteur, dates (`invitations`) ; la colonne `consumed_by`, qui nommait le membre ayant accepté, n'est plus jamais renseignée depuis #138 — l'acceptation supprime la ligne | Faire entrer une personne dans un groupe ; la personne invitée est un tiers qui n'a pas encore de compte | Intérêt légitime (du membre qui invite un proche) | Valable 7 jours, à usage unique ; la ligne, adresse comprise, est supprimée à l'acceptation, sinon par la purge de conservation horaire 30 jours après sa création (#138), ou avant, avec le groupe s'il est supprimé ou à la purge du compte du membre qui l'a émise (#139) | Scaleway (si une adresse est saisie : email portant le nom du groupe, le nom affiché du membre qui invite, le lien, et la notice d'information de l'art. 14 — identité et contact du responsable, finalité, base légale, durée de conservation, source de l'adresse, lien vers la politique, droit de réclamation ; #134) |
| 1d | Protection de la connexion | adresse IP du client (en IPv6, réduite à son /64) et email saisi, à chaque tentative de connexion par mot de passe (`apps/api/src/auth/throttle.rs`) | Limiter les essais de mot de passe par couple (adresse, email) | Intérêt légitime (sécurité du service) | En mémoire du processus `api` seulement, jamais en base ; perdu au redémarrage, oublié dès une connexion réussie ; une entrée cesse de compter 15 min après sa première tentative (ou à la fin du blocage de 15 min) mais n'est retirée qu'à la prochaine tentative d'un autre email depuis la même adresse, quand la table atteint `MAX_TRACKED` (10 000 couples) ou au redémarrage — sans trafic, elle reste jusqu'au redémarrage | Aucun tiers |
| 2 | Agenda | événements, tâches et membre ayant coché une tâche, pièces jointes (photos/documents) | Planification familiale | Exécution du contrat | Tant que l'événement/le compte existe ; supprimé avec le groupe ou anonymisé (`created_by`) à la purge du compte auteur ; un fichier de pièce jointe orphelin (sans ligne en base) est supprimé par un balayage quotidien moins de 48 h après son écriture (fenêtre de 24 h + intervalle de 24 h) tant que l'API tourne, et à condition que `ADMIN_DATABASE_URL` désigne un rôle `BYPASSRLS` — sinon le balayage refuse de tourner et l'orphelin reste | Aucun tiers |
| 2a | Rappels d'événements, par notification ou par email | délai avant l'événement (`event_reminders`) ; file d'envoi par occurrence : heure d'envoi, statut, tentatives, dernière erreur de transport (`scheduled_notifications`) ; canal choisi par compte — notification (par défaut pour un compte créé depuis #306), email ou les deux (`users.reminder_channel`) ; par appareil abonné aux notifications, l'adresse d'abonnement attribuée par le service de notification du navigateur, la plateforme (`web`), les dates d'abonnement, de dernier enregistrement et de dernier envoi réussi, le nombre et la date de début des envois échoués d'affilée (`push_subscriptions`, 50 appareils au plus par compte : le 51e remplace celui qui a servi le moins récemment, enregistrement ou envoi réussi — arbitrage du 2026-10-01). L'email porte le titre et la date de l'événement ; la notification ne porte rien (message vide) et l'appareil affiche « Rappel d'un événement à venir ». Pas de repli d'un canal sur l'autre : sans appareil abonné, un rappel par notification n'est pas envoyé et l'application en avertit (arbitrage du responsable de traitement du 2026-10-01) | Prévenir avant un événement | Exécution du contrat | Un rappel vit jusqu'à sa suppression ou celle de l'événement ; les lignes de la file restent après envoi (statut `sent` ou `failed`) et partent avec le rappel ou l'événement. Un appareil abonné reste jusqu'à ce que son service de notification le dise expiré ou retiré (réponse 404 ou 410 : la ligne est supprimée au premier envoi qui la reçoit), jusqu'à ce que chaque envoi y échoue pendant au moins 7 jours avec au moins 20 échecs d'affilée (`MAX_CONSECUTIVE_FAILURES`, `MIN_FAILING_DAYS`), jusqu'au désabonnement par le membre, ou jusqu'à la purge du compte | Scaleway (email) ; service de notification du navigateur (notification, voir ci-dessus) ; le rappel part au **créateur de l'événement**, quel que soit le membre qui l'a posé, sur le canal du compte de ce créateur (`apps/api/src/jobs/scheduled_notifications.rs`) |
| 2b | Assignations d'événements | événement, membre assigné, date (`event_assignees`) ; posées par un membre, par défaut le créateur de l'événement, et pour un événement importé le membre qui a lancé l'import | Indiquer pour qui est un événement | Exécution du contrat | Tant que l'événement existe et que l'assignation n'est pas retirée ; elle survit au départ du groupe, pas à la purge du compte, qui la supprime (#139) | Membres du groupe |
| 3 | Stocks | articles du garde-manger/frigo, quantités, seuils, dates de péremption, code-barres d'un article scanné ou saisi (`stock_items.barcode`, #402) ; photo du code-barres envoyée pour le scan, qui peut montrer l'intérieur du logement : lue en mémoire par apps/web (`apps/web/src/routes/stocks/photo.rs`), ni écrite, ni journalisée, ni transmise à apps/api ou à un tiers — seuls les chiffres décodés continuent | Gestion de l'inventaire familial | Exécution du contrat | Idem #2 pour les articles ; la photo n'est pas conservée : elle disparaît avec la requête qui l'apporte | Aucun tiers (le code-barres seul est envoyé à Open Food Facts, sans donnée personnelle : voir « Sous-traitants (destinataires) ») |
| 4 | Recettes | recettes, ingrédients, historique des repas | Suggestions de repas (algorithme local, pas d'IA tierce) | Exécution du contrat | Idem #2 | Aucun tiers |
| 5 | Liste de courses | articles à acheter, source (manuel/recette/stock bas) | Liste de courses partagée | Exécution du contrat | Idem #2 | Aucun tiers |
| 6 | Budget | dépenses saisies manuellement (montant, nom, date) | Suivi du budget alimentaire familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7 | Messagerie | contenu des messages (chiffré au repos via `pgcrypto`) | Communication au sein du groupe familial | Exécution du contrat | Idem #2 | Aucun tiers |
| 7a | État de lecture de la messagerie | un horodatage par (groupe, membre), avancé quand le membre ouvre la messagerie (`message_read_state`) | Compter les messages non lus de ce membre | Exécution du contrat | Tant que le groupe existe ; survit au départ du groupe, pas à la purge du compte, qui la supprime (#139) | Aucun tiers |
| 8 | User admin (superadmin) | liste des groupes/utilisateurs à l'échelle globale, avec pour chaque groupe s'il a un propriétaire ; pour un groupe resté sans propriétaire seulement, la liste de ses membres (nom affiché, email, rôle, date d'arrivée, compte désactivé ou en suppression demandée) et la désignation de l'un des membres actifs comme propriétaire (#323) ; désactivation et réactivation d'un compte, date de désactivation (`users.deactivated_at`) et date de l'email d'avertissement (`users.deactivation_notice_sent_at`) ; demande de réactivation du titulaire, sa date et son message facultatif de 1000 caractères au plus (`account_reactivation_requests`, #289), date du premier refus depuis la désactivation (`users.reactivation_refused_at`) | Support technique/maintenance de la plateforme | Intérêt légitime (exploitation du service) | La désactivation n'efface rien et dure jusqu'à la réactivation, qui efface les deux dates. La demande de réactivation — une en attente à la fois — est supprimée par la décision du superadmin (réactivation ou refus) ou par la purge du compte ; tant qu'elle est en attente, la purge à 2 ans et son email d'avertissement sont suspendus, si c'est la première depuis la désactivation — pas la purge d'un compte dont la suppression a été demandée (ci-dessous). Le premier refus depuis la désactivation date `reactivation_refused_at` : une nouvelle demande reste possible mais ne suspend plus rien (arbitrage du responsable de traitement du 2026-09-29) ; il efface aussi la date de l'email, si bien que, si l'avertissement était déjà parti, un nouveau part une fois l'échéance à 30 jours ou moins, et la purge a lieu 30 jours après lui au plus tôt ; elle n'a de toute façon jamais lieu moins de 30 jours après le refus, que ce nouvel email parte ou non (#296). Ce report de 30 jours n'a lieu qu'une fois : un refus suivant supprime la demande sans rien changer d'autre, ni la date du premier refus ni celle de l'email, et ne repousse donc plus la purge (arbitrage du responsable de traitement du 2026-10-02, #313). `reactivation_refused_at` est effacé par la réactivation, une nouvelle désactivation et la purge. Sans réactivation, le compte est purgé (voir « Droit à l'effacement » ci-dessous) après 2 ans de désactivation — durée recommandée par la CNIL pour un compte inactif, arbitrage du responsable de traitement du 2026-09-28 (#256) ; son titulaire est averti par email 30 jours avant (de nouveau après le premier refus, s'il suit un avertissement, dans le cas ci-dessus), et la purge n'a jamais lieu moins de 30 jours après cet email ; s'il n'a pas pu partir, elle a lieu au plus tôt 2 ans et 30 jours après la désactivation. Exception : un compte dont la suppression a été demandée est purgé au terme de ses 30 jours de grâce, désactivé ou non, demande de réactivation en attente ou non, refus ou non (ligne 1, #139) | Scaleway (l'email d'avertissement : adresse du compte, dates de désactivation et de purge, identité et contact du responsable) |
| 9 | Import calendrier Google | URL de flux iCal privée (chiffrée), libellé, date du dernier import, événements importés et leur identifiant externe | Miroir en lecture seule d'un agenda Google externe | Consentement explicite (l'utilisateur fournit volontairement l'URL) | Jusqu'à la suppression de l'import par un administrateur ou le propriétaire du groupe, ou du groupe ; les événements importés restent après la suppression de l'import sauf si leur suppression est demandée avec lui ; la purge du compte du membre qui a configuré l'import supprime l'import, URL de flux comprise, et laisse ses événements dans le groupe (#139) | Hébergeur du flux (Google en pratique ; le code accepte toute URL `http`/`https`), interrogé par le serveur à chaque import lancé par un membre — aucune synchronisation en arrière-plan |
| — | Logs d'audit (transverse) | horodatage, acteur, action, cible, métadonnées (identifiants et compteurs) ; actions journalisées : export, demande et annulation de suppression de compte, demande de réactivation d'un compte désactivé, purge, suppression de groupe (y compris par la purge, faute de membre), transfert de propriété (y compris par la purge, par la réactivation d'un membre d'un groupe sans propriétaire et par la désignation du superadmin, avec le motif), changement de rôle, consultation des listes et des membres d'un groupe sans propriétaire, désactivation et réactivation d'un compte et refus d'une demande de réactivation par le superadmin — les connexions ne le sont pas | Traçabilité de sécurité, obligations RGPD (preuve des actions d'export/suppression) | Intérêt légitime | 6 mois glissants (recommandation de la CNIL pour les journaux, délibération n° 2021-122), appliqués par la purge de conservation horaire (#138) ; la purge d'un compte supprime aussitôt les entrées dont il est l'acteur ; restent, pour la même durée, celles qui le concernent sans être de son fait (voir « Droit à l'effacement » ci-dessous) (#139). Le minimum d'un an du décret n° 2021-1362 a été écarté : il vise les hébergeurs et les services de communication au public, pas une application familiale portée par une personne physique — arbitrage du responsable de traitement du 2026-09-19, à revoir si le service change de nature | Aucun tiers |
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
   (`account_reactivation_requests`, #289), appareils abonnés aux
   notifications (`push_subscriptions`, #306).
2. Supprime la part personnelle des autres tables : les entrées
   `audit_log` dont le compte est l'acteur, les invitations qu'il a
   émises (adresses de tiers comprises) et les imports calendrier qu'il a
   configurés (`calendar_imports`, URL de flux comprise — un porteur
   d'accès à un agenda externe ; les événements importés restent).
3. Anonymise la ligne `users` : email et nom remplacés, mot de passe
   haché, date de déclaration d'âge et acceptation des CGU (version et
   date) effacés, `deleted_at` renseigné.
4. Écrit, sans acteur, une entrée `audit_log` `account_purged` qui
   date la purge, une entrée `ownership_transferred` par groupe dont
   il transfère la propriété et une entrée `group_deleted` par groupe
   qu'il supprime faute de membre (ci-dessous).

Arbitrage du responsable de traitement du 2026-09-19 (#139) : est
supprimé tout ce qui n'est pas du contenu partagé avec le groupe. Le
contenu partagé reste, rattaché à la ligne anonymisée : événements, pièces
jointes et leurs fichiers, tâches cochées
(`event_occurrence_completions`), messages, stocks, recettes, historique
des repas, liste de courses, budget, et les groupes créés par le compte.
Cette conservation est portée par les conditions générales
(`docs/terms-of-service.md`, « Vos contenus »). Exception (arbitrage du
responsable de traitement du 2026-10-04, #323) : un groupe que la purge
laisse sans aucun membre est supprimé, avec tout son contenu — plus
personne ne peut le voir ni le supprimer. Les lignes partent dans la
transaction de la purge ; les fichiers des pièces jointes juste après,
et, si cette suppression échoue, par le balayage quotidien des fichiers
orphelins (ligne 2). Restent aussi, jusqu'au
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
appartenance — jusqu'à ce que le superadmin réactive l'un d'eux, ce qui
rejoue la succession sur les membres du moment, ou désigne un
propriétaire parmi les membres actifs (ni désactivés ni en suppression
demandée), le seul levier de secours (arbitrage du 2026-10-04, #323). Un
groupe dont le compte était le seul membre est supprimé avec son
contenu (même arbitrage, qui remplace celui du 2026-09-28).

Le membre qui devient ainsi propriétaire — héritier de la purge, membre
réactivé ou désigné — en est averti (#323) : un avis s'affiche sur sa page
d'accueil jusqu'à ce qu'il en prenne connaissance, et un email part au
passage horaire suivant du même job, quelles que soient ses préférences de
rappel (un email refusé par le relais est retenté au passage suivant).
Les dates de l'avis sont portées par son appartenance au groupe
(`group_members.ownership_*`) et partent avec elle.

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
