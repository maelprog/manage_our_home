import { Client } from "pg";

import { newBearerToken } from "./tokens.ts";

// Mirrors apps/api/tests/*_flow.rs's token-retrieval mechanism exactly:
// apps/api has no dev/test hook that returns tokens over HTTP, and adding
// one would weaken prod security for no real gain here — a direct DB access
// is the same trust boundary the Rust integration tests already rely on
// (`common::verification_token`), just from Node instead of sqlx.
//
// Since #335 the tables keep only the SHA-256 of each token, so the token
// the api mailed out cannot be read back. Instead the helpers below draw a
// fresh token (`tokens.ts`), set its hash on the latest row the api created
// for that account, and return the token: the row keeps its owner, expiry
// and state, only the secret changes.
//
// Requires DATABASE_URL to point at the same Postgres apps/api is using.

async function rekeyLatestToken(table: string, email: string): Promise<string | null> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { token, hash } = newBearerToken();
    const { rowCount } = await client.query(
      `UPDATE ${table} SET token_hash = $2
       WHERE token_hash = (
         SELECT t.token_hash FROM ${table} t
         JOIN users u ON u.id = t.user_id
         WHERE u.email = $1
         ORDER BY t.created_at DESC
         LIMIT 1
       )`,
      [email, hash],
    );
    return rowCount === 1 ? token : null;
  } finally {
    await client.end();
  }
}

export async function fetchVerificationToken(email: string): Promise<string> {
  const token = await rekeyLatestToken("email_verification_tokens", email);
  if (token === null) {
    throw new Error(`no verification token found for ${email}`);
  }
  return token;
}

export async function fetchPasswordResetToken(email: string): Promise<string> {
  const token = await rekeyLatestToken("password_reset_tokens", email);
  if (token === null) {
    throw new Error(`no password reset token found for ${email}`);
  }
  return token;
}

/**
 * Flips a user's `is_superadmin` flag directly in Postgres — mirrors how the
 * backend grants the global technical superadmin (there is no signup flow for
 * the role; it is set manually via SQL, see apps/api/src/user_admin/ and the
 * `make_superadmin` helper in apps/api/tests/user_admin_flow.rs). Used by the
 * F9 admin E2E suite to promote a freshly-registered account. The session's
 * `is_superadmin` is read live on every request, so no re-login is needed after
 * this — the next page load already sees the flag.
 */
export async function makeSuperadmin(email: string): Promise<void> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rowCount } = await client.query(
      `UPDATE users SET is_superadmin = true WHERE email = $1`,
      [email],
    );
    if (rowCount === 0) {
      throw new Error(`no user to promote for ${email}`);
    }
  } finally {
    await client.end();
  }
}

/**
 * Clears the age declaration of `email` (#318), leaving the account as one
 * opened through Google, or before #137, is: no declaration on file. The
 * registration form always records one, and a Google sign-in cannot be
 * driven from the suite, so this is how the declaration page is reached.
 */
export async function clearAgeDeclaration(email: string): Promise<void> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rowCount } = await client.query(
      `UPDATE users SET age_declared_at = NULL WHERE email = $1`,
      [email],
    );
    if (rowCount === 0) {
      throw new Error(`no user to clear the age declaration of for ${email}`);
    }
  } finally {
    await client.end();
  }
}

/**
 * Clears the acceptance of the CGU of `email` (#319), leaving the account as
 * one opened through Google, or before #319, is: no acceptance on file. Same
 * reason as `clearAgeDeclaration`: the registration form always records one.
 */
export async function clearTermsAcceptance(email: string): Promise<void> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rowCount } = await client.query(
      `UPDATE users SET terms_accepted_version = NULL, terms_accepted_at = NULL
       WHERE email = $1`,
      [email],
    );
    if (rowCount === 0) {
      throw new Error(`no user to clear the acceptance of the CGU of for ${email}`);
    }
  } finally {
    await client.end();
  }
}

/**
 * Records `version` as the version of the CGU `email` last accepted (#319),
 * as if the CGU had changed since: the account then gets the notice of a new
 * version, which the suite cannot otherwise bring about without a second
 * build.
 */
export async function setTermsAcceptedVersion(email: string, version: string): Promise<void> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rowCount } = await client.query(
      `UPDATE users SET terms_accepted_version = $2, terms_accepted_at = now()
       WHERE email = $1`,
      [email, version],
    );
    if (rowCount === 0) {
      throw new Error(`no user to set the accepted version of the CGU of for ${email}`);
    }
  } finally {
    await client.end();
  }
}

/**
 * Backdates every session of `email` as if opened `hours` hours ago (and
 * last used then too), so the superadmin session cap (#226) can be met
 * without waiting 12 hours.
 */
export async function ageSessions(email: string, hours: number): Promise<void> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rowCount } = await client.query(
      `UPDATE sessions s
          SET created_at = now() - make_interval(hours => $2),
              last_seen_at = now() - make_interval(hours => $2)
         FROM users u
        WHERE u.id = s.user_id AND u.email = $1`,
      [email, hours],
    );
    if (rowCount === 0) {
      throw new Error(`no session to age for ${email}`);
    }
  } finally {
    await client.end();
  }
}

/**
 * Backdates every verification token of `email` by `seconds`, so a resend
 * can be asked past apps/api's cooldown (#420) without waiting it out: the
 * browser's clock can be driven, the server's cannot.
 */
export async function ageVerificationTokens(email: string, seconds: number): Promise<void> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rowCount } = await client.query(
      `UPDATE email_verification_tokens t
          SET created_at = t.created_at - make_interval(secs => $2)
         FROM users u
        WHERE u.id = t.user_id AND u.email = $1`,
      [email, seconds],
    );
    if (rowCount === 0) {
      throw new Error(`no verification token to age for ${email}`);
    }
  } finally {
    await client.end();
  }
}

/** The verification tokens of `email`: all of them, and those still usable. */
export async function countVerificationTokens(
  email: string,
): Promise<{ issued: number; unconsumed: number }> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rows } = await client.query(
      `SELECT count(*)::int AS issued,
              count(*) FILTER (WHERE t.consumed_at IS NULL)::int AS unconsumed
         FROM email_verification_tokens t
         JOIN users u ON u.id = t.user_id
        WHERE u.email = $1`,
      [email],
    );
    return rows[0];
  } finally {
    await client.end();
  }
}

/**
 * Leaves the group named `groupName` the way the account purge can (#323):
 * `memberEmail` joins it as a standard member, and its owner's membership
 * goes — the group has no owner. The purge itself runs hourly in apps/api
 * and cannot be driven from the suite.
 */
export async function makeGroupOwnerless(groupName: string, memberEmail: string): Promise<void> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const joined = await client.query(
      `INSERT INTO group_members (group_id, user_id, role)
       SELECT g.id, u.id, 'standard' FROM groups g, users u
        WHERE g.name = $1 AND u.email = $2`,
      [groupName, memberEmail],
    );
    if (joined.rowCount !== 1) {
      throw new Error(`could not add ${memberEmail} to ${groupName}`);
    }
    const left = await client.query(
      `DELETE FROM group_members
        WHERE role = 'owner' AND group_id = (SELECT id FROM groups WHERE name = $1)`,
      [groupName],
    );
    if (left.rowCount !== 1) {
      throw new Error(`no owner to remove from ${groupName}`);
    }
  } finally {
    await client.end();
  }
}

/**
 * The stored bounds of the event `title` created by `email`, as Europe/Paris
 * wall-clock `YYYY-MM-DDTHH:MM` strings — the app's fixed display timezone.
 *
 * Since #117 an all-day event's edit form shows two dates, the end being the
 * last day covered, so the form can no longer tell a normalized row
 * (midnight → the midnight after) from one that kept the time of day it was
 * typed with. The normalization #101 added is a property of what is
 * *stored*; this reads it where it lives.
 */
export async function fetchEventBounds(
  email: string,
  title: string,
): Promise<{ starts: string; ends: string }> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rows } = await client.query(
      `SELECT to_char(e.starts_at AT TIME ZONE 'Europe/Paris', 'YYYY-MM-DD"T"HH24:MI') AS starts,
              to_char(e.ends_at AT TIME ZONE 'Europe/Paris', 'YYYY-MM-DD"T"HH24:MI') AS ends
       FROM events e
       JOIN users u ON u.id = e.created_by
       WHERE u.email = $1 AND e.title = $2
       ORDER BY e.created_at DESC
       LIMIT 1`,
      [email, title],
    );
    if (rows.length === 0) {
      throw new Error(`no event "${title}" found for ${email}`);
    }
    return { starts: rows[0].starts as string, ends: rows[0].ends as string };
  } finally {
    await client.end();
  }
}

function requireDatabaseUrl(): string {
  const url = process.env.DATABASE_URL;
  if (!url) {
    throw new Error(
      "DATABASE_URL must point at the same Postgres apps/api is using (see e2e/README.md)",
    );
  }
  return url;
}
