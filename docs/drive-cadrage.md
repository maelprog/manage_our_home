# Cadrage — exporter la liste de courses vers une enseigne (voie 1)

État au 2026-11-20. Ce document est un **constat et une recommandation**, pas un
plan d'implémentation. Il répond à l'issue #912 : la demande du 2026-11-17 et
l'arbitrage du même jour retiennent la **voie 1**, un export de la liste de
courses vers l'enseigne, sans accord partenaire ni compte enseigne. Les voies 2
(accord partenaire) et 3 (automatisation avec le compte de l'utilisateur) sont
écartées pour la v1 et ne sont pas étudiées ici.

---

## 0. Réponse courte

- **Aucune enseigne n'a pu être établie comme acceptant un ajout au panier par
  URL ou un import de liste sans compte.** Ce que la voie 1 peut promettre,
  c'est d'ouvrir une **recherche** chez l'enseigne, pas de remplir un panier.
- **Une seule URL de recherche est déclarée par l'enseigne elle-même** :
  Auchan (`https://www.auchan.fr/recherche?text=<terme>`, `SearchAction` de sa
  page d'accueil). Chronodrive déclare un chemin `/search/*` ouvert par son
  application iOS, sans que le format exact du terme soit établi.
- **Cinq sites bloquent tout accès non navigateur dès la page d'accueil**
  (Carrefour, Monoprix et Système U derrière Cloudflare, Intermarché et Leclerc
  Drive derrière DataDome) : non établi, pas contourné.
- **Qualité des résultats : non établie pour toutes les enseignes.** Chaque
  enseigne qui répond interdit sa page de recherche aux robots dans son
  `robots.txt` ; aucun essai de recherche n'a donc été fait par un outil. Le
  protocole d'essai manuel est au § 6.
- **Texte à envoyer** : le nom tel que saisi, pas le nom canonique de la
  taxonomie (§ 3).
- **Recommandation v1** (§ 4) : une liste copiable et partageable, valable pour
  toutes les enseignes, plus un lien « Rechercher chez <enseigne> » par ligne
  pour les seules enseignes dont le format est établi ; l'enseigne choisie dans
  les réglages de la famille, aucun magasin stocké en v1.

---

## 1. Méthode

- Pour chaque enseigne, dans cet ordre : `robots.txt`, puis la page d'accueil,
  puis les fichiers d'association d'applications
  (`/.well-known/apple-app-site-association`, `/.well-known/assetlinks.json`),
  puis les CGU. Une requête par fichier, sans boucle de réessai ni script
  d'exploration ; les `robots.txt` ont été relevés en une commande, une
  requête par site, le 2026-11-20.
- **Aucune page qu'un `robots.txt` interdit n'a été demandée** : la page de
  recherche est interdite aux robots chez E.Leclerc, Intermarché, Auchan,
  Monoprix et Chronodrive (§ 2). Le format de leur URL de recherche n'est donc
  établi que lorsque l'enseigne le déclare elle-même dans une page autorisée.
- Une réponse anti-robot (défi Cloudflare, captcha DataDome) est notée telle
  quelle : aucune nouvelle tentative, aucun changement d'outil pour passer.
- Aucune connexion à un compte, aucun identifiant d'enseigne utilisé.
- Les sources secondaires (presse, comparateurs, extraits de moteur de
  recherche) sont signalées comme telles ; elles ne suffisent pas à classer une
  ligne « accepte ».

Un lien « Rechercher chez <enseigne> » est suivi par la personne qui clique,
dans son navigateur : ce n'est pas une exploration par un robot, et
`robots.txt` ne la régit pas. Ce qui la régit, ce sont les CGU (§ 2.2). En
revanche le serveur de manage_our_home ne doit **jamais** demander lui-même ces
URL (ni aperçu, ni préchargement, ni vérification de lien) : ce serait un robot,
et il tomberait sous ces `robots.txt`.

---

## 2. Ce que chaque enseigne accepte sans compte ni accord

Légende des cellules : **accepte** / **n'accepte pas** / **non établi**, suivi
de ce qui le fonde. Toutes les observations directes datent du 2026-11-20.

### 2.1 Tableau

| Enseigne | URL de recherche | Magasin requis | Lien profond appli | Import de liste | Ajout panier par URL | Qualité des résultats (10 ingrédients) | Source ou essai | Date |
|---|---|---|---|---|---|---|---|---|
| Carrefour | non établi : page d'accueil bloquée | non établi | non établi | non établi | non établi | non établi | `robots.txt` lu (200) : la recherche n'y est pas interdite, `/set-store`, `/get-store`, `/webview`, `/g`, `/b` le sont ; page d'accueil : 403, défi Cloudflare (`_cf_chl_opt`), non contourné | 2026-11-20 |
| E.Leclerc (Leclerc Drive) | non établi : `www.e.leclerc/recherche` existe mais est interdit aux robots ; `www.leclercdrive.fr`, qui sert le drive, est bloqué | non établi | non établi : `www.e.leclerc` n'a aucun fichier d'association d'applications (404 pour les deux) ; celui de `leclercdrive.fr` est interdit par son `robots.txt` | non établi : un mémo de liste en texte libre existerait dans l'appli Leclerc Drive (source secondaire : Clubic), sans import documenté | non établi | non établi | `robots.txt` des deux sites lus (200) ; `www.e.leclerc` : 200, application monopage sans `SearchAction` dans le HTML servi ; `www.leclercdrive.fr` : 403, captcha DataDome, non contourné | 2026-11-20 |
| Intermarché | non établi : `/recherche/*` et `/rechercheproduits/*` interdits aux robots ; page d'accueil bloquée | non établi : `/magasins/*` interdit aux robots, `/?pdvref*` aussi (un paramètre de point de vente existe, indice seulement) | non établi | non établi | non établi | non établi | `robots.txt` lu (200) ; page d'accueil : 403, captcha DataDome, non contourné | 2026-11-20 |
| Système U | non établi | non établi | non établi | non établi | non établi | non établi | `www.coursesu.com/robots.txt` : 403, défi Cloudflare ; rien d'autre demandé | 2026-11-20 |
| Auchan | **accepte** : `https://www.auchan.fr/recherche?text=<terme>` (`SearchAction` déclarée dans la page d'accueil ; formulaire `GET /recherche`, champ `text`) | non établi : la page d'accueil propose « Choisir vos courses » ; `robots.txt` interdit `/*storeReference=*` (un paramètre de magasin existe, indice seulement) | **accepte** (iOS) : l'application `com.auchan.auchan` revendique tous les chemins (`*`) hormis une liste d'exclusions qui ne contient pas `/recherche` ; Android : `assetlinks.json` délègue `handle_all_urls` à `com.auchan.android`, chemins non établis | non établi | non établi | non établi : `/recherche*` interdit aux robots | `robots.txt`, page d'accueil (200), `apple-app-site-association` et `assetlinks.json` (200) ; CGU du 12/06/2026 lues (§ 2.2) | 2026-11-20 |
| Monoprix | non établi : `/search/` et `/?q=` interdits aux robots ; page d'accueil bloquée | non établi | non établi | non établi | non établi | non établi | `robots.txt` lu (200) ; page d'accueil : 403, « Just a moment... », défi Cloudflare, non contourné | 2026-11-20 |
| Chronodrive | non établi pour le format exact : le chemin `/search/*` est revendiqué par l'appli iOS ; le formulaire de la page d'accueil est en `POST` ; `/search` est interdit aux robots | non établi : `robots.txt` interdit `/*shopId` à Googlebot (un paramètre de magasin existe, indice seulement) | **accepte** (iOS) : `apple-app-site-association` liste `/search/*`, `/product/*`, `/category/*` pour l'appli `fr.chronodrive.vad` ; Android : `assetlinks.json` délègue `handle_all_urls` à `com.ikomobi.chronodrive`, chemins non établis | non établi : un « mémo » existe (`/memo`, `/mylistes` dans `robots.txt` ; alimenté par l'objet Izy selon le Journal du Net), import depuis une appli tierce non documenté | non établi | non établi : `/search` interdit aux robots | `robots.txt`, page d'accueil (200), `assetlinks.json` et `apple-app-site-association` (200) ; CGV lues (§ 2.2) | 2026-11-20 |

Aucune enseigne n'est classée « n'accepte pas » : aucune source ne dit qu'une
fonction est refusée, seulement qu'elle n'a pas pu être observée.

### 2.2 Ce que disent les CGU sur les liens et les robots

- **Auchan** — CGU du site auchan.fr, « mises à jour le 12/06/2026 »
  (`https://www.auchan.fr/cgu/ep-cgu`), lues le 2026-11-20. Article 2 : usage
  privé du site. Article 8 : l'usage de systèmes automatisés « visant à extraire
  des données du Site dans un but commercial ou en vue d'entraîner une
  intelligence artificielle » est interdit sans licence écrite ; « tout lien
  renvoyant vers le Site, même tacitement autorisé, devra être retiré sur simple
  demande ». Un lien de recherche suivi par l'utilisateur n'extrait rien ; il
  est toléré tant qu'Auchan ne demande pas son retrait.
- **Chronodrive** — CGV (`https://www.chronodrive.com/cgv`, « dernière mise à
  jour le 26/06/2024 » selon la page), lues le 2026-11-20 jusqu'aux
  100 000 premiers caractères. Article 19 : un site personnel non commercial peut
  « placer sur son site un lien simple renvoyant directement à la page du site
  chronodrive.com », sans encapsulation ; tout lien « devra être retiré sur
  simple demande ». Article 3 : l'ajout au panier se fait « en cliquant sur les
  produits ». Que manage_our_home soit un « site personnel non commercial » au
  sens de cette clause n'est pas établi (question 7, § 6).
- **Carrefour** — CGU à `https://www.carrefour.fr/conditions-generales-utilisation`,
  non lues directement (site bloqué). Un extrait de moteur de recherche indique
  que les liens créés sans autorisation doivent être retirés à la demande de
  Carrefour France : source secondaire, à confirmer.
- **E.Leclerc, Intermarché, Système U, Monoprix** — CGU non trouvées par une
  page autorisée et non bloquée : **non lues**. À lire avant d'activer un lien
  vers ces enseignes.

---

## 2 bis. Lidl et Aldi

| Enseigne | Courses alimentaires en ligne ou drive | Fondement | Date |
|---|---|---|---|
| Lidl | **Absence non confirmée formellement**, indices officiels concordants | Page d'accueil `www.lidl.fr` : « Retrouvez vos produits alimentaires dans votre supermarché » ; la page Click & Collect (`/c/click-collect-lidl/s10075746`) ne liste que des catégories non alimentaires ; les CGV (version du 08/06/2026, article 2) ne disent ni n'excluent rien sur l'alimentaire. Presse 2023 (Journal du Geek, Presse-Citron) : boutique en ligne non alimentaire seulement | 2026-11-20 |
| Aldi | **Non confirmé** | Page d'accueil `www.aldi.fr` et sa foire aux questions : aucune mention d'achat en ligne, de drive ni de livraison ; aucune déclaration explicite d'absence. Source secondaire 2026 (meslistesdecourses.fr) : ni drive ni livraison en France | 2026-11-20 |

Aucune des deux enseignes n'a de recherche drive vers laquelle exporter ; elles
restent hors du réglage d'enseigne de la v1.

---

## 3. Quel texte envoyer à la recherche

L'issue proposait : le nom canonique de la taxonomie (`recipes::taxonomy`,
#900/#901) quand un tag existe, sinon le nom saisi. Le nom canonique est le
**premier nom français de la catégorie Open Food Facts**
(`Taxonomy::label`, #819), c'est-à-dire un nom de catégorie, souvent au pluriel.

Relevé sur 10 ingrédients courants, tags et noms canoniques lus dans
`apps/api/data/off-categories-fr.tsv.gz` et `ingredient-aliases-fr.tsv` en
reproduisant les règles de `Taxonomy::canonical_ingredient` (normalisation,
alias, ancêtre commun) ; ce relevé n'a pas été fait par la fonction Rust
elle-même :

| Nom saisi | Tag | Nom canonique | Écart avec le nom saisi |
|---|---|---|---|
| crème fraîche | `en:fermented-creams` (alias #901) | Crèmes épaisses | autre terme |
| parmesan | `en:parmigiano-reggiano` | Parmigiano Reggiano | autre terme (appellation) |
| lait | `en:milks` | Laits | pluriel |
| beurre | `en:butters` | Beurres | pluriel |
| œufs | `en:eggs` | Œufs | casse |
| farine | `en:flours` | Farines | pluriel |
| pâtes | `en:pastas` | pâtes alimentaires | autre terme |
| riz | `en:rices` | Riz | casse |
| tomates | `en:tomatoes` | Tomates | casse |
| Nutella (marque) | aucun | — | — |

Sur ces 10 noms, 9 ont un tag ; pour 3 d'entre eux le nom canonique est un
autre terme que celui saisi, pour 3 un pluriel, pour 3 seule la casse change.
La qualité des résultats par enseigne n'a pas pu être mesurée (§ 2.1).

**Recommandation : envoyer le nom tel que saisi** (espaces de tête et de fin
retirés, sans quantité ni unité, qui sont des champs à part de la ligne), et ne
pas substituer le nom canonique :

- le nom canonique est fait pour classer, pas pour chercher : « Crèmes
  épaisses » ou « Parmigiano Reggiano » ne sont pas ce qu'une personne tape, et
  le second écarte les fromages vendus sous le nom de parmesan ;
- le nom saisi est déjà une requête formulée par l'utilisateur ; une marque n'a
  le plus souvent pas de tag (un nom suivi d'une marque, « Lait demi-écrémé
  Lactel », n'en a jamais), et quand une marque seule en a un, le nom canonique
  efface la marque (« Kinder Maxi » devient « Barres chocolatées au lait ») ;
- une seule règle, sans dépendance à la taxonomie pour le lien.

Le tag reste utile ailleurs : regrouper les lignes, et plus tard chercher par
code-barres (§ 5, issue 4). Cette recommandation est à confirmer par l'essai
manuel du § 6 (question 3) ; si l'essai montre qu'un nom canonique donne de
meilleurs résultats chez une enseigne, la règle se change dans la seule
fonction qui génère le lien.

---

## 4. Recommandation pour la v1

**Une liste copiable et partageable, plus un lien « Rechercher chez
<enseigne> » par ligne pour les enseignes dont le format d'URL est établi ;
l'enseigne choisie dans les réglages de la famille, sans magasin.**

- **Export texte** : la liste non cochée en texte brut, une ligne par article
  (nom, quantité et unité), à copier dans le presse-papiers ou à envoyer par la
  feuille de partage du téléphone. Elle sert pour toutes les enseignes, y
  compris celles dont rien n'est établi, et pour la liste papier.
- **Lien par ligne** : affiché seulement si la famille a choisi une enseigne
  dont l'URL de recherche est établie — aujourd'hui Auchan ; Chronodrive et les
  autres quand l'essai manuel du § 6 aura établi leur format. Le lien s'ouvre
  dans un nouvel onglet avec `rel="noopener noreferrer"`, le terme encodé en
  UTF-8 pour URL. Sur téléphone, chez Auchan et peut-être Chronodrive, il ouvre
  l'application de l'enseigne si elle est installée (§ 2.1).
- **Réglage** : l'enseigne seulement. Le magasin n'est pas stocké en v1 : aucune
  enseigne n'a établi de paramètre de magasin dans l'URL de recherche, et
  l'enseigne retient déjà le magasin choisi par l'utilisateur sur son propre
  site.

**Limites annoncées à l'utilisateur**, à côté des liens :

- le lien ouvre une recherche, il ne remplit pas le panier : il faut choisir le
  produit et l'ajouter soi-même ;
- les résultats et les prix sont ceux du magasin choisi sur le site de
  l'enseigne, qui peut demander d'en choisir un ;
- en cliquant, on quitte manage_our_home : l'enseigne reçoit le nom recherché et
  applique ses propres cookies et sa politique de confidentialité ;
- un lien peut cesser de marcher si l'enseigne change son site.

---

## 5. Issues d'implémentation à créer

Non créées ; à découper par `/plan-issues` après les réponses du § 6.

1. **Réglage d'enseigne de la famille** — un choix parmi les enseignes de la
   v1, modifiable par qui peut modifier les réglages de la famille ; aucun
   magasin.
2. **Génération des liens « Rechercher chez <enseigne> »** — fonction pure
   (enseigne, nom saisi) → URL, testée d'abord (TDD) : encodage, nom vide, nom
   avec quantité ; un gabarit par enseigne établie, aucun pour les autres ; le
   lien sur chaque ligne non cochée de `/grocery-list`.
3. **Export texte et partage de la liste** — texte brut de la liste non cochée,
   copie dans le presse-papiers, feuille de partage quand le navigateur la
   propose ; indépendant de l'enseigne.
4. **Appariement code-barres (éventuel)** — pour une ligne dont le tag
   correspond à un article en stock scanné (`off_products`), chercher par code
   EAN plutôt que par nom ; conditionné à un essai manuel montrant qu'une
   enseigne accepte un EAN dans sa recherche (question 5).
5. **Déclaration RGPD** — registre des traitements (ligne 5, liste de courses)
   et politique de confidentialité : l'enseigne choisie par la famille, et le
   fait qu'en cliquant un lien le nom de l'article part chez l'enseigne. Si un
   magasin préféré est stocké plus tard, il indique un lieu de vie : à déclarer
   avec sa durée de conservation.

---

## 6. Points non établis — questions à l'utilisateur

Un essai manuel, dans un navigateur ordinaire, sans être connecté, est ce qui
peut les trancher ; un outil automatique ne le peut pas sans enfreindre les
`robots.txt` ou contourner une protection.

1. **Format de l'URL de recherche** de Carrefour, Leclerc Drive, Intermarché,
   Système U, Monoprix et Chronodrive : chercher « crème fraîche » et copier
   l'adresse de la page de résultats. L'adresse se rouvre-t-elle dans un autre
   onglet, sans être connecté ?
2. **Magasin requis** : à l'ouverture de cette adresse sans magasin choisi,
   l'enseigne affiche-t-elle des résultats, ou impose-t-elle d'abord un choix de
   magasin ou de drive ? Le magasin apparaît-il dans l'adresse ?
3. **Qualité des résultats** : pour les 10 ingrédients du § 3, chez chaque
   enseigne, le premier résultat est-il le bon produit (oui / proche / non) ?
   Refaire l'essai avec le nom canonique pour crème fraîche, parmesan et pâtes.
4. **Lien profond** : sur téléphone, avec l'application de l'enseigne
   installée, l'adresse du point 1 ouvre-t-elle l'application sur la recherche ?
5. **Code-barres** : la recherche d'une enseigne accepte-t-elle un code EAN à
   13 chiffres et trouve-t-elle le produit ?
6. **Import de liste et ajout au panier par URL** : une enseigne propose-t-elle,
   sans compte, de coller une liste ou d'ouvrir un panier prérempli depuis un
   lien ? Rien ne l'établit à ce jour.
7. **CGU** : manage_our_home est-il un « site personnel non commercial » au sens
   de l'article 19 des CGV de Chronodrive ? Faut-il lire les CGU de Leclerc,
   Intermarché, Système U et Monoprix (non lues, § 2.2) avant d'activer un lien
   vers elles, ou demander une autorisation à Carrefour ?
8. **Lidl et Aldi** : une confirmation par leur service client suffirait-elle,
   ou les laisse-t-on hors du réglage sur la seule base du § 2 bis ?

---

## Sources

Observations directes du 2026-11-20 : `robots.txt` demandé aux dix sites cités (celui de `www.coursesu.com` a répondu 403) ;
pages d'accueil de `www.carrefour.fr`, `www.e.leclerc`, `www.leclercdrive.fr`,
`www.intermarche.com`, `www.auchan.fr`, `www.monoprix.fr`,
`www.chronodrive.com`, `www.lidl.fr`, `www.aldi.fr` ;
`/.well-known/apple-app-site-association` et `/.well-known/assetlinks.json` de
`www.auchan.fr`, `www.chronodrive.com`, `www.e.leclerc` ; CGU Auchan
(`/cgu/ep-cgu`), CGV Chronodrive (`/cgv`), CGV Lidl
(`/c/conditions-generales-de-vente/s10016727`), page Click & Collect Lidl, FAQ
Aldi (`/foire-aux-questions.html`).

Sources secondaires : Clubic, « Pourquoi utiliser LeclercDrive &
LeclercChezMoi ? » ; Journal du Net, « Chronodrive lance sa télécommande de
courses Izy » ; Journal du Geek (2023-05-19) et Presse-Citron sur le lancement
de la boutique en ligne Lidl ; meslistesdecourses.fr, « Aldi courses en
ligne : prix et disponibilité 2026 » ; extrait de moteur de recherche des CGU
de carrefour.fr. Relevé préliminaire du 2026-11-17 en commentaire de l'issue
#912 (sources secondaires, non reprises ici).
