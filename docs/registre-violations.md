# Registre des violations de données — gabarit

Le RGPD impose de consigner **toute** violation de données personnelles, y
compris celles qui ne sont notifiées ni à la CNIL ni aux personnes : les
faits, leurs effets et les mesures prises (art. 33(5)). La marche à suivre
est dans `docs/procedure-violation.md`.

**Ce fichier ne contient et ne contiendra aucune entrée.** Le dépôt est
public, et une entrée décrit une faille, les comptes qu'elle touche et ce
qui en a fui : la publier aggraverait la violation qu'elle consigne. Le
registre réel est tenu hors du dépôt par le responsable de traitement, à
[emplacement du registre des violations — à renseigner avant la mise en
ligne], et il est ouvert le jour de la mise en ligne, même vide. Ce fichier
en fixe la forme : une entrée par incident, avec les rubriques ci-dessous,
remplies au fil de la procédure et jamais effacées. Une rubrique sans
réponse s'écrit « inconnu à ce jour », pas en blanc.

## Gabarit d'une entrée

```
### V-<AAAA>-<NN> — <intitulé court>

Prise de connaissance : <date et heure, fuseau> — fait partir les 72 h
Signalé par         : <responsable / membre / sous-traitant / tiers>
Date de l'incident  : <date et heure, ou fourchette, ou « inconnue »>

Faits
- Ce qui s'est passé, et la cause si elle est connue
- Nature : confidentialité / intégrité / disponibilité
- Données en cause : tables, objets du stockage, colonnes
- Chiffrées ? clé restée hors d'atteinte ?
- Personnes concernées : catégories et nombre (approximatif s'il le faut)
- Enregistrements concernés : nombre (approximatif s'il le faut)

Effets
- Conséquences probables pour les personnes
- Niveau de risque retenu : aucun / faible / élevé, et pourquoi

Mesures
- Confinement (sessions révoquées, secrets renouvelés, service arrêté…)
- Mesures pour en limiter les effets
- Mesures pour éviter qu'elle se reproduise

Notification CNIL (art. 33)
- Faite : oui, le <date et heure>, n° <référence> / non, motif : <…>
- Compléments : <dates>
- Retard sur les 72 h : <motif>, ou sans objet

Information des personnes (art. 34 et seuil du 2026-10-02)
- Faite : oui, le <date>, à <nombre> comptes / non, motif : <…>
- Comptes non joignables (purgés) : <nombre>
- Personnes sans compte (adresses d'invitation) : <traitement retenu>

Clôture : <date>
```
