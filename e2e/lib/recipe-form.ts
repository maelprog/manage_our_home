import { Page } from "@playwright/test";

// The recipe form's ingredient lines and steps (#539), filled from the
// short notation the suites already wrote their recipes in: one ingredient
// per line, `nom | quantité | unité | mois de saison | optionnel`, trailing
// fields optional. Each field of line n is named « Quantité (ligne n) »,
// « Ingrédient (ligne n) »… — the number is said to a screen reader only.

/** The field `label` of ingredient line `n` (1-based). */
export function lineField(page: Page, label: string, n: number) {
  return page.getByRole("textbox", { name: `${label} (ligne ${n})`, exact: true });
}

/** Fills the ingredient lines from the top, one per line of `ingredients`. */
export async function fillIngredients(page: Page, ingredients: string): Promise<void> {
  const lines = ingredients.split("\n").filter((l) => l.trim() !== "");
  for (const [i, text] of lines.entries()) {
    const n = i + 1;
    const [name, quantity, unit, months, optional] = text.split("|").map((f) => f.trim());
    await lineField(page, "Ingrédient", n).fill(name ?? "");
    if (quantity) await lineField(page, "Quantité", n).fill(quantity);
    if (unit) await lineField(page, "Unité", n).fill(unit);
    if (months) await lineField(page, "Saison", n).fill(months);
    if (optional) {
      await page.getByRole("checkbox", { name: `Facultatif (ligne ${n})`, exact: true }).check();
    }
  }
}

/** Fills the steps from the first, one per line of `instructions`. */
export async function fillSteps(page: Page, instructions: string): Promise<void> {
  const steps = instructions.split("\n").filter((l) => l.trim() !== "");
  for (const [i, step] of steps.entries()) {
    await page.getByRole("textbox", { name: `Étape ${i + 1}`, exact: true }).fill(step);
  }
}
