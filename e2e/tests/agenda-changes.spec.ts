import { expect, Page, test } from "@playwright/test";
import {
  fetchVerificationToken,
  seedAgendaChangeNotification,
  seedMessageNotification,
} from "../lib/db";

// The changes another member makes to the agenda (#853): their pastille,
// label and rows in the notification centre, and the three switches of
// `/account/notifications` — every setting there a switch, saved as soon
// as it changes, read back after a reload.
//
// The centre's rows are written straight into Postgres (`lib/db.ts`), as
// apps/api writes them when another member of the family changes the
// agenda: the suite would otherwise need a second account in the family.

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

const PASSWORD = "e2e-agenda-change-password-1";

/**
 * Avoids every form label of the pages under test (`getByLabel` matches
 * substrings, the family switcher included): Titre, Début, Fin, Lieu,
 * Description, Rappel, Heure du récapitulatif, and the switches' words.
 */
const FAMILY = "Foyer Girouette";

async function registerAndLogin(page: Page, prefix: string): Promise<string> {
  const email = uniqueEmail(prefix);
  await page.goto("/register");
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Nom affiché").fill("Alix");
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
  return email;
}

const toggle = (page: Page, name: string) => page.getByRole("switch", { name, exact: true });

const SAVED = /\/account\/notifications\?notice=channel_saved$/;

test.describe("Agenda changes — settings", () => {
  test("every setting is a switch, saved as it changes and read back after a reload", async ({
    page,
  }) => {
    await registerAndLogin(page, "agenda-switches");
    await page.goto("/account/notifications");

    // On by default, the three of the agenda; no button with the script.
    for (const name of ["Ajouts", "Modifications", "Suppressions"]) {
      await expect(toggle(page, name)).toBeChecked();
    }
    await expect(page.getByRole("button", { name: "Enregistrer" })).toBeHidden();
    await expect(page.getByText("Chaque réglage s’applique dès que vous le changez.")).toBeVisible();

    // One click saves one switch; the others stay as they were.
    await toggle(page, "Modifications").click();
    await expect(page).toHaveURL(SAVED);
    await expect(page.getByRole("status").filter({ hasText: "Préférence enregistrée." })).toBeVisible();
    await expect(toggle(page, "Modifications")).not.toBeChecked();
    await expect(toggle(page, "Modifications")).toBeFocused();
    await expect(toggle(page, "Ajouts")).toBeChecked();
    await expect(toggle(page, "Suppressions")).toBeChecked();

    // From the keyboard: Space flips the focused switch, which keeps the
    // focus once the page is back.
    await toggle(page, "Suppressions").focus();
    await page.keyboard.press("Space");
    await expect(page).toHaveURL(SAVED);
    await expect(toggle(page, "Suppressions")).not.toBeChecked();
    await expect(toggle(page, "Suppressions")).toBeFocused();

    // Both reminder switches off, and the recap off: allowed (#853).
    await toggle(page, "Par notification sur mes appareils").click();
    await expect(page).toHaveURL(SAVED);
    await expect(toggle(page, "Par notification sur mes appareils")).not.toBeChecked();
    await toggle(page, "Récapitulatif quotidien").click();
    await expect(page).toHaveURL(SAVED);
    // The recap off, its hour is still changed, and kept.
    await page.getByLabel("Heure du récapitulatif").selectOption("18");
    await expect(page).toHaveURL(SAVED);

    // Read back from the server, not from the page left behind.
    await page.goto("/account/notifications");
    await expect(toggle(page, "Ajouts")).toBeChecked();
    await expect(toggle(page, "Modifications")).not.toBeChecked();
    await expect(toggle(page, "Suppressions")).not.toBeChecked();
    await expect(toggle(page, "Par notification sur mes appareils")).not.toBeChecked();
    await expect(toggle(page, "Par email")).not.toBeChecked();
    await expect(toggle(page, "Récapitulatif quotidien")).not.toBeChecked();
    // The recap off, the hour shown is the one chosen while it was off; on
    // again, the recap comes at that hour.
    await expect(page.getByLabel("Heure du récapitulatif")).toHaveValue("18");
    await toggle(page, "Récapitulatif quotidien").click();
    await expect(page).toHaveURL(SAVED);
    await page.goto("/account/notifications");
    await expect(toggle(page, "Récapitulatif quotidien")).toBeChecked();
    await expect(page.getByLabel("Heure du récapitulatif")).toHaveValue("18");
  });

  test("without JavaScript, the switches are sent by « Enregistrer »", async ({ browser }) => {
    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    await registerAndLogin(page, "agenda-switches-nojs");
    await page.goto("/account/notifications");
    await expect(page.getByText("Sans JavaScript, le choix s'envoie avec le bouton.")).toBeVisible();
    await toggle(page, "Ajouts").uncheck();
    await toggle(page, "Par email").check();
    await page.getByRole("button", { name: "Enregistrer" }).click();
    await expect(page).toHaveURL(SAVED);
    await expect(toggle(page, "Ajouts")).not.toBeChecked();
    await expect(toggle(page, "Par email")).toBeChecked();
    await expect(toggle(page, "Modifications")).toBeChecked();
    await context.close();
  });
});

test.describe("Agenda changes — notification centre", () => {
  test("the filter lists the changes; one opens its event, a deletion stays here", async ({
    page,
  }) => {
    const email = await registerAndLogin(page, "agenda-centre");
    await page.goto("/groups/new");
    await page.getByLabel("Nom du groupe").fill(FAMILY);
    await page.getByRole("button", { name: "Créer le groupe" }).click();
    await expect(page).toHaveURL(/\/groups\?notice=group_created$/);

    await page.goto("/agenda/new");
    await page.getByLabel("Titre").fill("Réunion d’école");
    await page.getByLabel("Début").fill("2030-03-14T18:00");
    await page.getByLabel("Fin").fill("2030-03-14T19:00");
    await page.getByRole("button", { name: "Créer l'événement" }).click();
    await expect(page).toHaveURL(/\/agenda\?notice=event_created$/);

    await seedMessageNotification(email, FAMILY, "Sam a écrit dans la messagerie.");
    await seedAgendaChangeNotification(
      email,
      FAMILY,
      "Sam a modifié « Réunion d’école » : lieu.",
      "Réunion d’école",
    );
    await seedAgendaChangeNotification(
      email,
      FAMILY,
      "Sam a supprimé « Piscine » du 20/11/2026.",
      null,
    );

    // The last pastille, lit; the changes alone, under their label.
    await page.goto("/notifications");
    const filters = page.getByRole("navigation", { name: "Filtres" });
    await filters.getByRole("link", { name: "Changements d’agenda", exact: true }).click();
    await expect(page).toHaveURL("/notifications?kind=agenda_change");
    await expect(filters.getByRole("link").last()).toHaveText("Changements d’agenda");
    await expect(filters.getByRole("link", { name: "Changements d’agenda", exact: true })).toHaveAttribute(
      "aria-current",
      "true",
    );
    const changed = page.getByRole("button", { name: /« Réunion d’école » : lieu/ });
    const deleted = page.getByRole("button", { name: /a supprimé « Piscine »/ });
    await expect(changed).toContainText("Changement d’agenda ·");
    await expect(deleted).toContainText("Non lue");
    await expect(page.getByText("Sam a écrit dans la messagerie.")).toHaveCount(0);

    // A deletion has nowhere to go: read, and the page stays.
    await deleted.click();
    await expect(page).toHaveURL("/notifications");
    await expect(page.getByRole("button", { name: /a supprimé « Piscine »/ })).not.toContainText("Non lue");

    // A change opens its event.
    await page.goto("/notifications?kind=agenda_change");
    await page.getByRole("button", { name: /« Réunion d’école » : lieu/ }).click();
    await expect(page).toHaveURL(/\/agenda\/[0-9a-f-]{36}$/);
    await expect(page.getByRole("heading", { name: "Réunion d’école" })).toBeVisible();
  });
});
