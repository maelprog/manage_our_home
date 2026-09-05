//! Dates de test exprimées dans le fuseau de l'application, pas dans celui du
//! runner.
//!
//! L'app affiche tout en **Europe/Paris** — le fuseau v1 figé, cf.
//! `apps/web/src/routes/agenda/mod.rs` (`DISPLAY_TZ`). Les helpers de
//! date des specs lisaient `new Date()` dans l'horloge locale du processus
//! Node, qui vaut UTC sur un runner GitHub. Entre 22 h et minuit UTC les deux
//! ne désignent plus le même jour, et un événement construit sur « aujourd'hui
//! » selon le runner était déjà terminé selon l'app : `home.spec.ts` échouait
//! toutes les nuits sur cette fenêtre, à l'heure près.
//!
//! Rien ici ne dépend d'un `TZ=` posé à l'extérieur : le fuseau est nommé, donc
//! le résultat est le même sur le poste d'un développeur et en CI.

/** Composantes de la date courante à Paris, sûres pour un calcul calendaire. */
function parisParts(now: Date): { y: number; m: number; d: number } {
	// `en-CA` rend `YYYY-MM-DD`, le seul format ISO qu'Intl produise nativement.
	const [y, m, d] = new Intl.DateTimeFormat("en-CA", {
		timeZone: "Europe/Paris",
		year: "numeric",
		month: "2-digit",
		day: "2-digit",
	})
		.format(now)
		.split("-")
		.map(Number);
	return { y, m, d };
}

/**
 * `YYYY-MM-DD` du jour à Paris, décalé de `offset` jours.
 *
 * Le décalage porte sur les composantes de date et non sur l'instant : ajouter
 * 24 h traverserait mal les changements d'heure (le 25 octobre dure 25 h à
 * Paris, et `+24 h` depuis 00 h 30 retomberait dans la même journée).
 * `Date.UTC` normalise seul les débordements de mois et d'année.
 */
export function parisDay(offset = 0, now: Date = new Date()): string {
	const { y, m, d } = parisParts(now);
	return new Date(Date.UTC(y, m - 1, d + offset)).toISOString().slice(0, 10);
}

/** `YYYY-MM-DD` pour un quantième du mois courant **à Paris**. */
export function parisDayOfMonth(day: number, now: Date = new Date()): string {
	const { y, m } = parisParts(now);
	return `${y}-${String(m).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
}
