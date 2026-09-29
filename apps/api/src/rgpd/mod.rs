pub mod export;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{http::header, http::StatusCode, Json};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit;
use crate::auth::session::{scoped_tx, user_scoped_tx, AuthUser};
use crate::error::{AppError, AppResult};
use crate::AppState;

/// GET /account/export — Art. 15 (accès) and Art. 20 (portabilité).
/// Self-service: a user can only ever export their own data (scoped by
/// `AuthUser`, no target-user parameter exists on this route).
///
/// Its scope is everything held that concerns the caller (#140): what they
/// created, what others did that names them (events they are assigned to,
/// audit entries targeting their account), their sessions, Google identity,
/// invitations, read markers and completions — and that in every group still
/// holding such a row, including a group they have left or were removed
/// from, since leaving deletes the membership and nothing else.
///
/// Every family-scoped category is fetched per group through `scoped_tx`
/// (RLS-enforced) and filtered at the SQL layer on the column that names the
/// caller, so this is never a "dump my whole family's data" endpoint. Secrets
/// are left out: session ids (they are the session cookie), invitation and
/// email tokens, OAuth refresh tokens, attachment storage keys and ICS feed
/// URLs. Attachments come as metadata plus a short-lived presigned URL.
pub async fn export_account(
    State(state): State<AppState>,
    auth: AuthUser,
) -> AppResult<impl IntoResponse> {
    let profile = sqlx::query!(
        r#"SELECT id, email, email_verified, display_name, created_at, age_declared_at,
                  deletion_requested_at, (password_hash IS NOT NULL) AS "has_password!"
           FROM users WHERE id = $1"#,
        auth.user_id
    )
    .fetch_one(&state.db)
    .await?;
    let profile_json = json!({
        "id": profile.id,
        "email": profile.email,
        "email_verified": profile.email_verified,
        "display_name": profile.display_name,
        "created_at": profile.created_at,
        // #137: the art. 8 GDPR age declaration is data held about the person,
        // so art. 15 puts it in their export like the rest of the profile.
        // `null` for an account that never made one (Google sign-in, or an
        // account older than the declaration).
        "age_declared_at": profile.age_declared_at,
        // Whether a password is set, never its hash.
        "has_password": profile.has_password,
        "deletion_requested_at": profile.deletion_requested_at,
    });

    // "My groups" are read from the caller's own `group_members` rows, with
    // role and joined_at in the same query (issue #209, as `list_groups` since
    // #205). The membership filter is written here and does not rest on RLS:
    // under a role that bypasses it (the superuser of the `e2e` job and of
    // infra/docker-compose.yml) an unfiltered `SELECT … FROM groups` returned
    // every group of the database. Under the role apps/api/README.md
    // prescribes, the policies of 0014 (`group_members_self_read`,
    // `groups_membership_read`) let these rows through with only
    // `app.user_id` set, and apply the same filter a second time.
    //
    // The groups the caller no longer belongs to but that still hold a row
    // naming them come from `account_export_group_ids()` (0019, #140): the
    // runtime role cannot see across families, and that function answers
    // with group ids only, for the user of `app.user_id` only. The content
    // tables still require `app.family_id`, hence one `scoped_tx` per group
    // below.
    let mut group_tx = user_scoped_tx(&state.db, auth.user_id).await?;
    let memberships = sqlx::query!(
        r#"SELECT g.id, g.name, gm.role AS "role: String", gm.joined_at,
                  (g.created_by = $1) AS "created_by_me!"
           FROM group_members gm
           JOIN groups g ON g.id = gm.group_id
           WHERE gm.user_id = $1
           ORDER BY g.name"#,
        auth.user_id
    )
    .fetch_all(&mut *group_tx)
    .await?;
    let holding_my_rows = sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM account_export_group_ids() AS id ORDER BY 1"#
    )
    .fetch_all(&mut *group_tx)
    .await?;
    group_tx.commit().await?;

    let member_of: Vec<Uuid> = memberships.iter().map(|m| m.id).collect();
    let scope = export::export_scope(&member_of, &holding_my_rows);

    let mut c = export::ExportCategories::default();
    for m in &memberships {
        c.group_memberships.push(json!({
            "group_id": m.id,
            "name": m.name,
            "role": m.role,
            "joined_at": m.joined_at,
            "created_by_me": m.created_by_me,
        }));
    }
    // (storage_key, the attachment's JSON) — presigned once every group has
    // been read, outside any transaction.
    let mut attachments: Vec<(String, Value)> = Vec::new();

    for group in &scope {
        let group_id = group.group_id;
        let mut tx = scoped_tx(&state.db, group_id, auth.user_id).await?;

        if !group.is_member {
            // Deleted since `account_export_group_ids()` answered: its
            // content went with it (ON DELETE CASCADE), nothing to read.
            let Some(former) = sqlx::query!(
                r#"SELECT name, (created_by = $2) AS "created_by_me!" FROM groups WHERE id = $1"#,
                group_id,
                auth.user_id
            )
            .fetch_optional(&mut *tx)
            .await?
            else {
                tx.commit().await?;
                continue;
            };
            c.former_groups.push(json!({
                "group_id": group_id,
                "name": former.name,
                "created_by_me": former.created_by_me,
            }));
        }

        let events = sqlx::query!(
            r#"SELECT id, group_id, title, description, location, starts_at, ends_at, all_day, is_task, completed_at, rrule, created_at
               FROM events WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.agenda_events.extend(events.into_iter().map(|e| {
            json!({
                "id": e.id, "group_id": e.group_id, "title": e.title, "description": e.description,
                "location": e.location, "starts_at": e.starts_at, "ends_at": e.ends_at,
                "all_day": e.all_day, "is_task": e.is_task, "completed_at": e.completed_at,
                "rrule": e.rrule, "created_at": e.created_at,
            })
        }));

        // Art. 15: being assigned to an event is data about the person,
        // whoever created the event and whoever made the assignment.
        let assignments = sqlx::query!(
            r#"SELECT e.id, e.group_id, e.title, e.description, e.location, e.starts_at, e.ends_at,
                      e.all_day, e.is_task, e.completed_at, e.rrule, ea.created_at AS assigned_at
               FROM event_assignees ea JOIN events e ON e.id = ea.event_id
               WHERE e.group_id = $1 AND ea.user_id = $2 ORDER BY ea.created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.event_assignments.extend(assignments.into_iter().map(|e| {
            json!({
                "event_id": e.id, "group_id": e.group_id, "title": e.title,
                "description": e.description, "location": e.location,
                "starts_at": e.starts_at, "ends_at": e.ends_at, "all_day": e.all_day,
                "is_task": e.is_task, "completed_at": e.completed_at, "rrule": e.rrule,
                "assigned_at": e.assigned_at,
            })
        }));

        // Reminders carry no author: they belong to the event, and their
        // emails go to its creator (`jobs::scheduled_notifications`).
        let reminders = sqlx::query!(
            r#"SELECT r.id, r.event_id, r.offset_minutes, r.channel, r.created_at
               FROM event_reminders r JOIN events e ON e.id = r.event_id
               WHERE e.group_id = $1 AND e.created_by = $2 ORDER BY r.created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.event_reminders.extend(reminders.into_iter().map(|r| {
            json!({
                "id": r.id, "event_id": r.event_id, "group_id": group_id,
                "offset_minutes": r.offset_minutes, "channel": r.channel,
                "created_at": r.created_at,
            })
        }));

        let notifications = sqlx::query!(
            r#"SELECT sn.id, sn.event_id, sn.occurrence_at, sn.fire_at, sn.status, sn.attempts,
                      sn.last_error, sn.created_at
               FROM scheduled_notifications sn JOIN events e ON e.id = sn.event_id
               WHERE e.group_id = $1 AND e.created_by = $2 ORDER BY sn.fire_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.reminder_notifications
            .extend(notifications.into_iter().map(|n| {
                json!({
                    "id": n.id, "event_id": n.event_id, "group_id": group_id,
                    "occurrence_at": n.occurrence_at, "fire_at": n.fire_at,
                    "status": n.status, "attempts": n.attempts,
                    "last_error": n.last_error, "created_at": n.created_at,
                })
            }));

        let attachment_rows = sqlx::query!(
            r#"SELECT a.id, a.event_id, a.storage_key, a.filename, a.mime_type, a.size_bytes, a.created_at
               FROM event_attachments a JOIN events e ON e.id = a.event_id
               WHERE e.group_id = $1 AND a.uploaded_by = $2 ORDER BY a.created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        attachments.extend(attachment_rows.into_iter().map(|a| {
            (
                a.storage_key,
                json!({
                    "id": a.id, "event_id": a.event_id, "group_id": group_id,
                    "filename": a.filename, "mime_type": a.mime_type,
                    "size_bytes": a.size_bytes, "created_at": a.created_at,
                }),
            )
        }));

        let completions = sqlx::query!(
            r#"SELECT c.event_id, c.occurrence_at, c.completed_at
               FROM event_occurrence_completions c JOIN events e ON e.id = c.event_id
               WHERE e.group_id = $1 AND c.completed_by = $2 ORDER BY c.completed_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.event_completions.extend(completions.into_iter().map(|x| {
            json!({
                "event_id": x.event_id, "group_id": group_id,
                "occurrence_at": x.occurrence_at, "completed_at": x.completed_at,
            })
        }));

        let stocks = sqlx::query!(
            r#"SELECT id, group_id, name, category, quantity, unit, reorder_threshold, created_at
               FROM stock_items WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.stock_items.extend(stocks.into_iter().map(|s| {
            json!({
                "id": s.id, "group_id": s.group_id, "name": s.name, "category": s.category,
                "quantity": s.quantity, "unit": s.unit, "reorder_threshold": s.reorder_threshold,
                "created_at": s.created_at,
            })
        }));

        let recipe_rows = sqlx::query!(
            r#"SELECT id, group_id, name, instructions, created_at
               FROM recipes WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.recipes.extend(recipe_rows.into_iter().map(|r| {
            json!({
                "id": r.id, "group_id": r.group_id, "name": r.name,
                "instructions": r.instructions, "created_at": r.created_at,
            })
        }));

        let meals = sqlx::query!(
            r#"SELECT id, group_id, recipe_id, eaten_on, created_at
               FROM meal_history WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.meal_history.extend(meals.into_iter().map(|m| {
            json!({
                "id": m.id, "group_id": m.group_id, "recipe_id": m.recipe_id,
                "eaten_on": m.eaten_on, "created_at": m.created_at,
            })
        }));

        let groceries = sqlx::query!(
            r#"SELECT id, group_id, name, quantity, unit, checked, source, created_at
               FROM grocery_items WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.grocery_items.extend(groceries.into_iter().map(|g| {
            json!({
                "id": g.id, "group_id": g.group_id, "name": g.name, "quantity": g.quantity,
                "unit": g.unit, "checked": g.checked, "source": g.source, "created_at": g.created_at,
            })
        }));

        let budget_rows = sqlx::query!(
            r#"SELECT id, group_id, name, amount_cents, spent_at, created_at
               FROM budget_entries WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.budget_entries.extend(budget_rows.into_iter().map(|b| {
            json!({
                "id": b.id, "group_id": b.group_id, "name": b.name,
                "amount": b.amount_cents as f64 / 100.0, "spent_at": b.spent_at,
                "created_at": b.created_at,
            })
        }));

        let message_rows = sqlx::query!(
            r#"SELECT id, group_id, pgp_sym_decrypt(content, $3) as "content!", edited_at, created_at
               FROM messages WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id,
            state.message_encryption_key,
        )
        .fetch_all(&mut *tx)
        .await?;
        c.messages.extend(message_rows.into_iter().map(|m| {
            json!({
                "id": m.id, "group_id": m.group_id, "content": m.content,
                "edited_at": m.edited_at, "created_at": m.created_at,
            })
        }));

        let read_state = sqlx::query!(
            "SELECT last_read_at FROM message_read_state WHERE group_id = $1 AND user_id = $2",
            group_id,
            auth.user_id
        )
        .fetch_optional(&mut *tx)
        .await?;
        c.message_read_state.extend(
            read_state.map(|r| json!({ "group_id": group_id, "last_read_at": r.last_read_at })),
        );

        // The ICS feed URL is a bearer credential (see 0010's migration
        // comment / `google_calendar::imports`'s doc comment on why the
        // `GET` endpoint never returns it decrypted either) — export
        // includes the import's metadata but not the secret URL itself.
        let calendar_rows = sqlx::query!(
            r#"SELECT id, group_id, label, last_imported_at, created_at
               FROM calendar_imports WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.calendar_imports
            .extend(calendar_rows.into_iter().map(|ci| {
                json!({
                    "id": ci.id, "group_id": ci.group_id, "label": ci.label,
                    "last_imported_at": ci.last_imported_at, "created_at": ci.created_at,
                })
            }));

        // Pending ones only: accepting an invitation deletes it
        // (`groups::accept_invitation`), the membership's `joined_at` is what
        // remains of it. The token is the link itself: left out, like every
        // bearer secret here.
        let sent = sqlx::query!(
            r#"SELECT id, invited_email, created_at, expires_at, consumed_at
               FROM invitations WHERE group_id = $1 AND created_by = $2 ORDER BY created_at"#,
            group_id,
            auth.user_id
        )
        .fetch_all(&mut *tx)
        .await?;
        c.invitations_sent.extend(sent.into_iter().map(|i| {
            json!({
                "id": i.id, "group_id": group_id, "invited_email": i.invited_email,
                "created_at": i.created_at, "expires_at": i.expires_at,
                "consumed_at": i.consumed_at,
            })
        }));

        tx.commit().await?;
    }

    // The bytes are not inlined: a presigned GET, valid for
    // `storage::PRESIGNED_URL_TTL`, like the one the agenda hands out.
    for (storage_key, mut attachment) in attachments {
        let url = state
            .storage
            .presigned_get_url(&storage_key)
            .await
            .map_err(AppError::Internal)?;
        attachment["download_url"] = json!(url);
        c.event_attachments.push(attachment);
    }

    // Account-level tables, not family-scoped and not under RLS.
    let sessions = sqlx::query!(
        r#"SELECT created_at, last_seen_at, expires_at, revoked_at, restricted
           FROM sessions WHERE user_id = $1 ORDER BY created_at"#,
        auth.user_id
    )
    .fetch_all(&state.db)
    .await?;
    c.sessions = sessions
        .into_iter()
        .map(|s| {
            json!({
                "created_at": s.created_at, "last_seen_at": s.last_seen_at,
                "expires_at": s.expires_at, "revoked_at": s.revoked_at,
                "restricted": s.restricted,
            })
        })
        .collect();

    let identities = sqlx::query!(
        r#"SELECT provider, provider_user_id, linked_at
           FROM oauth_identities WHERE user_id = $1 ORDER BY linked_at"#,
        auth.user_id
    )
    .fetch_all(&state.db)
    .await?;
    c.oauth_identities = identities
        .into_iter()
        .map(|o| {
            json!({
                "provider": o.provider, "provider_user_id": o.provider_user_id,
                "linked_at": o.linked_at,
            })
        })
        .collect();

    let verifications = sqlx::query!(
        r#"SELECT created_at, expires_at, consumed_at
           FROM email_verification_tokens WHERE user_id = $1 ORDER BY created_at"#,
        auth.user_id
    )
    .fetch_all(&state.db)
    .await?;
    c.email_verifications = verifications
        .into_iter()
        .map(|t| {
            json!({
                "created_at": t.created_at, "expires_at": t.expires_at,
                "consumed_at": t.consumed_at,
            })
        })
        .collect();

    let resets = sqlx::query!(
        r#"SELECT created_at, expires_at, consumed_at
           FROM password_reset_tokens WHERE user_id = $1 ORDER BY created_at"#,
        auth.user_id
    )
    .fetch_all(&state.db)
    .await?;
    c.password_resets = resets
        .into_iter()
        .map(|t| {
            json!({
                "created_at": t.created_at, "expires_at": t.expires_at,
                "consumed_at": t.consumed_at,
            })
        })
        .collect();

    // The caller's own actions, and those of others that name them — on
    // their account, on their role in a group, a group handed to them —
    // without the other actor's id.
    let audit_rows = sqlx::query!(
        r#"SELECT occurred_at, action, target_type, target_id, metadata,
                  (actor_user_id IS NOT DISTINCT FROM $1) AS "by_me!"
           FROM audit_log
           WHERE actor_user_id = $1
              OR (target_type IN ('user', 'group_member') AND target_id = $1::text)
              OR metadata->>'new_owner_id' = $1::text
           ORDER BY occurred_at, id"#,
        auth.user_id
    )
    .fetch_all(&state.db)
    .await?;
    c.audit_log = audit_rows
        .into_iter()
        .map(|a| {
            json!({
                "occurred_at": a.occurred_at, "action": a.action,
                "target_type": a.target_type, "target_id": a.target_id,
                "metadata": a.metadata, "by_me": a.by_me,
            })
        })
        .collect();

    let doc = export::build_export(profile_json, c);

    let mut audit_tx = crate::db::begin(&state.db).await?;
    audit::record(
        &mut audit_tx,
        Some(auth.user_id),
        "account_data_exported",
        "user",
        &auth.user_id.to_string(),
        json!({}),
    )
    .await?;
    audit_tx.commit().await?;

    Ok(Json(doc))
}

/// GET /privacy-policy — static document, no auth required (a prospective
/// user needs to be able to read it before registering). Content lives in
/// `docs/privacy-policy.md` (repo root) and is compiled into the binary via
/// `include_str!`, so there's no filesystem dependency at runtime and no
/// drift between what's deployed and what's in source control.
const PRIVACY_POLICY_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/privacy-policy.md"
));

pub async fn privacy_policy() -> Response {
    markdown_document(PRIVACY_POLICY_MD)
}

/// GET /legal-notice — the LCEN art. 6-III notice (publisher, publication
/// director, host), and GET /terms-of-service — the CGU (#132). Same three
/// properties as the privacy policy above, for the same reasons: public, no
/// session, compiled in from `docs/` so the served text and the versioned one
/// cannot drift.
///
/// They live in this module rather than one of their own because what groups
/// the three is the way they are served, not the regulation behind them: one
/// `include_str!`, one public route, one markdown response, rendered by
/// `apps/web` with the same `validation::rgpd::render_markdown`.
const LEGAL_NOTICE_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/legal-notice.md"
));

const TERMS_OF_SERVICE_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/terms-of-service.md"
));

pub async fn legal_notice() -> Response {
    markdown_document(LEGAL_NOTICE_MD)
}

pub async fn terms_of_service() -> Response {
    markdown_document(TERMS_OF_SERVICE_MD)
}

/// The one response shape these three documents share. `text/markdown` and not
/// HTML: rendering is `apps/web`'s job, and the API stays the single source of
/// the text itself.
fn markdown_document(md: &'static str) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        md,
    )
        .into_response()
}
