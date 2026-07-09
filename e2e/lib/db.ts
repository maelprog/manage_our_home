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

function requireDatabaseUrl(): string {
  const url = process.env.DATABASE_URL;
  if (!url) {
    throw new Error(
      "DATABASE_URL must point at the same Postgres apps/api is using (see e2e/README.md)",
    );
  }
  return url;
}
