import { expect, Page, test } from "@playwright/test";
import {
  ageVerificationTokens,
  clearAgeDeclaration,
  clearTermsAcceptance,
  countVerificationTokens,
  fetchPasswordResetToken,
  fetchVerificationToken,
  setTermsAcceptedVersion,
} from "../lib/db";

function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.test`;
}

test.describe("Auth — register → verify → login → logout", () => {
  test("full journey", async ({ page }) => {
    const email = uniqueEmail("e2e-register");
    const password = "e2e-password-1";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("E2E User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();

    await expect(page).toHaveURL(/\/register\/check-email$/);
    await expect(page.getByText("Un email de confirmation vous a été envoyé")).toBeVisible();

    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);
    await expect(page.getByText("Email vérifié")).toBeVisible();

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();

    await expect(page).toHaveURL("/");
    await expect(page.getByText("Bienvenue")).toBeVisible();
    await expect(page.getByText("E2E User")).toBeVisible();

    await page.getByRole("button", { name: "Se déconnecter" }).click();
    await expect(page).toHaveURL(/\/login$/);
  });

  test("wrong password shows a single generic error", async ({ page }) => {
    const email = uniqueEmail("e2e-badlogin");
    const password = "e2e-password-2";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Bad Login User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("totally-wrong");
    await page.getByRole("button", { name: "Se connecter" }).click();

    await expect(page.getByText("Email ou mot de passe incorrect.")).toBeVisible();
  });

  test("duplicate email registration shows an inline field error", async ({ page }) => {
    const email = uniqueEmail("e2e-dup");
    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Dup User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("password-one");
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(page).toHaveURL(/\/register\/check-email$/);

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Dup User 2");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("password-two");
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();

    await expect(page.getByText("Un compte existe déjà avec cet email.")).toBeVisible();
  });

  // #137: art. 8 GDPR — the service is not open under 15, and the form says so
  // instead of creating the account and sorting it out later.
  test("registering without the age declaration is refused", async ({ page }) => {
    const email = uniqueEmail("e2e-no-age");
    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("No Age User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("e2e-password-3");
    await page.getByRole("button", { name: "Créer mon compte" }).click();

    await expect(page).toHaveURL(/\/register$/);
    await expect(
      page.getByText("Le service n'est pas ouvert aux moins de 15 ans"),
    ).toBeVisible();

    // And the form comes back usable: the email and the display name are still
    // there, the password never is (it is not echoed back into the page), and
    // ticking the box registers the account.
    await expect(page.getByLabel("Email")).toHaveValue(email);
    await expect(page.getByLabel("Nom affiché")).toHaveValue("No Age User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("e2e-password-3");
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(page).toHaveURL(/\/register\/check-email$/);
  });

  // #318: an account with no age declaration on file — opened through Google,
  // or before #137 — is held at the declaration page until it declares, and
  // every page of the app sends it back there.
  test("an account without age declaration is held at the declaration", async ({ page }) => {
    const email = uniqueEmail("e2e-age-later");
    const password = "e2e-password-4";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Age Later User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);
    await clearAgeDeclaration(email);

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/account/age");
    await expect(page.getByRole("heading", { name: "Déclaration d'âge" })).toBeVisible();

    for (const path of ["/", "/agenda", "/account", "/login"]) {
      await page.goto(path);
      await expect(page).toHaveURL("/account/age");
    }

    // Declaring nothing is refused with the registration's message.
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(page).toHaveURL("/account/age?error=age_declaration_required");
    await expect(
      page.getByText(
        "Le service n'est pas ouvert aux moins de 15 ans : cochez la case pour déclarer votre âge.",
      ),
    ).toBeVisible();

    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(page).toHaveURL("/");
    await expect(page.getByText("Bienvenue")).toBeVisible();

    // Done once: the page now sends the account on to the app.
    await page.goto("/account/age");
    await expect(page).toHaveURL("/");
  });

  // #318: no parental-consent path — an account that does not declare can
  // only leave.
  test("an account without age declaration can log out", async ({ page }) => {
    const email = uniqueEmail("e2e-age-leave");
    const password = "e2e-password-5";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Age Leave User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);
    await clearAgeDeclaration(email);

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/account/age");

    await page.getByRole("button", { name: "Se déconnecter" }).click();
    await expect(page).toHaveURL(/\/login$/);
    await page.goto("/account/age");
    await expect(page).toHaveURL(/\/login$/);
  });

  // #319: registering without accepting the CGU is refused, and the form
  // comes back with the boxes as they were left.
  test("registering without accepting the CGU is refused", async ({ page }) => {
    const email = uniqueEmail("e2e-no-terms");
    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("No Terms User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("e2e-password-6");
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();

    await expect(page).toHaveURL(/\/register$/);
    await expect(
      page.getByText("Cochez la case pour accepter les conditions générales d'utilisation."),
    ).toBeVisible();
    await expect(
      page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }),
    ).toBeChecked();

    await page.getByRole("textbox", { name: "Mot de passe" }).fill("e2e-password-6");
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(page).toHaveURL(/\/register\/check-email$/);
  });

  // #319: an account with no acceptance of the CGU on file — opened through
  // Google, or before #319 — is held at the acceptance page until it accepts.
  test("an account without acceptance of the CGU is held at the acceptance", async ({ page }) => {
    const email = uniqueEmail("e2e-terms-later");
    const password = "e2e-password-7";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Terms Later User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);
    await clearTermsAcceptance(email);

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/account/terms");
    await expect(
      page.getByRole("heading", { name: "Conditions d'utilisation" }),
    ).toBeVisible();

    for (const path of ["/", "/agenda", "/account", "/login", "/account/age"]) {
      await page.goto(path);
      await expect(page).toHaveURL("/account/terms");
    }

    // Accepting nothing is refused.
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(page).toHaveURL("/account/terms?error=terms_acceptance_required");
    await expect(page.getByText("cochez la case pour les accepter.")).toBeVisible();

    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Continuer" }).click();
    await expect(page).toHaveURL("/");
    await expect(page.getByText("Bienvenue")).toBeVisible();

    // Done: the page now sends the account on to the app, and no notice of a
    // new version shows — the version in force is the one accepted.
    await expect(page.getByText("Les conditions d'utilisation ont changé")).toHaveCount(0);
    await page.goto("/account/terms");
    await expect(page).toHaveURL("/");
  });

  // #319: a member who accepted an earlier version is not held — continued
  // use is acceptance, per the CGU — but told on the home page and on
  // /account, until they acknowledge it.
  test("a member on an earlier version of the CGU is told until they acknowledge it", async ({ page }) => {
    const email = uniqueEmail("e2e-terms-update");
    const password = "e2e-password-8";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Terms Update User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);
    await setTermsAcceptedVersion(email, "2000-01-01");

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/");
    const notice = page.getByRole("heading", { name: "Les conditions d'utilisation ont changé" });
    await expect(notice).toBeVisible();
    await expect(page.getByRole("link", { name: "Lire la nouvelle version" })).toHaveAttribute(
      "href",
      "/terms-of-service",
    );

    await page.goto("/account");
    await expect(notice).toBeVisible();

    await page.getByRole("button", { name: "J'en ai pris connaissance" }).click();
    await expect(page).toHaveURL("/");
    await expect(notice).toHaveCount(0);
    await page.goto("/account");
    await expect(notice).toHaveCount(0);
  });
});

test.describe("Auth — renvoi de l'email de vérification (#420)", () => {
  const RESEND = "Renvoyer l'email de vérification";
  const SENT =
    "Si un compte en attente de vérification existe pour cette adresse, " +
    "un nouvel email de vérification vient de lui être envoyé.";

  async function register(page: Page, email: string): Promise<void> {
    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Resend User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill("e2e-resend-password-1");
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    await expect(page).toHaveURL(/\/register\/check-email$/);
  }

  test("the button counts the cooldown down, then comes back", async ({ page }) => {
    // The browser's clock, driven: no 30-second wait.
    await page.clock.install();
    await page.goto("/register/check-email");
    const button = page.locator("button[data-resend-cooldown]");
    // The length comes from the page, which has it from the shared constant.
    const cooldown = Number(await button.getAttribute("data-resend-cooldown"));
    expect(cooldown).toBeGreaterThan(1);

    await expect(button).toBeDisabled();
    // The installed clock still flows: read the count, don't predict it.
    const left = async () =>
      Number((/^Renvoyer l'email \((\d+) s\)$/.exec((await button.textContent()) ?? "") ?? [])[1]);
    const before = await left();
    expect(before).toBeGreaterThan(0);
    expect(before).toBeLessThanOrEqual(cooldown);
    await page.clock.runFor(2000);
    await expect.poll(left).toBeLessThan(before);
    await expect(button).toBeDisabled();
    // Not announced each second: no live region holds the count.
    expect(await button.evaluate((b) => b.closest("[aria-live]"))).toBeNull();

    await page.clock.runFor(cooldown * 1000);
    await expect(button).toBeEnabled();
    await expect(button).toHaveText(RESEND);
  });

  test("register, resend, and the second link verifies the account", async ({ page }) => {
    const email = uniqueEmail("e2e-resend");
    await page.clock.install();
    await register(page, email);
    const button = page.getByRole("button", { name: /^Renvoyer l'email/ });
    const cooldown = Number(await button.getAttribute("data-resend-cooldown"));
    await page.clock.runFor(cooldown * 1000);
    await expect(button).toBeEnabled();
    // apps/api keeps its own clock: the registration's token is moved past
    // the cooldown instead.
    await ageVerificationTokens(email, cooldown + 1);

    await page.getByLabel("Email").fill(email);
    await button.click();
    await expect(page).toHaveURL(/\/verify-email\/resend$/);
    await expect(page.getByText(SENT)).toBeVisible();
    // A new email left: the countdown starts again.
    await expect(page.getByRole("button", { name: /^Renvoyer l'email/ })).toBeDisabled();

    // The suite's apps/api has no mailbox to read (its SMTP host is a dummy,
    // and a failed send is only logged): the second email is seen as the
    // token it carries — a new one, the first consumed.
    expect(await countVerificationTokens(email)).toEqual({ issued: 2, unconsumed: 1 });
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);
    await expect(page.getByText("Email vérifié")).toBeVisible();
  });

  test("the login page leads to the same form, which says the same to everyone", async ({
    page,
  }) => {
    const pending = uniqueEmail("e2e-resend-pending");
    await register(page, pending);
    const verified = uniqueEmail("e2e-resend-verified");
    await register(page, verified);
    await page.goto(`/verify-email?token=${await fetchVerificationToken(verified)}`);
    await expect(page.getByText("Email vérifié")).toBeVisible();

    const answers: string[] = [];
    for (const email of [uniqueEmail("e2e-resend-unknown"), verified, pending]) {
      await page.goto("/login");
      await page.getByRole("link", { name: "Email de vérification non reçu ?" }).click();
      await expect(page).toHaveURL(/\/verify-email\/resend$/);
      await page.getByLabel("Email").fill(email);
      // The answer as the server sent it: once loaded, the page's button
      // counts down, and its label depends on when it is read.
      const [answer] = await Promise.all([
        page.waitForResponse(
          (r) => r.request().method() === "POST" && r.url().endsWith("/verify-email/resend"),
        ),
        page.getByRole("button", { name: RESEND }).click(),
      ]);
      await expect(page.getByText(SENT)).toBeVisible();
      answers.push(`${answer.status()} ${await answer.text()}`);
    }
    expect(answers[0]).toMatch(/^200 /);
    expect(answers[1]).toBe(answers[0]);
    expect(answers[2]).toBe(answers[0]);
  });
});

test.describe("Auth — forgot → reset → login with new password", () => {
  test("full journey", async ({ page }) => {
    const email = uniqueEmail("e2e-reset");
    const oldPassword = "e2e-old-password-1";
    const newPassword = "e2e-new-password-2";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Reset User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(oldPassword);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const verifyToken = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${verifyToken}`);

    await page.goto("/forgot-password");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("button", { name: "Envoyer le lien de réinitialisation" }).click();
    // Anti-enumeration: identical message regardless of whether the
    // account exists.
    await expect(page.getByText("Si ce compte existe, un email a été envoyé.")).toBeVisible();

    const resetToken = await fetchPasswordResetToken(email);
    // The emailed link carries the token in the fragment (#142): the
    // browser never sends it, so it stays out of the request line, and the
    // page scrubs it from the address bar once it has read it.
    const landing = page.waitForRequest((req) => req.url().includes("/reset-password"));
    await page.goto(`/reset-password#token=${resetToken}`);
    expect((await landing).url()).not.toContain(resetToken);
    await expect(page).toHaveURL(/\/reset-password$/);
    await page.getByLabel("Nouveau mot de passe").fill(newPassword);
    await page.getByRole("button", { name: "Réinitialiser" }).click();
    await expect(page.getByText("Mot de passe mis à jour")).toBeVisible();

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(newPassword);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/");
  });

  test("a link without a token in its fragment shows the invalid-link notice", async ({
    page,
  }) => {
    await page.goto("/reset-password");
    await expect(page.getByText("Ce lien de réinitialisation n'existe pas.")).toBeVisible();
    await expect(page.getByRole("button", { name: "Réinitialiser" })).toHaveCount(0);
  });
});

test.describe("Auth-gate redirects", () => {
  test("unauthenticated visitor hitting a non-auth route is redirected to /login", async ({
    page,
  }) => {
    await page.goto("/");
    await expect(page).toHaveURL(/\/login$/);
  });

  test("authenticated visitor hitting /login or /register is redirected to /", async ({
    page,
  }) => {
    const email = uniqueEmail("e2e-redirect");
    const password = "e2e-password-3";

    await page.goto("/register");
    await page.getByLabel("Email").fill(email);
    await page.getByLabel("Nom affiché").fill("Redirect User");
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("checkbox", { name: "Je déclare avoir 15 ans ou plus." }).check();
    await page.getByRole("checkbox", { name: "J'accepte les conditions générales d'utilisation." }).check();
    await page.getByRole("button", { name: "Créer mon compte" }).click();
    const token = await fetchVerificationToken(email);
    await page.goto(`/verify-email?token=${token}`);

    await page.goto("/login");
    await page.getByLabel("Email").fill(email);
    await page.getByRole("textbox", { name: "Mot de passe" }).fill(password);
    await page.getByRole("button", { name: "Se connecter" }).click();
    await expect(page).toHaveURL("/");

    await page.goto("/login");
    await expect(page).toHaveURL("/");

    await page.goto("/register");
    await expect(page).toHaveURL("/");
  });
});

// Google OAuth E2E: best-effort skipped. There is no test Google OAuth
// client/credentials available in this environment (real Google consent
// screen, no sandboxed provider), and apps/api's flow talks to Google's
// live userinfo endpoint (apps/api/src/auth/oauth_google.rs) with no mock
// seam today. A real round-trip test would need either a recorded/stubbed
// OAuth provider or dedicated test Google credentials, neither of which
// exist yet — tracked as follow-up, not blocking this epic.
test.describe("Auth — Google OAuth", () => {
  test.skip(
    true,
    "No test Google OAuth provider/credentials available in this environment; " +
      "apps/api/src/auth/oauth_google.rs talks to Google's live endpoints with no mock seam.",
  );
  test("continuing with Google reaches an authenticated session", async () => {
    // Intentionally left unimplemented — see the module-level skip reason.
  });
});
