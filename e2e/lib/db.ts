import { Client } from "pg";

// Mirrors apps/api/tests/*_flow.rs's token-retrieval mechanism exactly:
// those integration tests read `email_verification_tokens`/
// `password_reset_tokens` directly off the test Postgres database rather
// than exposing any test-only HTTP endpoint (see e.g. auth_flow.rs's
// `SELECT t.token FROM email_verification_tokens t JOIN users u ...`).
// apps/api has no dev/test hook that returns tokens over HTTP, and adding
// one would weaken prod security for no real gain here — a direct DB read
// is the same trust boundary the Rust integration tests already rely on,
// just from Node instead of sqlx.
//
// Requires DATABASE_URL to point at the same Postgres apps/api is using.

export async function fetchVerificationToken(email: string): Promise<string> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rows } = await client.query(
      `SELECT t.token FROM email_verification_tokens t
       JOIN users u ON u.id = t.user_id
       WHERE u.email = $1
       ORDER BY t.created_at DESC
       LIMIT 1`,
      [email],
    );
    if (rows.length === 0) {
      throw new Error(`no verification token found for ${email}`);
    }
    return rows[0].token as string;
  } finally {
    await client.end();
  }
}

export async function fetchPasswordResetToken(email: string): Promise<string> {
  const client = new Client({ connectionString: requireDatabaseUrl() });
  await client.connect();
  try {
    const { rows } = await client.query(
      `SELECT t.token FROM password_reset_tokens t
       JOIN users u ON u.id = t.user_id
       WHERE u.email = $1
       ORDER BY t.created_at DESC
       LIMIT 1`,
      [email],
    );
    if (rows.length === 0) {
      throw new Error(`no password reset token found for ${email}`);
    }
    return rows[0].token as string;
  } finally {
    await client.end();
  }
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
