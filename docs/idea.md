Application familiale 
agenda événements réguliers 
taches avec rappel (récupérer drive) — voir clarification: c'est un type d'événement agenda, pas une entité séparée
user avec rôles 
auth ??
user admin ?? — voir clarification: superadmin technique global, distinct de l'admin de groupe
liste de course — voir clarification: liée aux recettes + stocks
idees recettes 
variees avec ce qui a été mangé les 2 dernières semaines, + produits de saison
scan frigo ? — voir clarification: reporté, stocks saisis manuellement en v1
+ stocks ? — voir clarification
messagerie — voir clarification: un fil par famille, texte seul
gestion budget ? — voir clarification: lié à la liste de courses
groups handling — voir clarification: un user peut appartenir à plusieurs groupes/familles

The application is rgpd compliant, and each data is encrypted. The key is stored according to software Dev/devOps best practices.

Auth: 
The auth can be done by Google account or by email + password. A forgotten password handling allows the user to recover his password by sending a mail to the mail of the account. In user settings, the user authentified by email + password can change to a new password by entering the current and the new one. 

Groups:
Create button: User can create groups. When creating a group, the user chooses a name. 

Owner: the user creating the group is automatically owner and admin for it. When leaving the group, the owner must declare another user owner of the group. Only the owner has the rights to delete the group. Owner also has admin rights. Owner role cannot be removed, except by leaving the group. Only one owner at a time. Owner can add and remove admin role. Owner user can remove admin users from the group.

Admin: admin rights can be given by the owner or another admin. Admin role can only be removed by the owner. Admin user can remove standard users from the group. Admin can create invitation link to the group. 

Standard: A standard user has access to all group functionalities, except user management.

Agenda:
An agenda is attributed to a group. A user can only access group information if he is in the group.

Add button: A user can add an event by clicking in the corresponding time on the calendar. There is also an "add event" button at the top of the calendar.
Add settings: A user specifies the date, if the event takes the whole day, if not, the time. He can attribute a/multiple user to the event. By default the user who created it is added. He can add a notification Xmin/h/day before the event, that will be sent to the attributed users. The user can be set as recurring (daily/monthly/yearly/custom), and will automatically be added to the calendar. Files can also be added to the related event. The user can write a note to add to the event.
Edit button: clicking on an event shows the current settings. An "Edit" button is available, to modify the event.
Edit settings: all fields are editable. When modifying a recurring event, the user chooses to modify all, this one, or this one and following.
Delete: A "delete" button is available when displaying event information, or when editing an event. When deleting a recurring event, the user chooses to delete all/this one/this one and following. When not a recurring event, a confirmation dialog opens.

Agenda apis:
The agenda can add and synchronize in real time with other agendas like Google agenda. 
When importing an agenda, the user chooses the attributed user(s). By default the adding user is attributed.
When exporting, only the events attributed to the user are exported.

## Clarifications (epic scoping)

- **Groups** : un user peut appartenir à plusieurs groupes/familles (non précisé jusqu'ici, à ajouter au modèle de données Auth+Groups).
- **Tâches avec rappel** : un type d'événement agenda (pas une entité séparée), avec les mêmes mécanismes de rappel/récurrence/assignation que les events classiques.
- **Stocks** : saisie manuelle en v1 (le scan frigo par vision viendra automatiser/enrichir la saisie dans une epic séparée plus tard). Seuil de réassort défini par article, partagé au niveau famille (pas de personnalisation par user sur le seuil lui-même).
- **Liste de courses** : une seule liste par famille (pas par user). Alimentée automatiquement par (a) les ingrédients manquants d'une recette choisie comparés aux stocks, et (b) des items récurrents à racheter (custom par user) basés sur le niveau de stock. Dépend de Stocks et de Recettes (qui doit produire une liste d'ingrédients structurée) pour être spec'able.
- **User admin** : un superadmin technique global (l'administrateur de la plateforme), distinct des rôles owner/admin/standard scopés au groupe. Recoupe le rôle "administrateur technique / mainteneur" déjà mentionné dans architecture.md (section RGPD). À cadrer comme sa propre epic.
- **Messagerie** : un seul fil de discussion par famille (pas de DM entre users). Texte seul en v1, pièces jointes reportées à une version ultérieure.
- **Budget** : lié à la liste de courses — prix saisis manuellement par item, avec cumul par période. Pas un suivi de dépenses générales (loyer, factures, etc.) indépendant.
- **Rappels d'événement, mobile, hors ligne (sine qua non, 2026-09-03)** : la
  notification d'un rappel (réglage `Add settings` ci-dessus) doit se
  déclencher sur l'app mobile même sans réseau au moment prévu. Vérifié
  compatible avec le front Rust actuel via le plugin natif Local Notifications
  de Capacitor — détail dans `architecture.md` (Questions résolues #4) et
  `front-stack-study.md` § 2.3.

Ordre de dépendance recommandé pour le spec : Groups (multi-famille) → Agenda → Stocks → Recettes → Liste de courses → Budget. Messagerie et User admin sont indépendants et peuvent être spec'és à tout moment après Groups.

