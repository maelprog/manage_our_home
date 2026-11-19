import { Browser, expect, Page, test } from "@playwright/test";
import { fetchVerificationToken } from "../lib/db";

// Issue #867 — « Accès » in the family's settings: the owner alone sets from
// which role the members reach each restricted feature. The Rust tests check
// the markup and the body sent; only a browser says that a radio sends the
// form by itself, that the form waits while it is sent, that the page comes
// back at the card, that the button does the job without JavaScript, and
// that the threshold reaches another member's menu.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-access-password-1";

async function registerAndLogin(page: Page, prefix: string, displayName: string): Promise<void> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill(displayName);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("textbox", { name: "Confirmation" }).fill(PASSWORD);
  await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
  await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
  await page.getByRole("button", { name: "Créer mon compte" }).click();
  await expect(page).toHaveURL(/\/register\/check-email$/);
  const token = await fetchVerificationToken(email);
  await page.goto(`/verify-email?token=${token}`);
  await page.goto("/login");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("textbox", { name: "Mot de passe" }).fill(PASSWORD);
  await page.getByRole("button", { name: "Se connecter" }).click();
  await expect(page).toHaveURL("/");
}

async function createGroup(page: Page, name: string): Promise<void> {
  await page.goto("/groups/new");
  await page.getByLabel("Nom du groupe").fill(name);
  await page.getByRole("button", { name: "Créer le groupe" }).click();
  await expect(page).toHaveURL(/\/groups\?notice=group_created$/);
}

async function openSettings(page: Page, family: string): Promise<void> {
  await page.goto("/groups");
  await page.locator("li", { hasText: family }).getByRole("link", { name: "Paramètres" }).click();
  await expect(page.getByRole("heading", { level: 1, name: family })).toBeVisible();
}

async function secondUser(browser: Browser, prefix: string, displayName: string): Promise<Page> {
  const context = await browser.newContext();
  const page = await context.newPage();
  await registerAndLogin(page, prefix, displayName);
  return page;
}

test.describe("The family's access settings (#867)", () => {
  test("the owner lowers the budget's threshold: a standard member gets the Budget link; an admin sees no card", async ({
    page,
    browser,
  }) => {
    const family = `Foyer Seuils ${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
    await registerAndLogin(page, "e2e-access-owner", "Seuils Owner");
    await createGroup(page, family);
    await page.goto("/groups");
    await page.locator("li", { hasText: family }).getByRole("link", { name: "Membres" }).click();
    await page.getByRole("button", { name: "Créer une invitation" }).click();
    const invite = await page.locator(".notice.success a").getAttribute("href");
    if (!invite) throw new Error("no invitation link rendered");

    const member = await secondUser(browser, "e2e-access-member", "Seuils Member");
    await member.goto(invite);
    await member.getByRole("button", { name: "Rejoindre le groupe" }).click();

    // A standard member: no card, and no Budget link at the default
    // threshold.
    await openSettings(member, family);
    await expect(member.locator("#acces")).toHaveCount(0);
    await member.goto("/");
    await expect(member.locator('a[href="/budget"]')).toHaveCount(0);

    // The owner: seven features, `admin` checked by default; the choice
    // applies itself, the button being the fallback without JavaScript.
    await openSettings(page, family);
    const card = page.locator("#acces");
    await expect(card.getByRole("heading", { name: "Accès" })).toBeVisible();
    await expect(card.getByRole("group")).toHaveCount(7);
    await expect(card.getByRole("radio")).toHaveCount(21);
    const budget = card.getByRole("group", { name: "Budget" });
    await expect(budget.getByRole("radio", { name: "Admins et propriétaire" })).toBeChecked();
    await expect(card.getByRole("button", { name: "Enregistrer l'accès" })).toBeHidden();
    await expect(card.getByText("Le choix s'applique dès que vous le touchez.")).toBeVisible();

    // While it is sent, the form waits: every radio locked, the budget's
    // fieldset saying so. The page is gone before a locator could read
    // it, so a listener of the page's own `change`, run after enhance.js's,
    // notes what it shows in sessionStorage, which outlives the page.
    await page.evaluate(() => {
      document.addEventListener("change", () => {
        const card = document.getElementById("acces")!;
        const radios = Array.from(card.querySelectorAll<HTMLInputElement>('input[type="radio"]'));
        const budget = card.querySelector("fieldset")!;
        sessionStorage.setItem(
          "e2e-access-waiting",
          JSON.stringify({
            form: card.querySelector("form")!.getAttribute("aria-busy"),
            fieldset: budget.getAttribute("aria-busy"),
            status: budget.querySelector('[role="status"]')!.textContent,
            disabled: radios.filter((r) => r.disabled).length,
          }),
        );
      });
    });
    await budget.getByRole("radio", { name: "Tous les membres" }).check();
    // Back where it was sent from (`data-keep-scroll`), the focus on the
    // radio: no anchor, which would take the focus away.
    await expect(page).toHaveURL(/\/settings\?notice=access_saved$/);
    const waiting = await page.evaluate(() => JSON.parse(sessionStorage.getItem("e2e-access-waiting") ?? "null"));
    expect(waiting).toEqual({ form: "true", fieldset: "true", status: "Enregistrement…", disabled: 21 });
    await expect(
      card.getByRole("status").filter({
        hasText: "Accès enregistré. Les membres voient le changement à leur prochaine page.",
      }),
    ).toBeVisible();
    await expect(budget.getByRole("radio", { name: "Tous les membres" })).toBeChecked();
    await expect(budget.getByRole("radio", { name: "Tous les membres" })).toBeFocused();

    // The standard member now reaches the budget.
    await member.goto("/");
    await expect(member.locator('a[href="/budget"]').first()).toBeAttached();

    // Made admin, they still see no card.
    await page.goto("/groups");
    await page.locator("li", { hasText: family }).getByRole("link", { name: "Membres" }).click();
    const row = page.locator("li", { hasText: "Seuils Member" });
    await row.locator("select[name=role]").selectOption("admin");
    await row.getByRole("button", { name: "Changer le rôle" }).click();
    await expect(page.getByText("Rôle mis à jour.")).toBeVisible();
    await openSettings(member, family);
    await expect(member.locator("#acces")).toHaveCount(0);
    await expect(member.getByRole("heading", { name: "Couleur de la famille" })).toBeVisible();

    await member.context().close();
  });

  test("without JavaScript, « Enregistrer l'accès » sends the choice", async ({ page, browser }) => {
    const family = `Foyer Sans Script ${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
    await registerAndLogin(page, "e2e-access-nojs", "Sans Script Owner");
    await createGroup(page, family);

    const context = await browser.newContext({
      javaScriptEnabled: false,
      storageState: await page.context().storageState(),
    });
    const nojs = await context.newPage();
    await openSettings(nojs, family);
    const card = nojs.locator("#acces");
    const button = card.getByRole("button", { name: "Enregistrer l'accès" });
    await expect(button).toBeVisible();
    await expect(card.getByText("Sans JavaScript, le choix s'envoie avec le bouton.")).toBeVisible();
    const messagerie = card.getByRole("group", { name: "Messagerie" });
    await messagerie.getByRole("radio", { name: "Propriétaire seul" }).check();
    // Nothing sent until the button.
    await expect(nojs).toHaveURL(/\/settings$/);
    await button.click();
    await expect(nojs).toHaveURL(/\/settings\?notice=access_saved#acces$/);
    await expect(messagerie.getByRole("radio", { name: "Propriétaire seul" })).toBeChecked();
    await expect(
      card.getByRole("group", { name: "Budget" }).getByRole("radio", { name: "Admins et propriétaire" }),
    ).toBeChecked();

    await context.close();
  });
});
