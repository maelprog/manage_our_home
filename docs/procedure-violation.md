# Procédure en cas de violation de données — Manage Our Home

Marche à suivre au titre des articles 33 et 34 du RGPD. Arrêtée par le
responsable de traitement le 2026-10-02 (#143). Le registre où chaque
incident est consigné est décrit dans `docs/registre-violations.md` ;
l'analyse des risques qui fonde les seuils ci-dessous, dans `docs/aipd.md`.

**Une violation de données personnelles**, c'est toute atteinte à la
sécurité qui entraîne, de façon accidentelle ou illicite, la destruction,
la perte, l'altération, la divulgation ou l'accès non autorisé à des données
personnelles (art. 4(12)) : une fuite (confidentialité), mais aussi une
modification non voulue (intégrité) ou une perte, même temporaire, faute de
sauvegarde (disponibilité).

## Qui fait quoi

Le **responsable de traitement, seul**, constate, qualifie et décide. Le
service n'a ni équipe ni délégué à la protection des données : il n'y a
personne d'autre à qui déléguer une étape, et aucune étape n'attend l'avis
d'un tiers. Un signalement peut venir de n'importe qui — un membre, une
personne invitée, le sous-traitant email (tenu de prévenir « dans les
meilleurs délais », art. 33(2)), un chercheur en sécurité — et arrive à
[adresse de contact — à renseigner avant la mise en ligne].

## Les étapes

1. **Constater et dater.** Ouvrir aussitôt une entrée au registre des
   violations, avec l'heure à laquelle le responsable a acquis une certitude
   raisonnable qu'un incident de sécurité a touché des données
   personnelles. **C'est cette heure qui fait partir les 72 heures**, pas
   celle de l'incident ni celle de la fin de l'analyse.
2. **Contenir.** Couper l'accès en cause avant d'en mesurer l'étendue. Les
   leviers dont le service dispose :
   - révoquer les sessions, sur les comptes touchés ou sur tous. La table
     `sessions` ne garde que l'empreinte SHA-256 du jeton que porte le
     cookie (#222) : une copie de la table ne suffit pas à se connecter.
     La révocation reste le levier quand ce sont les cookies eux-mêmes qui
     ont pu fuir (poste d'un membre, serveur compromis en marche) ;
   - supprimer les jetons porteurs encore valables, stockés en clair :
     après une fuite de la base, **chacun est utilisable par qui tient la
     copie**. Un jeton d'invitation (`invitations.token`, valable
     7 jours) fait entrer n'importe quel compte dans le groupe, quelle que
     soit l'adresse invitée ; un jeton de réinitialisation
     (`password_reset_tokens`, 1 h) donne le compte ; un jeton de
     vérification (`email_verification_tokens`, 24 h) valide une adresse.
     Les invitations sont ensuite à réémettre, les demandes de
     réinitialisation et de vérification à refaire.

   Les commandes, **connecté en `migration_role`**
   (`MIGRATION_DATABASE_URL`) :

   ```sql
   BEGIN;
   -- Garde : sans BYPASSRLS, la RLS forcée d'`invitations` cache les
   -- lignes au SELECT comme au DELETE. On s'arrête plutôt que de
   -- « réussir » sans rien supprimer.
   SELECT current_user, rolsuper OR rolbypassrls AS contourne_rls
     FROM pg_roles WHERE rolname = current_user;
   DO $$
   BEGIN
     IF NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles
             WHERE rolname = current_user) THEN
       RAISE EXCEPTION 'rôle % sans BYPASSRLS : se reconnecter en migration_role',
         current_user;
     END IF;
   END $$;
   SELECT
     (SELECT count(*) FROM sessions WHERE revoked_at IS NULL),
     (SELECT count(*) FROM invitations
       WHERE consumed_at IS NULL AND expires_at > now()),
     (SELECT count(*) FROM password_reset_tokens
       WHERE consumed_at IS NULL AND expires_at > now()),
     (SELECT count(*) FROM email_verification_tokens
       WHERE consumed_at IS NULL AND expires_at > now());
   UPDATE sessions SET revoked_at = now() WHERE revoked_at IS NULL;
   DELETE FROM invitations
     WHERE consumed_at IS NULL AND expires_at > now();
   DELETE FROM password_reset_tokens
     WHERE consumed_at IS NULL AND expires_at > now();
   DELETE FROM email_verification_tokens
     WHERE consumed_at IS NULL AND expires_at > now();
   COMMIT;
   ```

   **Le rôle compte.** `invitations` est sous une politique RLS forcée : sur
   le rôle d'exécution de l'API (sans `BYPASSRLS`), le `SELECT` comme le
   `DELETE` ne voient aucune ligne, le `DELETE` répond `DELETE 0` sans
   erreur, et les jetons survivent au confinement — sans qu'aucun écart de
   compte ne le trahisse. C'est pourquoi le bloc commence par contrôler le
   rôle : `contourne_rls` doit valoir `true`. Sinon le `DO` lève une
   erreur, la transaction est annulée et rien n'est touché ; se reconnecter
   en `migration_role` et recommencer. Une fois le rôle vérifié, les
   comptes du premier `SELECT` disent combien de jetons valables existaient
   (à reporter au registre), et chaque `UPDATE`/`DELETE` doit toucher le
   même nombre de lignes. Le filtre ne retire que les jetons encore
   utilisables : une invitation expirée reste, parce qu'elle figure dans
   l'export du compte dont elle porte l'adresse (art. 15,
   `account_export_received_invitations`) jusqu'à sa purge à 30 jours, et
   elle ne fait plus entrer personne ;
   - renouveler les secrets exposés : `OAUTH_ENCRYPTION_KEY`,
     `MESSAGE_ENCRYPTION_KEY`, `CALENDAR_FEED_ENCRYPTION_KEY` (ce qui
     suppose de rechiffrer les colonnes concernées), les identifiants
     Postgres, MinIO et SMTP, la clé VAPID (`VAPID_PRIVATE_KEY`) ;
   - arrêter l'API, en dernier recours.
3. **Qualifier.** Répondre par écrit, dans l'entrée du registre : quelles
   tables, quels objets du stockage, quels comptes ; confidentialité,
   intégrité ou disponibilité ; les données ont-elles quitté le serveur ;
   étaient-elles chiffrées, **et la clé est-elle restée hors d'atteinte** ?
   Une colonne chiffrée par `pgcrypto` dont la clé a fui avec elle compte
   comme non chiffrée. Un mot de passe haché (argon2) n'est pas chiffré.
   Les pièces jointes ne sont pas chiffrées par l'application.
4. **Décider de la notification à la CNIL** (art. 33). Elle est due, sous
   72 heures, sauf si la violation n'est **pas susceptible** d'engendrer un
   risque pour les droits et libertés des personnes. Le doute se résout en
   notifiant. Elle se fait par le téléservice de notification de la CNIL ;
   ce qui n'est pas encore connu à la 72e heure se complète ensuite
   (art. 33(4)), et une notification en retard dit pourquoi. Une décision
   de ne pas notifier s'écrit au registre avec son motif.
5. **Prévenir les personnes** (art. 34) — voir le seuil ci-dessous.
6. **Clore l'entrée** : cause, mesures prises pour éviter que cela se
   reproduise, et la date de clôture.

## Quand les personnes sont prévenues

Le RGPD n'impose d'informer directement les personnes que si la violation
présente un **risque élevé** (art. 34(1)), et en dispense lorsque les données
étaient chiffrées de façon à rester inintelligibles (art. 34(3)(a)). Le
responsable de traitement a retenu un seuil **plus bas** (arbitrage du
2026-10-02) :

> **Tout compte touché est prévenu par email dès qu'une donnée personnelle
> non chiffrée a fui, même si le risque est jugé faible.**

- « A fui » : une atteinte à la confidentialité — les données ont été lues
  ou copiées par quelqu'un qui n'en avait pas le droit, ou ont pu l'être.
- « Non chiffrée » : au sens de l'étape 3 — clé compromise, hachage ou
  pièce jointe comptent comme non chiffrés.
- L'email part à l'adresse du compte, dans les meilleurs délais, sans
  attendre la décision de la CNIL. Il dit, en termes simples, ce qui s'est
  passé, quelles données du destinataire sont en cause, les conséquences
  probables, ce qui a été fait, ce que la personne peut faire (changer son
  mot de passe, se méfier d'un hameçonnage), et l'adresse de contact
  (art. 34(2)). Aucune fonction de l'application n'envoie un tel email : il
  part à la main, par le relais SMTP du service.
- Un compte purgé n'a plus d'adresse : il ne peut pas être prévenu, et
  l'entrée du registre le dit.

Hors de ce seuil — atteinte à l'intégrité ou à la disponibilité, fuite de
données restées chiffrées — c'est la règle de l'art. 34 qui s'applique :
les personnes sont prévenues si le risque est élevé.

**Ce que l'arbitrage ne tranche pas** : une personne **sans compte** dont
l'adresse figure dans une invitation (`invitations.invited_email`). Elle relève
aujourd'hui de l'art. 34 seul, c'est-à-dire prévenue à cette adresse si le
risque est élevé.
