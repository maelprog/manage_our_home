import { expect, Page } from "@playwright/test";

// The theme a visitor picks on /account/appearance (#585). The page must be
// logged in: the appearance page is an account page.

export type ThemeChoice = "Automatique" | "Clair" | "Sombre";

/** Picks `choice` on the appearance page and saves it. */
export async function chooseTheme(page: Page, choice: ThemeChoice): Promise<void> {
  await page.goto("/account/appearance");
  await page.getByRole("radio", { name: choice }).check();
  await page.getByRole("button", { name: "Enregistrer", exact: true }).click();
  await expect(page).toHaveURL(/\/account\/appearance\?notice=saved$/);
  await expect(page.getByRole("radio", { name: choice })).toBeChecked();
}

/** What the page in view resolved: `data-theme`, `--bg` and `color-scheme` of `<html>`. */
export async function resolvedTheme(
  page: Page,
): Promise<{ dataTheme: string | null; bg: string; colorScheme: string }> {
  return page.evaluate(() => {
    const root = document.documentElement;
    const style = getComputedStyle(root);
    return {
      dataTheme: root.getAttribute("data-theme"),
      bg: style.getPropertyValue("--bg").trim(),
      colorScheme: style.colorScheme,
    };
  });
}
