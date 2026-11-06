# apps/mobile — l'application Android de MHome

Une coquille [Capacitor](https://capacitorjs.com) (#665) : une WebView qui
ouvre le site lui-même, à son adresse https (modèle A de
`docs/front-stack-study.md` § 2). `apps/web` tourne tel quel ; l'application
n'ajoute que ce qu'un navigateur ne donne pas, et ne contient aucune logique
du site.

| Fichier | Rôle |
|---|---|
| `package.json`, `package-lock.json` | Capacitor 8.6, `@capacitor/android`, `@capacitor/app` ; TypeScript pour lire la configuration |
| `capacitor.config.ts` | le site ouvert (`staging` ou `prod`), le nom, le marqueur de User-Agent, la page hors ligne |
| `www/hors-ligne.html` | la page affichée quand le site ne répond pas |
| `android/` | le projet Android (Gradle), versionné |
| `scripts/check-shell.test.mjs` | les contrôles de la coquille (`npm test`) |

## Ce que fait l'application

- **Le site, à son adresse.** La WebView charge `https://staging.watallo.fr`
  (variante `staging`) ou l'adresse de production donnée au build (variante
  `prod`). Les cookies du site restent dans la WebView, propres à
  l'application : ce ne sont pas ceux de Chrome. La connexion par email et
  mot de passe, la survie de la session au redémarrage et la messagerie en
  direct (WebSocket) restent à contrôler sur téléphone (§ 5).
- **Pas de connexion Google dans l'application.** La page de Google est sur
  une autre origine : elle s'ouvre dans Chrome, et la session qu'elle ouvre
  reste dans Chrome, pas dans l'application. Google refuse de toute façon
  son OAuth dans une WebView (`403 disallowed_useragent`). La connexion
  Google native arrive avec #669.
- **Les autorités de certification du système, et elles seules**
  (`android/app/src/main/res/xml/network_security_config.xml`), pour les deux
  variantes. Pas celles qu'un utilisateur installe dans les réglages du
  téléphone, aucune embarquée dans l'APK, aucun http en clair. Le staging a
  un certificat Let's Encrypt depuis #683 : il n'a plus besoin de l'autorité
  maison (`home-ca`), retirée.
- **Un marqueur dans le User-Agent** : la WebView ajoute ` MHomeApp` à son
  User-Agent. `apps/web` le reconnaît avec `mobile_app::is_mobile_app`
  (`apps/web/src/mobile_app.rs`), d'où `capacitor.config.ts` lit la
  constante : les deux ne peuvent pas diverger.
- **Le nom et les icônes du manifeste web** : le nom vient de
  `apps/web/src/brand.rs` (`NAME`), les couleurs aussi (`THEME_COLOR`,
  `BACKGROUND_COLOR`), et les icônes sont copiées depuis `apps/web/src/brand/`
  à chaque build (tâche Gradle `copyBrandIcons`).
- **Hors ligne** : si le site ne répond pas (pas de réseau, nom qui ne se
  résout pas, serveur muet), l'application affiche `www/hors-ligne.html`,
  servie depuis l'APK. Son bouton « Réessayer » recharge le site. Une page
  d'erreur du serveur (404, 500…) s'affiche telle quelle, pas la page hors
  ligne (`MainActivity.java`).
- **Le bouton retour** remonte l'historique de la WebView, puis ferme
  l'application.
- **Les liens vers un autre site** s'ouvrent dans le navigateur du
  téléphone, pas dans l'application.
- **La photo du code-barres** (Stocks → ajouter) ouvre l'appareil photo ;
  l'application demande la permission la première fois.

L'APK signé que les familles installent est publié en release GitHub
(#666) : « Publier un APK signé » et « Installer l'application
(familles) » plus bas. Hors de ce paquet : les rappels locaux (#668), la
connexion Google native (#669), iOS.

## Guide pas à pas : construire et installer l'APK `staging`

Écrit pour Linux ou WSL (Ubuntu 24.04), sans rien supposer d'Android. Chaque
étape dit ce qu'on doit voir. Les versions sont celles avec lesquelles ce
guide a été suivi le 2026-11-06.

### 1. Installer les outils

**Node.js 24** (22.18 au minimum : `package.json`, `engines`). Avec
[nvm](https://github.com/nvm-sh/nvm) :

```sh
nvm install 24
node --version      # v24.x.y
```

**Java 21** (Capacitor 8 compile en Java 21) :

```sh
sudo apt update
sudo apt install -y openjdk-21-jdk-headless unzip
java -version       # openjdk version "21.0.…"
```

**Le SDK Android en ligne de commande** (sans Android Studio) : les
« command-line tools » 19.0 (build 13114758), puis la plateforme Android 16
(API 36), les build-tools 36.0.0 et platform-tools (qui contient `adb`).

```sh
mkdir -p ~/android-sdk/cmdline-tools
cd /tmp
curl -fLo cmdline-tools.zip https://dl.google.com/android/repository/commandlinetools-linux-13114758_latest.zip
sha256sum cmdline-tools.zip
# 7ec965280a073311c339e571cd5de778b9975026cfcbe79f2b1cdcb1e15317ee  cmdline-tools.zip
unzip -q cmdline-tools.zip -d ~/android-sdk/cmdline-tools
mv ~/android-sdk/cmdline-tools/cmdline-tools ~/android-sdk/cmdline-tools/latest
```

Puis, dans `~/.bashrc` (et dans le terminal courant) :

```sh
export ANDROID_HOME="$HOME/android-sdk"
export PATH="$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH"
```

```sh
yes | sdkmanager --licenses              # accepte les licences Google
sdkmanager "platform-tools" "platforms;android-36" "build-tools;36.0.0"
sdkmanager --list_installed
# build-tools;36.0.0   | 36.0.0 | ...
# platform-tools       | 37.0.1 | ...
# platforms;android-36 | 2      | ...
adb --version                            # Android Debug Bridge version …
```

### 2. Construire l'APK

Depuis la racine du dépôt :

```sh
cd apps/mobile
npm ci
npm test
npx cap sync android
cd android
./gradlew assembleStagingDebug
```

À voir :

- `npm ci` finit par `found 0 vulnerabilities` ;
- `npm test` affiche `ℹ pass 13` et `ℹ fail 0` ;
- `npx cap sync android` finit par `✔ update android` et `[info] Sync
  finished` ; il écrit la configuration du staging dans
  `android/app/src/main/assets/capacitor.config.json` (`"url":
  "https://staging.watallo.fr"`) ;
- `./gradlew assembleStagingDebug` télécharge Gradle 8.14.3 et ses
  dépendances au premier lancement (plusieurs minutes), puis finit par
  `BUILD SUCCESSFUL`.

L'APK est `android/app/build/outputs/apk/staging/debug/app-staging-debug.apk`.
Il est signé d'une clé de débogage : installable par `adb` ou à la main, pas
publiable. L'APK publié est construit par la CI (« Publier un APK signé »).

**Variante `prod`** : l'adresse de production n'est pas encore fixée, elle
se donne au moment du `sync` (une origine https, rien après le nom) :

```sh
MHOME_TARGET=prod MHOME_PROD_URL=https://exemple.fr npx cap sync android
cd android && ./gradlew assembleProdDebug
```

Le build vérifie que le dernier `sync` correspond à la variante construite :
`assembleStagingDebug` après un `sync` de prod (ou l'inverse) échoue avec
`the synced capacitor.config.json opens …, not a staging site`. Refaire
alors le `sync` de la bonne variante. Les deux applications ont des
identifiants distincts (`fr.watallo.mhome.staging` et `fr.watallo.mhome`) et
peuvent cohabiter sur un téléphone.

### 3. Préparer le téléphone

Le staging n'est joignable que par le VPN : installer **Tailscale** sur le
téléphone, s'y connecter et le laisser allumé (guide local
`docs/guides/guide_tailscale.md`). Contrôle : Chrome du téléphone ouvre
`https://staging.watallo.fr` sans avertissement.

### 4a. Installer par câble USB (`adb`)

Sur le téléphone : Paramètres → À propos du téléphone → toucher sept fois
« Numéro de build » (« Vous êtes maintenant développeur »). Puis Paramètres →
Système → Options pour les développeurs → activer « Débogage USB ».
Brancher le câble, accepter « Autoriser le débogage USB ? ».

Sous **WSL**, le port USB n'est pas visible de Linux. Le plus simple : le
`adb` de Windows ([platform-tools pour
Windows](https://developer.android.com/tools/releases/platform-tools)),
lancé depuis PowerShell sur le fichier construit dans WSL
(`\\wsl$\Ubuntu\…\app-staging-debug.apk`). Sinon `usbipd-win` attache le
téléphone à WSL.

```sh
adb devices
# List of devices attached
# 1A2B3C4D        device        ("unauthorized" : accepter l'invite sur le téléphone)
adb install -r android/app/build/outputs/apk/staging/debug/app-staging-debug.apk
# Performing Streamed Install
# Success
```

### 4b. Installer sans câble

Copier l'APK sur le téléphone (Drive, câble en mode fichiers, mail à
soi-même), l'ouvrir depuis l'application Fichiers, autoriser « Installer des
applications inconnues » pour cette application quand Android le demande,
puis « Installer ».

### 4c. Sur un émulateur

Il faut la virtualisation (`ls /dev/kvm` doit exister ; sous WSL 2 elle
l'est en général) :

```sh
sdkmanager "emulator" "system-images;android-36;google_apis;x86_64"
echo no | avdmanager create avd -n mhome -k "system-images;android-36;google_apis;x86_64" -d pixel_7
$ANDROID_HOME/emulator/emulator -avd mhome &
adb wait-for-device
adb install -r android/app/build/outputs/apk/staging/debug/app-staging-debug.apk
```

L'émulateur passe par le réseau de la machine : si elle est sur Tailscale,
le staging répond.

### 5. Contrôler

L'icône **MHome** (la maison blanche sur fond pétrole) est dans la liste
des applications. À l'ouverture :

1. **La page de connexion du staging**, sans avertissement de certificat.
   Se connecter par email et mot de passe ; ouvrir la messagerie d'une
   famille et envoyer un message depuis un autre appareil : il arrive sans
   recharger (WebSocket).
2. **La session survit** : fermer l'application (balayer depuis les
   applications récentes), la rouvrir : on est toujours connecté.
3. **Mode avion**, puis rouvrir l'application : la page « Pas de
   connexion » avec le bouton « Réessayer ». Couper le mode avion, toucher
   « Réessayer » : le site revient.
4. **Retour** : ouvrir une page puis une autre, le bouton retour remonte
   l'une après l'autre, puis ferme l'application.
5. **Lien externe** : un lien vers un autre site s'ouvre dans Chrome, et
   l'application reste sur sa page.
6. **Photo** : Stocks → ajouter → « Photographier le code » ouvre
   l'appareil photo (Android demande la permission la première fois).
7. **Le pont Capacitor sous la CSP du site.** Le téléphone branché en USB,
   ouvrir `chrome://inspect` dans Chrome sur l'ordinateur : la page du
   staging apparaît sous « WebView in fr.watallo.mhome.staging » (APK de
   débogage seulement) → « inspect ». Dans la console :

   ```js
   Capacitor.isNativePlatform()          // true
   await Capacitor.Plugins.App.getInfo() // {name: 'MHome', id: 'fr.watallo.mhome.staging', …}
   navigator.userAgent                   // "… Mobile Safari/537.36 MHomeApp"
   ```

   et aucune ligne « Refused to execute … Content Security Policy » dans la
   console après un rechargement de la page. Le pont est injecté par la
   WebView (`addDocumentStartJavaScript`), pas par un `<script>` dans la
   page : la CSP du site (`script-src 'self'`) ne le voit pas. Une WebView
   trop ancienne pour cette fonction ferait
   retomber Capacitor sur un script inline, que la CSP bloquerait :
   mettre à jour « Android System WebView » depuis le Play Store.

## Publier un APK signé (#666)

Ce que les familles installent : la variante `prod`, en release, signée
d'une clé qui ne change jamais. Android n'installe une mise à jour par-dessus
l'application que si elle est signée de la même clé et que son
`versionCode` est plus grand ; sinon il faut désinstaller, et l'application
perd ses données (la session, et les rappels locaux une fois #668 livré).

Le workflow `.github/workflows/mobile-release.yml` fait le reste à chaque
tag `mobile-vX.Y.Z` : `npm ci`, `npm audit --audit-level=high` (bloquant),
`npm test`, `npx cap sync android` vers la production, `./gradlew
assembleProdRelease` signé, contrôle de la signature et de la version, puis
une release GitHub du tag avec `mhome-X.Y.Z.apk` et `mhome-X.Y.Z.apk.sha256`.
La version vient du tag : `versionName` X.Y.Z, `versionCode`
X × 1 000 000 + Y × 1 000 + Z (`scripts/release-version.mjs`). Pas de
suffixe (`-rc.1`), mineure et correctif jusqu'à 999, et un tag qui n'est
pas au-dessus de tous les `mobile-v…` déjà poussés est refusé : aucun
téléphone ne pourrait l'installer par-dessus.

La variante `staging` n'a pas de release : elle ouvre le serveur de test,
joignable par le VPN seulement. `./gradlew assembleStagingRelease` échoue
avec `the staging flavour has no release build`.

La clé n'est jamais dans le dépôt : elle vit chez vous, et une copie
encodée dans un secret GitHub. C'est vous qui faites les étapes 1 à 5, une
seule fois.

### 1. Générer la clé

Dans un dossier **hors du dépôt**, avec Java 21 (`keytool` est livré avec,
§ 1 du guide ci-dessus) :

```sh
mkdir -p ~/mhome-release && cd ~/mhome-release
keytool -genkeypair -v -keystore mhome-release.p12 -storetype PKCS12 \
  -alias mhome -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=MHome"
```

`keytool` demande deux fois le mot de passe (« Enter keystore password »,
« Re-enter new password ») : en choisir un long, généré par le gestionnaire
de mots de passe. Il finit par `[Storing mhome-release.p12]`.

- `-alias mhome` : le build cherche la clé sous ce nom
  (`android/app/build.gradle`).
- `-dname "CN=MHome"` : ce nom est lisible par quiconque a l'APK. Ne pas y
  mettre de nom de personne.
- `-validity 10000` : environ 27 ans ; une clé expirée ne signe plus de mise
  à jour.
- Un seul mot de passe : un fichier PKCS12 protège la clé et le fichier du
  même mot de passe.

### 2. Ranger la clé et son mot de passe

- **Le gestionnaire de mots de passe** : une entrée « MHome — clé de
  signature Android » qui porte le mot de passe et, en pièce jointe, le
  fichier `mhome-release.p12`.
- **Une copie hors ligne** : le fichier sur une clé USB rangée ailleurs que
  l'ordinateur, le mot de passe noté avec (ou sur papier, à part).
- Jamais dans le dépôt (`android/.gitignore` ignore `*.p12`, `*.jks`,
  `*.keystore`), jamais dans un mail ni un dossier synchronisé partagé.

**Si la clé ou son mot de passe sont perdus**, plus aucune mise à jour n'est
possible : une nouvelle clé donne une autre application aux yeux
d'Android, et chaque famille doit désinstaller l'ancienne (et perdre ses
données) pour installer la nouvelle. GitHub n'aide pas : un secret ne se
relit pas. **Si la clé fuit**, quelqu'un peut signer un APK qu'Android
installera par-dessus le vôtre : supprimer les secrets, générer une
nouvelle clé, et prévenir les familles qu'elles doivent réinstaller.

### 3. Lire les empreintes

```sh
keytool -list -v -keystore ~/mhome-release/mhome-release.p12 -alias mhome
```

Après le mot de passe, sous « Certificate fingerprints » :

```
	 SHA1: 12:34:…:AB
	 SHA256: 12:34:56:…:EF
```

Le **SHA-256** identifie l'APK publié : il se recopie dans la ligne
ci-dessous (dans une PR), en majuscules avec les `:`, telle que `keytool`
l'affiche, **avant** le premier tag. Le workflow lit cette ligne et refuse
de publier un APK signé d'une autre clé, ou si elle n'est pas remplie. Ne
pas changer la forme de la ligne.

Empreinte SHA-256 du certificat de signature : `aucune clé générée`

Le **SHA-1** sert à la connexion Google native (#669) : c'est l'empreinte
que Google Cloud demande pour un client OAuth Android.

### 4. Renseigner les secrets

Sur GitHub, page du dépôt → **Settings** → **Secrets and variables** →
**Actions** :

- onglet **Secrets** → **New repository secret** :
  - `MHOME_KEYSTORE_BASE64` : le fichier encodé en base64, sur une ligne
    (`base64 -w0 ~/mhome-release/mhome-release.p12` l'affiche) ;
  - `MHOME_KEYSTORE_PASSWORD` : le mot de passe ;
- onglet **Variables** → **New repository variable** :
  - `MHOME_PROD_URL` : l'adresse que l'application ouvre, une origine https
    sans chemin (`https://exemple.fr`). Ce n'est pas un secret.

Ou, avec `gh` (connecté au compte propriétaire du dépôt) :

```sh
base64 -w0 ~/mhome-release/mhome-release.p12 | gh secret set MHOME_KEYSTORE_BASE64 --repo maelprog/manage_our_home
gh secret set MHOME_KEYSTORE_PASSWORD --repo maelprog/manage_our_home   # demande la valeur ; la coller
gh variable set MHOME_PROD_URL --body https://exemple.fr --repo maelprog/manage_our_home
gh secret list --repo maelprog/manage_our_home     # les deux noms, sans leur valeur
```

Il manque l'un des trois : le workflow s'arrête à sa première étape en
nommant celui qui manque. Le build Gradle fait de même hors CI
(`MHOME_KEYSTORE_FILE`, `MHOME_KEYSTORE_PASSWORD`, `-PmhomeVersionName`,
`-PmhomeVersionCode`).

### 5. Publier

Une fois la ligne d'empreinte fusionnée sur `main` :

```sh
git fetch origin
git tag mobile-v1.0.0 origin/main
git push origin mobile-v1.0.0
```

Onglet **Actions** → « Mobile release » : le run finit vert, et la release
`mobile-v1.0.0` porte `mhome-1.0.0.apk` et `mhome-1.0.0.apk.sha256`. Pour
un premier essai, deux tags de test (`mobile-v0.1.0` puis `mobile-v0.1.1`)
suffisent à vérifier que la seconde s'installe par-dessus la première sans
désinstaller ; `mobile-v1.0.0` passera au-dessus des deux.

Contrôle sur l'ordinateur, avec les build-tools du § 1 :

```sh
sha256sum -c mhome-1.0.0.apk.sha256        # mhome-1.0.0.apk: OK
$ANDROID_HOME/build-tools/36.0.0/apksigner verify --print-certs mhome-1.0.0.apk
# Signer #1 certificate SHA-256 digest: 1234…ef   (l'empreinte ci-dessus, en minuscules, sans les « : »)
```

## Installer l'application (familles)

Ce qu'il faut : un téléphone Android 7 ou plus récent, et l'adresse de la
page des releases (`https://github.com/maelprog/manage_our_home/releases`).

1. **Télécharger** : sur le téléphone, ouvrir la dernière release
   « MHome Android », toucher `mhome-X.Y.Z.apk` dans « Assets ».
2. **Vérifier le fichier** (conseillé pour la première installation) : sur
   un ordinateur, calculer le SHA-256 de l'APK et le comparer à celui des
   notes de la release (et du fichier `.sha256`) :
   - Linux : `sha256sum mhome-X.Y.Z.apk` ;
   - macOS : `shasum -a 256 mhome-X.Y.Z.apk` ;
   - Windows (PowerShell) : `Get-FileHash mhome-X.Y.Z.apk -Algorithm SHA256`.

   Les deux doivent être identiques. Ce contrôle dit que le fichier n'a pas
   été abîmé ou remplacé en route ; il ne dit pas qui l'a publié, puisque
   l'empreinte est sur la même page. Ce qui le dit, c'est le certificat :
   `apksigner verify --print-certs` (§ 5 ci-dessus) doit donner l'empreinte
   de ce README.
3. **Autoriser l'installation** : ouvrir le fichier téléchargé. Android
   répond que le navigateur n'a pas le droit d'installer des applications :
   **Paramètres** → activer **Autoriser cette source** (« Installer des
   applications inconnues »), revenir, puis **Installer**. Play Protect peut
   signaler une application qu'il ne connaît pas : c'est le cas de toute
   application installée hors du Play Store.
4. **Retirer l'autorisation** une fois installée : Paramètres →
   Applications → Chrome (ou le navigateur utilisé) → « Installer des
   applications inconnues » → désactiver. Les libellés varient d'un
   fabricant à l'autre.

**Mettre à jour** : même chemin avec l'APK de la nouvelle release ; Android
propose « Mettre à jour » et l'application garde sa session. Il refuse une
mise à jour signée d'une autre clé (« conflit avec un package existant ») :
ne pas désinstaller pour la forcer, et prévenir l'administrateur.

## La CI

Le job `mobile` de `.github/workflows/ci.yml` lance `npm ci`,
`npm audit --audit-level=high` (bloquant, sans liste d'ignore, comme
`cargo audit`), `npm test`, `npx cap sync android` puis
`./gradlew assembleStagingDebug`. Le workflow `mobile-release.yml` publie
l'APK signé (« Publier un APK signé »). Renovate tient `package.json` à
jour (`renovate.json`), les paquets `@capacitor/*` ensemble, sans
changement de version majeure (une majeure de Capacitor se migre à la
main, `npx cap migrate`). Les dépendances Gradle d'`android/` ne sont
suivies ni par Renovate ni par un audit.
