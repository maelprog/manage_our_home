# Revue ciblée RLS, migrations et modèle de permission — 2026-11-11

Issue #382 (ex-revue `/code-review ultra` de #98). Tête relue : `origin/main`
au 2026-11-11 (`8135e11`), migrations `0001` à `0052`.

Le rapport ne corrige rien. Chaque constat confirmé doit devenir une issue
(une par défaut), après arbitrage.

## Méthode

- Lecture des 52 migrations (politiques, `FORCE`, fonctions `SECURITY DEFINER`,
  droits accordés), de `auth/session.rs` (`scoped_tx`, `user_scoped_tx`,
  `token_scoped_tx`), de tous les handlers qui ouvrent une transaction ou lisent
  sur le pool, des sept endpoints `/admin/*` et des jobs sur `admin_db`.
- Inventaire outillé : chaque fonction `async` de `apps/api/src` qui appelle
  `scoped_tx` sans `require_role` a été relue à la main. Il y a 90 appels à
  `scoped_tx` dans `apps/api/src` (décompte `grep` de cette revue, sans la
  définition ni les commentaires). Chaque handler qui écrit a été confronté à
  la barre `can_modify` / `can_configure` / `actor_can_act_on` de son module.
- Reproduction en SQL des constats 1, 2 et 8 sur un `postgres:16` migré de
  `0001` à `0052`, sous un rôle `NOSUPERUSER NOBYPASSRLS` doté des droits que
  prescrivent `apps/api/README.md` et `infra/postgres/init/01-roles.sh`. Le
  script est en annexe. Les autres constats viennent de la lecture du code ;
  le champ « Preuve » de chacun le précise.

Barème de sévérité : **haute** (fuite ou écriture entre familles ou entre
comptes, exploitable aujourd'hui sans autre défaut) ; **moyenne** (fuite ou
perte de données réelle mais bornée, ou défense en profondeur qui ne tient pas
ce qu'elle promet) ; **basse** (cas limite, course, durcissement) ; **info**
(choix assumé, consigné pour mémoire).

## Verdict

Aucun constat de sévérité haute. Aucun handler relu ne lit ni n'écrit les
données d'une famille sans vérifier, dans le code Rust, que l'appelant en est
membre. En revanche, la RLS ne vérifie pas cette appartenance (constat 1). Elle
filtre sur un identifiant de groupe que fournit l'appelant. Deux fuites réelles
touchent les **anciens membres** d'une famille (constats 2 et 3). Une règle de
permission de l'agenda n'est pas appliquée aux rappels ni aux pièces jointes
(constat 4).

| # | Sévérité | Sujet | Preuve |
|---|---|---|---|
| 1 | moyenne | La RLS ne vérifie pas l'appartenance : `app.group_id` vient de l'URL | reproduit (SQL) |
| 2 | moyenne | L'export d'un ancien membre lit l'état actuel des événements où il est encore assigné, privés compris | reproduit (SQL) |
| 3 | moyenne | Les rappels continuent d'être envoyés au créateur d'un événement après son départ du groupe | lecture |
| 4 | moyenne | N'importe quel membre peut supprimer les rappels et les pièces jointes d'un événement qui n'est pas le sien | lecture |
| 5 | moyenne (à confirmer) | Connexion Google sur un compte à mot de passe non vérifié : préemption de compte | lecture |
| 6 | basse | Courses entre départ et transfert : groupe sans propriétaire ou sans membre | lecture |
| 7 | basse | Aucun garde au démarrage sur le rôle de `DATABASE_URL` / `ADMIN_DATABASE_URL` | lecture |
| 8 | basse | `audit_log` modifiable et effaçable par le rôle d'exécution | reproduit (SQL) |
| 9 | basse (à arbitrer) | Un admin peut nommer d'autres admins ; le retrait d'un membre n'est pas journalisé | lecture |
| 10 | basse (à arbitrer) | Le jeton d'invitation suffit : l'adresse invitée n'est pas comparée au compte | lecture |
| 11 | basse (à arbitrer) | L'export montre le budget à un membre standard désigné comme payeur | lecture |
| 12 | info | Un ancien membre garde 30 jours les notifications de la famille | lecture |
| 13 | info | Le superadmin peut se désactiver lui-même, ou désactiver le dernier superadmin | lecture |

## Constats

### 1. La RLS ne vérifie pas l'appartenance : `app.group_id` vient de l'URL — moyenne

- **Où** : `apps/api/src/auth/session.rs:706-721` (`scoped_tx`) ;
  politiques `group_members_isolation` (`migrations/0001_users_auth_groups.sql:146`,
  réécrite en `0042_rls_app_group_id.sql:41`), `groups_group_isolation`
  (`0014_membership_read_policies.sql:43`, `0042:104-106`) et les autres
  politiques que `0042` réécrit (25 au total, selon son en-tête) ; `groups::require_role` (`apps/api/src/groups/mod.rs:632-645`).
- **Constat** : chaque handler ouvre `scoped_tx(&state.db, group_id, user_id)`
  avec le `group_id` du chemin, **avant** tout contrôle d'appartenance. Les
  politiques comparent `group_id` à `app.group_id` et ne regardent pas
  `group_members` (sauf l'événement privé, `0044`). Un appelant étranger au
  groupe qui obtient `app.group_id` voit donc toute la famille, sous les
  politiques comme hors d'elles. Seul le code Rust l'en empêche, de deux
  manières : un `require_role` / `require_budget_access`, ou une jointure sur
  `group_members me` écrite dans la requête (`get_group`, `get_member`,
  `get_member_avatar`, `list_national_days`). Le commentaire de `scoped_tx`
  (« enforce tenant isolation at the DB layer ») promet plus que cela. La
  doc de `get_member` (`groups/mod.rs:279-282`) le reconnaît déjà.
- **Scénario** : un handler ajouté demain oublie `require_role`, ou fait sa
  première lecture avant. Un utilisateur connecté qui connaît ou devine l'UUID
  d'une autre famille lit ses données et y écrit (renommer le groupe, s'insérer
  dans `group_members`). La RLS ne bloque rien. Reproduit sous le rôle
  prescrit : avec `app.user_id` = un étranger et `app.group_id` = G1,
  `SELECT name FROM groups` rend G1, `UPDATE groups SET name = …` rend
  `UPDATE 1`, et `INSERT INTO group_members (G1, étranger, 'standard')` rend
  `INSERT 0 1` (annexe, R2). Avec `user_scoped_tx` seul, il ne voit aucun
  groupe (R3).
- **Aujourd'hui** : aucun chemin exploitable trouvé. Les 90 appels sont suivis
  d'un contrôle d'appartenance, ou relèvent d'un cas où il n'en faut pas :
  `create_group` sur un id neuf, `accept_invitation` via le jeton,
  `export_account` sur les seules lignes de l'appelant, `list_upcoming` sur ses
  seules appartenances.
- **Piste** : que `scoped_tx` vérifie lui-même l'appartenance avant de poser
  `app.group_id` (la lecture passe par `group_members_self_read` sous
  `app.user_id`), avec des variantes nommées pour les trois exceptions (export
  d'un ancien groupe, acceptation d'invitation, relecture `load_message`). Autre
  voie : ajouter à chaque politique un prédicat d'appartenance, par une
  fonction `SECURITY DEFINER` sur le modèle de
  `event_is_assigned_to_current_user`. Elle coûte une migration qui réécrit toutes
  les politiques à clé de groupe.

### 2. L'export d'un ancien membre lit l'état actuel des événements où il est encore assigné — moyenne

- **Où** : `apps/api/src/groups/mod.rs:1024` (`remove_member`) et `:1092`
  (`leave_group`), qui ne suppriment que la ligne `group_members` ;
  `apps/api/src/agenda/events.rs` (`update_event` avec `assignee_ids` absent :
  les assignés restent tels quels) ; `apps/api/src/rgpd/mod.rs:236-245`
  (`event_assignments`) ; `account_export_group_ids()`
  (`migrations/0033_recipe_selections.sql:54`, branche `event_assignees`).
- **Constat** : le départ d'un membre laisse ses lignes `event_assignees`.
  L'export (`GET /account/export`) retrouve le groupe quitté par
  `account_export_group_ids()`, ouvre `scoped_tx` dessus, puis lit le titre,
  la description, le lieu, les dates et la récurrence **actuels** de chaque
  événement où il est assigné. La politique de `events` (`0044`) lui laisse
  voir les événements privés, puisqu'il en reste assigné.
- **Scénario** : B est retiré d'une famille (séparation, conflit). Un événement
  privé lui était assigné. Après son départ, A change le titre et le lieu
  (« rendez-vous avocat », adresse). B demande son export et lit le nouveau
  titre et le nouveau lieu. Il en sera ainsi tant que personne ne modifie la
  liste des assignés. Reproduit sous le rôle prescrit (annexe, R1) : l'export
  de B rend `titre modifie apres depart | lieu secret | t`.
- **Même mécanisme, en moins grave** : les lignes dont l'ancien membre est
  l'auteur (`events`, `budget_entries`, `grocery_items`…) sont exportées dans
  leur état actuel. Les modifications faites par d'autres après son départ y
  figurent donc.
- **Piste** : supprimer les `event_assignees` du membre dans le groupe au
  départ (`leave_group`, `remove_member`) ; ou figer ce que l'export rend pour
  un groupe quitté. Les deux demandent un arbitrage (art. 15 contre
  confidentialité de la famille).

### 3. Les rappels continuent d'être envoyés au créateur d'un événement après son départ — moyenne

- **Où** : `apps/api/src/jobs/scheduled_notifications.rs:222-230` (requête des
  rappels dus, sans filtre d'appartenance) et `reach` (`:362-420`), qui ne
  vérifie que l'état du compte (`deactivated_at`, `deleted_at`) ;
  `apps/api/src/agenda/reminders.rs:49-64` (`create_reminder` : la seule
  barre est l'appartenance).
- **Constat** : un rappel va au créateur de l'événement (`e.created_by`). Le
  job tourne sur `admin_db` et ne vérifie pas que ce créateur est encore membre
  du groupe. Les occurrences récurrentes continuent d'être matérialisées.
- **Scénario** : C a créé « Anniversaire de D » avec un rappel, puis C est
  retiré du groupe. Chaque rappel dû lui envoie le **titre actuel** (que les
  membres restants peuvent changer) et la date, selon son canal : par email,
  par push, et dans son centre de notifications, avec le `group_id` d'une
  famille dont il n'est plus membre. Un membre restant peut en plus ajouter
  des rappels à cet événement : chacun produit de nouveaux envois vers C.
- **Piste** : ne traiter que les rappels dont le créateur est encore membre
  (jointure `group_members` dans la requête des rappels dus, ou retrait avec
  motif, comme `RECIPIENT_GONE`), et supprimer les rappels de l'ancien membre
  au départ.

### 4. N'importe quel membre peut supprimer les rappels et les pièces jointes d'un événement qui n'est pas le sien — moyenne

- **Où** : `apps/api/src/agenda/reminders.rs:185-205` (`delete_reminder`) ;
  `apps/api/src/agenda/attachments.rs:314-338` (`delete_attachment`) ;
  `upload_attachment` (`:60-70`) et `create_reminder` (`reminders.rs:49-64`).
  Ces quatre handlers s'arrêtent à `require_role`.
- **Constat** : `agenda::can_modify` (`agenda/mod.rs:8-26`) réserve la
  modification et la suppression au créateur, aux admins et propriétaires, et
  aux participants d'un événement privé. `docs/v1-scope.md` dit « a standard
  member gets no edit/delete on another member's event ». Les sous-ressources
  échappent à cette barre. Le registre (`docs/registre-traitements.md`, ligne
  2a) assume que n'importe quel membre **pose** un rappel ; rien n'assume la
  **suppression**.
- **Scénario** : un membre standard supprime la pièce jointe (ordonnance,
  billet) d'un événement qu'il n'a pas créé. L'objet est effacé du stockage
  avant la ligne (`delete_objects`), sans retour possible. Il peut aussi
  supprimer le rappel qu'un autre a posé : le créateur n'est plus prévenu.
- **Piste** : appliquer `can_modify`, ou une règle à arbitrer (« l'auteur du
  rappel ou de la pièce jointe, ou `can_modify` sur l'événement »). Il faudra
  ajouter la colonne d'auteur qui manque à `event_reminders`.

### 5. Connexion Google sur un compte à mot de passe non vérifié : préemption de compte — moyenne, à confirmer

- **Où** : `apps/api/src/auth/oauth_google.rs:321-359` (`sign_in_profile` :
  recherche par adresse, puis rattachement de l'identité Google au compte
  trouvé) ; `apps/api/src/auth/mod.rs:578` (la connexion par mot de passe
  refuse un compte non vérifié) et `:408` (la vérification pose
  `email_verified = true`).
- **Constat** : un profil Google vérifié se rattache au compte qui porte la
  même adresse, même si ce compte n'est **pas vérifié**. Le rattachement
  n'efface pas son `password_hash` et ne pose pas `email_verified`. Le domaine
  est l'authentification, mais c'est une lecture entre comptes, dans le
  périmètre de l'issue.
- **Scénario** : un attaquant inscrit `victime@…` avec son propre mot de
  passe ; le compte reste non vérifié. Plus tard, la victime se connecte avec
  Google. Elle arrive dans ce compte, y crée sa famille et ses données. Le jour
  où l'adresse est vérifiée (lien resté dans la boîte de la victime depuis
  l'inscription de l'attaquant, ou renvoi ultérieur), le mot de passe de
  l'attaquant ouvre le compte de la victime.
- **À confirmer** : par un test de flux. Il faut aussi vérifier si l'interface
  invite un compte Google non vérifié à vérifier son adresse.
- **Piste** : au rattachement d'une identité Google à un compte non vérifié,
  effacer le mot de passe et poser `email_verified = true`, ou refuser.

### 6. Courses entre départ et transfert : groupe sans propriétaire ou sans membre — basse

- **Où** : `apps/api/src/groups/mod.rs:529-548` (`perform_ownership_transfer`
  ne contrôle pas `rows_affected`) ; `:1058-1097` (`leave_group` compte les
  membres sans verrou) ; `:587` (`transfer_ownership`).
- **Scénarios** :
  1. le propriétaire A quitte le groupe en désignant B pendant que B le quitte.
     L'`UPDATE … role = 'owner'` de A ne touche plus aucune ligne, A part quand
     même, et le groupe reste **sans propriétaire** ;
  2. les deux derniers membres partent en même temps. Chacun compte 2 membres,
     les deux `DELETE` passent, et le groupe reste **sans membre**.
     `delete_if_abandoned` n'est appelé que par la purge de compte : le groupe
     et son contenu restent en base sans personne pour y accéder ni le
     supprimer.
- **Piste** : verrouiller les lignes `group_members` du groupe (`FOR UPDATE`)
  avant de compter et de transférer, et faire échouer le transfert si la cible
  a disparu. #400 a fait de même pour `designate_owner`.

### 7. Aucun garde au démarrage sur le rôle de `DATABASE_URL` / `ADMIN_DATABASE_URL` — basse

- **Où** : `apps/api/src/main.rs:41-68`. `ADMIN_DATABASE_URL` se replie sur
  `DATABASE_URL` (`:67-68`). Seuls les jobs vérifient leur rôle
  (`ensure_bypasses_rls`, `ensure_admin_pool`) ; les endpoints `/admin/*` ne
  le font pas.
- **Scénarios** :
  - `DATABASE_URL` pointe sur un superutilisateur ou un rôle `BYPASSRLS`.
    Toutes les politiques deviennent inertes, sans aucun signal. C'est déjà
    arrivé : le commentaire de `0014` raconte que l'API « only ever ran as a
    superuser » ;
  - `ADMIN_DATABASE_URL` est absent. Les jobs refusent de tourner (ils
    échouent fermés), mais `/admin/groups` répond une liste vide filtrée par
    la RLS au lieu d'une erreur.
- **Piste** : au démarrage, refuser un `DATABASE_URL` dont le rôle est
  `rolsuper OR rolbypassrls`, sauf indicateur de développement explicite.
  Exiger l'inverse pour `ADMIN_DATABASE_URL`, et supprimer le repli.

### 8. `audit_log` modifiable et effaçable par le rôle d'exécution — basse

- **Où** : `apps/api/README.md:63` et `infra/postgres/init/01-roles.sh`
  (`SELECT, INSERT, UPDATE, DELETE` sur toutes les tables pour `app_role`) ;
  table `audit_log` (`0001_users_auth_groups.sql:129`), sans RLS.
- **Constat** : le code ne modifie et ne supprime `audit_log` que depuis les
  jobs, sur `admin_db` (`jobs/account_purge.rs:363`,
  `jobs/retention_purge.rs:157`). Le rôle d'exécution garde pourtant les deux
  droits. Reproduit (annexe, R4) : `UPDATE 1`, puis `DELETE 1`.
- **Scénario** : une injection SQL ou un défaut dans un handler permet
  d'effacer ou de réécrire la trace des actions (`role_change`,
  `group_deleted`, `admin.*`).
- **Piste** : `REVOKE UPDATE, DELETE ON audit_log FROM app_role`, dans le
  README, dans `01-roles.sh` et dans une migration.

### 9. Un admin peut nommer d'autres admins ; le retrait d'un membre n'est pas journalisé — basse, à arbitrer

- **Où** : `apps/api/src/groups/mod.rs:950-966` (`change_role` :
  `actor_can_act_on("admin", "standard")` est vrai et `body.role` peut valoir
  `admin`) ; `:1010-1030` (`remove_member` n'appelle pas `audit::record`,
  alors que `change_role` le fait).
- **Scénarios** :
  - un admin promeut un membre standard en admin. Il ne peut plus le
    rétrograder ni le retirer : `actor_can_act_on("admin", "admin")` est faux.
    Seul le propriétaire le peut ;
  - le retrait d'un membre ne laisse aucune trace dans `audit_log`.
- **À arbitrer** : un admin a-t-il le droit de créer d'autres admins ?

### 10. Le jeton d'invitation suffit : l'adresse invitée n'est pas comparée au compte — basse, à arbitrer

- **Où** : `apps/api/src/groups/mod.rs:864-925` (`accept_invitation`).
- **Constat** : l'invitation peut porter une adresse (`invited_email`), mais
  l'acceptation ne la compare pas à celle du compte qui accepte. Le lien est
  un jeton porteur.
- **Scénario** : un email d'invitation transféré, ou lu par un tiers sur un
  appareil partagé, fait entrer un autre compte que le destinataire.
- **À arbitrer** : les invitations sans adresse existent (lien à partager). Il
  est sans doute voulu que le jeton suffise. Le préciser dans la doc ou dans
  la politique de confidentialité.

### 11. L'export montre le budget à un membre standard désigné comme payeur — basse, à arbitrer

- **Où** : `apps/api/src/rgpd/mod.rs:444-456`
  (`created_by = $2 OR paid_by = $2`, sans barre de rôle) ;
  `budget::require_budget_access` (`apps/api/src/budget/mod.rs:28-38`, #531).
- **Constat** : #531 ferme le budget aux membres standard ; la route leur
  répond 404 « so that the routes do not tell a child that there is a
  budget ». L'export leur rend pourtant chaque dépense dont ils sont le
  payeur : nom, montant, date, catégorie et articles liés.
- **À arbitrer** : art. 15 (une donnée qui désigne la personne) contre l'intention de #531.

### 12. Un ancien membre garde 30 jours les notifications de la famille — info

- **Où** : `migrations/0036_notifications.sql:73-78` (lecture par le seul
  destinataire, quelle que soit la famille) ; durée de rétention
  `NOTIFICATION_RETENTION_DAYS`.
- **Constat** : après un départ, les notifications reçues restent lisibles
  dans le centre de notifications : titres d'événements, noms d'articles,
  noms affichés des membres. Le constat 3 en ajoute d'autres, créées après le
  départ. Il faut choisir entre les supprimer au départ et les garder jusqu'à
  la purge.

### 13. Le superadmin peut se désactiver lui-même, ou désactiver le dernier superadmin — info

- **Où** : `apps/api/src/user_admin/admin.rs:175-212` (`deactivate_user`).
- **Constat** : rien ne compare la cible à l'acteur, ni ne compte les
  superadmins restants. On ne sort de cette situation que par la base.

## Tables sans RLS

Les 40 tables du schéma `public` (mesuré sur la base migrée de l'annexe) se
répartissent en 26 tables sous `ENABLE` + `FORCE ROW LEVEL SECURITY` et 14
tables sans RLS. Les 14 sont listées ci-dessous.

| Table | Justification | Reste à faire |
|---|---|---|
| `users` | Lue avant toute authentification (connexion, inscription, Google) et jointe pour afficher les noms des membres. Une politique sur `app.user_id` casserait ces chemins. | Aucune ; chaque lecture filtre sur l'id de la session ou passe par un contrôle d'appartenance (`get_group`, `get_member`). |
| `sessions` | Recherchée par l'empreinte du jeton, avant que l'utilisateur soit connu. | Aucune. |
| `oauth_identities` | Recherchée par `provider_user_id` avant la connexion. | Voir le constat 5. |
| `email_verification_tokens`, `password_reset_tokens` | Recherchées par jeton, avant la connexion. | Aucune. |
| `audit_log` | Écrite par le code, lue par personne côté requêtes. | Constat 8 : retirer `UPDATE`/`DELETE` au rôle d'exécution. |
| `account_reactivation_requests` | Propre au compte, écrite par une session restreinte, lue par le superadmin. | Une politique sur `app.user_id` serait possible ; gain faible. |
| `push_subscriptions` | Propre au compte, filtrée sur `user_id` dans le code. | Une politique sur `app.user_id` serait possible ; gain faible. |
| `login_throttle`, `rate_limit_log`, `csp_report_throttle`, `google_sign_in_nonces` | Tables techniques `UNLOGGED`, avant authentification, sans contenu familial. | Aucune. |
| `off_products` | Fiches publiques Open Food Facts, sans famille ni compte (`0026_stock_barcode.sql:20-36`). | Aucune. |
| `school_holidays` | Calendrier scolaire public. | Aucune. |

## Usages de `admin_db` (pool `BYPASSRLS`)

| Usage | Garde | Jugement |
|---|---|---|
| 7 endpoints `/admin/*` (`apps/api/src/user_admin/admin.rs`) | Extracteur `SuperAdminUser` (flag, puis âge de session) | Justifié. Aucun garde sur le rôle du pool (constat 7). |
| `jobs::scheduled_notifications` | `ensure_admin_pool` | Justifié. Ne filtre pas l'appartenance (constat 3). |
| `jobs::account_purge`, `groups::succession` | `ensure_bypasses_rls` | Justifié. Les héritiers sont choisis parmi les membres du groupe purgé, sous `FOR UPDATE`. |
| `jobs::retention_purge` | `ensure_bypasses_rls` | Justifié. |
| `jobs::stock_expiry`, `jobs::stock_expiry_recap` | `ensure_bypasses_rls` | Justifié. Les destinataires viennent de la jointure `group_members` sur le groupe de l'article. |
| `jobs::attachment_reconcile` | `rolsuper OR rolbypassrls` | Justifié. Il compte aussi `users.avatar_key` dans les clés à garder. |

Aucun handler hors `/admin/*` ne touche `admin_db`. Vérifié par `grep` :
`admin_db` n'apparaît que dans `main.rs`, `lib.rs`, `user_admin/` et `jobs/`.

## Fonctions `SECURITY DEFINER`

| Fonction | Exécutable par | Jugement |
|---|---|---|
| `account_export_group_ids()` (`0019`, `0033`) | rôle d'exécution seulement | Rend des ids de groupes, pour `app.user_id` seul. Tient. Sa liste de branches est tenue à la main : à revoir à chaque table qui nomme un utilisateur (`0039` explique pourquoi `paid_by` n'en a pas). |
| `account_export_received_invitations()` (`0019`, `0030`) | rôle d'exécution seulement | Sans le jeton. Tient. |
| `event_is_assigned_to_current_user(uuid)` (`0044`) | `PUBLIC` | Rend un booléen pour `app.user_id`. Tient. |
| `group_attachment_storage_keys(uuid)` (`0044`) | `PUBLIC` | Rend des clés seulement si `app.group_id` correspond et que `app.user_id` en est le propriétaire. Tient. |
| `account_export_event_attachments(uuid)`, `account_export_event_completions(uuid)` (`0045`) | `PUBLIC` | Rendent les lignes de l'appelant, dans le groupe de `app.group_id`. Tiennent. |

Les six ont un `search_path` figé avec `pg_temp` en dernier.

## Points vérifiés sans constat

- Les 26 tables sous RLS ont toutes `FORCE`. Aucune politique ne lit encore
  `app.family_id` après `0042`.
- `notifications_group_insert` laisse un membre écrire une notification pour
  n'importe quel `user_id`, dans sa propre famille seulement. Le centre
  d'invitations en dépend.
- `invitations_isolation` : la branche jeton compare l'empreinte (`0024`),
  jamais le jeton en clair.
- Messagerie : la charge de `NOTIFY` ne contient que des ids, et
  `load_message` relit sous la portée du groupe. Le WebSocket revalide
  l'appartenance et la session toutes les 30 s (AC #7, #221). La fenêtre de
  30 s pendant laquelle un membre retiré reçoit encore les messages est un
  choix consigné.
- Budget : chacun des 15 handlers HTTP de `apps/api/src/budget/` appelle
  `require_budget_access`, qui compte 15 appels (décompte `grep`).
- `stocks::scan` : le cache `off_products` est partagé entre familles. Son
  temps de réponse révèle seulement qu'un code a été scanné récemment par
  quelqu'un ; aucune famille n'y est désignée.
- `subscribe` (push) : un endpoint déjà enregistré par un autre compte passe
  au nouveau compte, ce qui sert le cas d'un navigateur partagé. Pour le
  détourner, il faut connaître l'URL d'abonnement, qui n'est pas publique.

## Hors périmètre, non couvert

- `apps/web` : la revue porte sur l'API, qui est l'unique garde.
- Infra k3s de staging : non relue.
- Les constats marqués « lecture » n'ont pas de test de reproduction. Un test
  de flux par constat retenu est la première étape de chaque issue.

## Annexe — reproduction SQL (constats 1, 2, 8)

Sur `postgres:16`, migrations `0001` à `0052` appliquées dans l'ordre par le
superutilisateur, après création de
`app_role NOSUPERUSER NOBYPASSRLS` avec, comme le prescrit
`01-roles.sh`, `SELECT, INSERT, UPDATE, DELETE` sur les tables et
`USAGE, SELECT` sur les séquences. Les ids sont abrégés ; le script complet
utilise des UUID.

```sql
-- Mise en place (superutilisateur) : A propriétaire et B membre de G1 ;
-- X étranger. Événement privé E1 de A, assigné à B.
-- B quitte G1, comme le font leave_group / remove_member :
DELETE FROM group_members WHERE group_id = G1 AND user_id = B;
UPDATE events SET title = 'titre modifie apres depart', location = 'lieu secret' WHERE id = E1;
SET ROLE app_role;

-- R1 (constat 2) : l'export de B
BEGIN;
SELECT set_config('app.user_id', B, true);
SELECT id FROM account_export_group_ids() AS id;          -- rend G1
SELECT set_config('app.group_id', G1, true);
SELECT e.title, e.location, e.is_private
  FROM event_assignees ea JOIN events e ON e.id = ea.event_id
  WHERE e.group_id = G1 AND ea.user_id = B;
-- => titre modifie apres depart | lieu secret | t
COMMIT;

-- R2 (constat 1) : X, étranger, avec app.group_id = G1
BEGIN;
SELECT set_config('app.user_id', X, true);
SELECT set_config('app.group_id', G1, true);
SELECT name FROM groups WHERE id = G1;                     -- => G1
SELECT count(*) FROM group_members WHERE group_id = G1;    -- => 1 (A)
UPDATE groups SET name = 'renomme par X' WHERE id = G1;    -- => UPDATE 1
INSERT INTO group_members (group_id, user_id, role) VALUES (G1, X, 'standard'); -- => INSERT 0 1
ROLLBACK;

-- R3 (constat 1, témoin) : X sans app.group_id
BEGIN;
SELECT set_config('app.user_id', X, true);
SELECT count(*) FROM groups;                               -- => 0
ROLLBACK;

-- R4 (constat 8)
BEGIN;
INSERT INTO audit_log (action, target_type, target_id) VALUES ('x', 'y', 'z');
UPDATE audit_log SET action = 'rewritten';                 -- => UPDATE 1
DELETE FROM audit_log;                                     -- => DELETE 1
ROLLBACK;
```
