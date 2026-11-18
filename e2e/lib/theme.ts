import { expect, Page } from "@playwright/test";

// The theme a visitor picks on /account/appearance (#585). The page must be
// logged in: the appearance page is an account page.

export type ThemeChoice = "Automatique" | "Clair" | "Sombre";

/** Whether `page` is posting the appearance form. */
export function appearancePosted(page: Page) {
  return page.waitForRequest(
    (request) => request.method() === "POST" && new URL(request.url()).pathname === "/account/appearance",
  );
}

/**
 * Picks `choice` on the appearance page, which saves it by itself (#844):
 * no button to press. A choice already in force is left as it is — picking
 * it again changes nothing, so nothing is sent.
 */
export async function chooseTheme(page: Page, choice: ThemeChoice): Promise<void> {
  await page.goto("/account/appearance");
  const radio = page.getByRole("radio", { name: choice });
  if (await radio.isChecked()) return;
  const posted = appearancePosted(page);
  await radio.check();
  await posted;
  await expect(page).toHaveURL(/\/account\/appearance\?notice=saved$/);
  await expect(radio).toBeChecked();
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
