# TODOS

## MFA — piste v2, à re-chiffrer le moment venu

**What:** authentification à deux facteurs. Piste privilégiée : TOTP (RFC 6238) +
codes de secours, opt-in pour les membres et obligatoire pour le superadmin —
éventuellement en commençant par le superadmin seul. Écarté à ce stade : l'OTP
par e-mail (la boîte mail est déjà le facteur de reset du mot de passe, donc pas
un second facteur), l'OTP par SMS (nouveau sous-traitant + numéro de téléphone à
déclarer au registre) et WebAuthn/passkeys (`navigator.credentials` impose du JS
client au cœur de la connexion, alors que le front SSR ne charge de script qu'en
complément du markup — `apps/web/src/assets.rs`, `Script`).

**Why:** pas un blocant v1 (déploiement familial restreint), mais le superadmin
est un compte à privilèges globaux — dès que l'app est exposée sur Internet,
un mot de passe seul est sa seule barrière.

**Context (état constaté le 2026-09-16, à revérifier) :** réutilisable en l'état
— sessions serveur en base (`sessions`, extracteur `AuthUser`,
`apps/api/src/auth/session.rs`), donc l'état « à demi authentifié » se pose sur
une ligne ; chiffrement au repos déjà outillé pour le secret TOTP
(`pgp_sym_encrypt`/`pgp_sym_decrypt` de pgcrypto, une clé dédiée par usage dans
`AppState`, `apps/api/src/lib.rs` ; modèle : les refresh tokens de
`apps/api/src/auth/oauth_google.rs`) ; `audit_log` (migration 0001) pour
tracer activation/désactivation ; front SSR en `<form method=post>`, donc un
écran de challenge est une route de plus, pas du JS. Les deux pièces qui portent
le coût : `login_inner` (`apps/api/src/auth/mod.rs`) crée directement une session
pleine et `AuthUser` fait confiance à toute ligne non révoquée — l'état
`pending_mfa` touche l'extracteur utilisé par toutes les routes ; et la
récupération en cas de perte du téléphone, qui est le coût récurrent réel sur une
app sans support. À re-spécifier au passage : `reset_password` (ne doit pas
devenir un bypass), comptes Google-only, `delete_account`. Le secret TOTP et les
codes de secours sont une nouvelle donnée à déclarer dans
`registre-traitements.md` et `privacy-policy.md`.

Ordre de grandeur estimé à cette date : ~10 j pour la version complète, ~4 j pour
la variante superadmin-seul sans codes de secours (déverrouillage par script
ops). **Ce chiffrage vaut pour l'état du projet au 2026-09-16 et n'est pas à
reprendre tel quel : refaire le relevé de l'auth avant de planifier.**

**Depends on / blocked by:** un plafond d'essais sur l'écran de challenge — un
code à 6 chiffres sans plafond, ce sont 10⁶ tentatives. `/login` en a un depuis
#178 (verrou par couple adresse/e-mail, `apps/api/src/auth/throttle.rs`), qui ne
couvrira pas la route de challenge d'office ; le rate-limiting de
`v2-deployment.md` (item 14, `/login` et `/register`) reste le cadre où le poser.
