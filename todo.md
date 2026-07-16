# TODO — env de test local pour infra/ (session 2026-07-10)

Objectif : pouvoir tester la solution simplement en local via
`cd infra && cp .env.example .env && docker compose up --build`.

## Fait

- **`infra/.env.example`** (nouveau) : toutes les variables consommées par
  `docker-compose.yml`, avec des valeurs de test qui permettent à l'API de
  booter (SMTP factice mais `SMTP_FROM` parsable, clés de chiffrement =
  simples passphrases `pgp_sym_encrypt`, `PUBLIC_BASE_URL=http://localhost`
  car Caddy écoute sur :80, `SECURE_COOKIES=false`). Caveats documentés en
  tête de fichier (emails non envoyés, OAuth Google non fonctionnel sans
  vraies creds).
- **`infra/postgres/init/01-admin-role.sh`** (nouveau) : le compose montait
  `./postgres/init` qui n'existait pas, alors que l'API se connecte en
  `admin_role` (BYPASSRLS) **au démarrage** → crash sinon. Le script crée le
  rôle au premier boot de postgres + `ALTER DEFAULT PRIVILEGES` (les tables
  n'existent pas encore, les migrations sqlx tournent au boot de l'API).
- **`infra/docker-compose.yml`** :
  - `ADMIN_ROLE_PASSWORD` passé au service postgres (pour le script d'init) ;
  - `SECURE_COOKIES: ${SECURE_COOKIES:-true}` (était hardcodé `"true"`,
    cookies potentiellement refusés en http local) ;
  - service one-shot `minio-init` (image `minio/mc`) qui crée le bucket
    `manage-our-home` (ni MinIO ni le client S3 de l'API ne le créent).
- **`infra/generate-env.sh`** réaligné : il générait des variables que le
  compose ne lit pas (`APP_ENCRYPTION_KEY`, `SESSION_ENCRYPTION_KEY`,
  `DATA_ENCRYPTION_KEY`, `JWT_SECRET`, `COOKIE_SECRET`, `SMTP_USER` au lieu
  de `SMTP_USERNAME`…) → même jeu de variables que `.env.example`, secrets
  aléatoires. Mots de passe en hex (base64 `+/=` casse les URLs
  `postgres://`).
- Validé : `docker compose --env-file .env.example config` se résout
  proprement ; `generate-env.sh` produit un `.env` complet (testé en
  scratchpad).

## Reste à faire

- [ ] **Vérifier le stack en conditions réelles** : bloqué par les
      permissions Docker dans ce WSL — daemon démarré via
      `sudo service docker start`, mais l'utilisateur `dev` n'est pas dans
      le groupe `docker` (socket `root:docker`). Fix :
      `sudo usermod -aG docker dev` puis nouvelle session (ou `newgrp
      docker`). Ensuite :
      ```sh
      cd infra && docker compose --env-file .env.example -p mom-envtest up -d postgres minio minio-init
      # vérifier : rôle admin_role créé, bucket manage-our-home créé
      docker compose -p mom-envtest down -v   # nettoyage
      ```
- [ ] Test complet `cp .env.example .env && docker compose up --build`
      (build des images api/web inclus) et un tour dans l'app sur
      http://localhost.
- [ ] Commit (branche actuelle : `tmp/machine-migration-2026-07-10` —
      probablement créer une branche dédiée + PR vers `main`).
