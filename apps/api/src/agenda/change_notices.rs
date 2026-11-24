//! Telling the members who see an agenda event that another one added,
//! changed or deleted it (#849): one `agenda_change` row of the centre per
//! recipient, written in the transaction of the write it reports.
//!
//! Who is told is decided here, in pure functions, from what the event
//! looks like to each member before and after the write — a private event
//! (#620) is seen by its participants alone, and nobody is told of one they
//! do not see, nor named its title. Who may be told at all, a database
//! read: the family's members whose account is active and who keep the
//! switch of that news on — `notify_agenda_added`, `_changed` or
//! `_deleted` (#853, one setting for all three until then); an import is
//! told to those who keep the additions' or the changes' on.
//!
//! Not told, event by event: an event mirrored from an imported calendar —
//! its rows are rewritten from the feed — nor the generated days (bank
//! holidays, school holidays), which are no rows of `events`. An import
//! tells once for all it added or changed ([`tell_imported`], #852, called
//! by `google_calendar::imports`); the other writers are the handlers of
//! `events.rs`.
//!
//! Once the write committed, the members told whose channel includes
//! notifications (`users.reminder_channel`, #306) get a push on each of
//! their devices ([`push_in_background`], #851), as for the reminders: the
//! service worker asks which kind of push its device got
//! (`notifications::push::Pending`) and shows « Agenda de la famille
//! modifié » — neither the event's title nor its author. The push names
//! the member's entry of the centre, and its click marks it read (#862).
//! No email: a member on email alone reads the change in the centre.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agenda::is_participant;
use crate::error::AppResult;
use crate::jobs::scheduled_notifications::push_entries;
use crate::notifications::center::{self, AgendaChanges};
use crate::notifications::{fcm, push};
use crate::AppState;

/// Who sees an event: everyone in the family when it is shared, its
/// participants alone when it is private — the rule of the `events` policy
/// (`event_is_assigned_to_current_user`, migration 0044).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Audience<'a> {
    pub is_private: bool,
    pub created_by: Uuid,
    pub assignees: &'a [Uuid],
}

impl Audience<'_> {
    pub fn sees(&self, user: Uuid) -> bool {
        !self.is_private || is_participant(user, self.created_by, self.assignees)
    }
}

/// The members to tell of an event added or deleted: the `candidates` who
/// see it — after the addition, before the deletion — but its `author`, in
/// the order of `candidates`.
pub(crate) fn recipients(candidates: &[Uuid], author: Uuid, audience: &Audience<'_>) -> Vec<Uuid> {
    candidates
        .iter()
        .copied()
        .filter(|&member| member != author && audience.sees(member))
        .collect()
}

/// The members to tell of an import that added or changed the events
/// `touched` (#852), each with how many of them they see: an event the
/// import creates is shared, but one it changes may have been made private
/// by a participant (#620), and is then counted for its participants alone.
/// Never its `author`; nobody who sees none of them. In the order of
/// `candidates`.
pub(crate) fn import_counts(
    candidates: &[Uuid],
    author: Uuid,
    touched: &[Audience<'_>],
) -> Vec<(Uuid, usize)> {
    candidates
        .iter()
        .filter(|&&member| member != author)
        .map(|&member| {
            let seen = touched.iter().filter(|event| event.sees(member)).count();
            (member, seen)
        })
        .filter(|&(_, seen)| seen > 0)
        .collect()
}

/// The members to tell of a change, split by what they are told.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ChangeRecipients {
    /// They see the event now and did not before (made shared again, or
    /// added to a private one's participants): told it was added.
    pub added: Vec<Uuid>,
    /// They saw it and still do: told what changed, when something they
    /// see did.
    pub changed: Vec<Uuid>,
}

/// The members to tell of a change of an event from `before` to `after`.
/// One who no longer sees it is told nothing: not even its title leaves
/// the event once it is hidden from them.
pub(crate) fn change_recipients(
    candidates: &[Uuid],
    author: Uuid,
    before: &Audience<'_>,
    after: &Audience<'_>,
    changes: AgendaChanges,
) -> ChangeRecipients {
    let mut told = ChangeRecipients::default();
    for &member in candidates {
        if member == author || !after.sees(member) {
            continue;
        }
        if !before.sees(member) {
            told.added.push(member);
        } else if !changes.is_empty() {
            told.changed.push(member);
        }
    }
    told
}

/// What a member sees of an event, as far as a change of it is news.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Visible<'a> {
    pub title: &'a str,
    pub location: Option<&'a str>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub rrule: Option<&'a str>,
    pub assignees: &'a [Uuid],
}

/// Which visible fields moved from `before` to `after`. A blank location is
/// none (the edit form sends an empty one for « no location »), and the
/// assignees are a set: their order is not news.
pub(crate) fn visible_changes(before: &Visible<'_>, after: &Visible<'_>) -> AgendaChanges {
    fn location(typed: Option<&str>) -> Option<&str> {
        typed.map(str::trim).filter(|l| !l.is_empty())
    }
    let set = |assignees: &[Uuid]| {
        let mut set = assignees.to_vec();
        set.sort_unstable();
        set.dedup();
        set
    };
    AgendaChanges {
        when: before.starts_at != after.starts_at
            || before.ends_at != after.ends_at
            || before.all_day != after.all_day
            || before.rrule != after.rrule,
        location: location(before.location) != location(after.location),
        title: before.title != after.title,
        participants: set(before.assignees) != set(after.assignees),
    }
}

/// What a change tells: an event added, changed, deleted, or an import
/// that added or changed several (#852).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum News {
    Added,
    Changed,
    Deleted,
    Imported,
}

/// A member's three switches of `/account/notifications` (#853):
/// `users.notify_agenda_added`, `_changed` and `_deleted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgendaSwitches {
    pub added: bool,
    pub changed: bool,
    pub deleted: bool,
}

impl AgendaSwitches {
    /// Whether the member is told of `news`. An import adds or changes
    /// events: either switch lets it through.
    pub fn wants(self, news: News) -> bool {
        match news {
            News::Added => self.added,
            News::Changed => self.changed,
            News::Deleted => self.deleted,
            News::Imported => self.added || self.changed,
        }
    }
}

/// A member of the family who may be told of a change, with their
/// switches.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Candidate {
    pub id: Uuid,
    pub switches: AgendaSwitches,
}

/// The `candidates` who want `news`, in their order.
pub(crate) fn wanting(candidates: &[Candidate], news: News) -> Vec<Uuid> {
    candidates
        .iter()
        .filter(|c| c.switches.wants(news))
        .map(|c| c.id)
        .collect()
}

/// The members of `group_id` who may be told of an agenda change: active
/// accounts, with their switches ([`wanting`] keeps those who want a
/// news). The author is among them; the functions above leave them out.
pub(crate) async fn candidates(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
) -> AppResult<Vec<Candidate>> {
    let rows = sqlx::query!(
        r#"SELECT gm.user_id, u.notify_agenda_added, u.notify_agenda_changed,
                  u.notify_agenda_deleted
           FROM group_members gm
           JOIN users u ON u.id = gm.user_id
           WHERE gm.group_id = $1
             AND u.deactivated_at IS NULL AND u.deleted_at IS NULL
             AND (u.notify_agenda_added OR u.notify_agenda_changed
                  OR u.notify_agenda_deleted)
           ORDER BY gm.user_id"#,
        group_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Candidate {
            id: row.user_id,
            switches: AgendaSwitches {
                added: row.notify_agenda_added,
                changed: row.notify_agenda_changed,
                deleted: row.notify_agenda_deleted,
            },
        })
        .collect())
}

/// Writes `event` for `recipients`, about the event `event_id`. Returns the
/// entries written, for the push to tell their devices without deciding
/// again (#4), and to name each one (#862). The transaction must be `scoped_tx` on `group_id`: the
/// INSERT policy of 0036 checks the family, not the recipient.
pub(crate) async fn write(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    author: Uuid,
    event_id: Uuid,
    recipients: &[Uuid],
    event: center::Event<'_>,
) -> AppResult<Vec<center::Entry>> {
    if recipients.is_empty() {
        return Ok(Vec::new());
    }
    // No `dedup_key`: every recipient gets their row (the unique index of
    // 0036 holds on the key alone), under an id drawn by `insert_entries`.
    Ok(center::insert_entries(
        tx,
        recipients,
        &center::NewNotification {
            group_id,
            event,
            object_id: Some(event_id),
            actor_id: Some(author),
            dedup_key: None,
        },
    )
    .await?)
}

/// The member whose write is reported: never told of it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Author<'a> {
    pub id: Uuid,
    pub name: &'a str,
}

/// An event as one write left it, or found it: who sees it and what they
/// see.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shown<'a> {
    pub audience: Audience<'a>,
    pub visible: Visible<'a>,
}

/// Tells the members who see `shown` that `author` added it.
pub(crate) async fn tell_added(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    author: Author<'_>,
    event_id: Uuid,
    shown: &Shown<'_>,
) -> AppResult<Vec<center::Entry>> {
    let candidates = wanting(&candidates(tx, group_id).await?, News::Added);
    let told = recipients(&candidates, author.id, &shown.audience);
    write(
        tx,
        group_id,
        author.id,
        event_id,
        &told,
        added(author, shown),
    )
    .await
}

/// Tells the members who saw `shown` that `author` deleted it — or, for a
/// series cut or an occurrence excluded, the occurrences from
/// `shown.visible.starts_at`.
pub(crate) async fn tell_deleted(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    author: Author<'_>,
    event_id: Uuid,
    shown: &Shown<'_>,
) -> AppResult<Vec<center::Entry>> {
    let candidates = wanting(&candidates(tx, group_id).await?, News::Deleted);
    let told = recipients(&candidates, author.id, &shown.audience);
    let event = center::Event::AgendaDeleted {
        author: author.name,
        title: shown.visible.title,
        starts_at: shown.visible.starts_at,
        all_day: shown.visible.all_day,
    };
    write(tx, group_id, author.id, event_id, &told, event).await
}

/// Tells of a change from `before` to `after`, the event `event_id` holding
/// `after`: what changed to those who saw it and still do, that it was
/// added to those who see it only now. Returns everyone written, either
/// way.
pub(crate) async fn tell_changed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    author: Author<'_>,
    event_id: Uuid,
    before: &Shown<'_>,
    after: &Shown<'_>,
) -> AppResult<Vec<center::Entry>> {
    let changes = visible_changes(&before.visible, &after.visible);
    let candidates = candidates(tx, group_id).await?;
    let everyone: Vec<Uuid> = candidates.iter().map(|c| c.id).collect();
    let mut told = change_recipients(
        &everyone,
        author.id,
        &before.audience,
        &after.audience,
        changes,
    );
    // Each told by the switch of what they are told (#853).
    let (adds, changes_on) = (
        wanting(&candidates, News::Added),
        wanting(&candidates, News::Changed),
    );
    told.added.retain(|member| adds.contains(member));
    told.changed.retain(|member| changes_on.contains(member));
    let mut written = write(
        tx,
        group_id,
        author.id,
        event_id,
        &told.added,
        added(author, after),
    )
    .await?;
    let event = center::Event::AgendaChanged {
        author: author.name,
        title: after.visible.title,
        changes,
    };
    written.extend(write(tx, group_id, author.id, event_id, &told.changed, event).await?);
    Ok(written)
}

/// Tells the family that `author` ran an import which added or changed the
/// events `touched` (#852): one row per member for the whole import, with
/// the count of those events they see ([`import_counts`]), naming none —
/// its `object_id` is empty, there is no single event to open. Nothing to
/// a member who sees none of them. Returns the entries written, to push
/// once committed. Same transaction rule as [`write`]; the author sees
/// every event they just wrote, so all of `touched` is read back.
pub(crate) async fn tell_imported(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group_id: Uuid,
    author: Author<'_>,
    touched: &[Uuid],
) -> AppResult<Vec<center::Entry>> {
    if touched.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query!(
        r#"SELECT e.is_private, e.created_by,
                  ARRAY(SELECT ea.user_id FROM event_assignees ea
                        WHERE ea.event_id = e.id) AS "assignees!"
           FROM events e
           WHERE e.id = ANY($1)"#,
        touched,
    )
    .fetch_all(&mut **tx)
    .await?;
    let audiences: Vec<Audience<'_>> = rows
        .iter()
        .map(|row| Audience {
            is_private: row.is_private,
            created_by: row.created_by,
            assignees: &row.assignees,
        })
        .collect();
    let candidates = wanting(&candidates(tx, group_id).await?, News::Imported);
    let counts = import_counts(&candidates, author.id, &audiences);
    // One INSERT per distinct count: the text differs by count alone.
    let mut by_count: std::collections::BTreeMap<usize, Vec<Uuid>> = Default::default();
    for &(member, count) in &counts {
        by_count.entry(count).or_default().push(member);
    }
    let mut told = Vec::with_capacity(counts.len());
    for (count, members) in &by_count {
        // No `dedup_key`: as in `write`.
        told.extend(
            center::insert_entries(
                tx,
                members,
                &center::NewNotification {
                    group_id,
                    event: center::Event::AgendaImported {
                        author: author.name,
                        count: *count,
                    },
                    object_id: None,
                    actor_id: Some(author.id),
                    dedup_key: None,
                },
            )
            .await?,
        );
    }
    Ok(told)
}

/// How long a push service keeps a change for a device that is offline: a
/// day. A change read later is in the notification centre anyway.
pub const CHANGE_PUSH_TTL_SECS: i64 = 24 * 3600;

/// Pushes the devices of the members `told` whose channel includes
/// notifications (`ReminderChannel::includes_push`), recording each
/// device's answer as the reminders do (`push_entries`). Called by
/// [`push_in_background`]; `send_push` and `send_fcm` are the pushes
/// themselves, by Web Push and by FCM (#1003), parameters so that a test
/// can stand in for the push services — `None` for one the server has not
/// configured.
pub async fn push_changes<P, PFut, F, FFut>(
    pool: &sqlx::PgPool,
    send_push: Option<&P>,
    send_fcm: Option<&F>,
    told: &[center::Entry],
) -> anyhow::Result<()>
where
    P: Fn(String, i64, Option<push::Payload>) -> PFut,
    PFut: std::future::Future<Output = push::PushOutcome>,
    F: Fn(String, fcm::Message) -> FFut,
    FFut: std::future::Future<Output = push::PushOutcome>,
{
    push_entries(
        pool,
        send_push,
        send_fcm,
        told,
        push::PushKind::AgendaChange,
        CHANGE_PUSH_TTL_SECS,
    )
    .await
}

/// [`push_changes`] on a task of its own, once the write's transaction
/// committed: the request does not wait for the push services. Nothing
/// without members told, nor on a server with neither a VAPID key nor FCM.
pub(crate) fn push_in_background(state: &AppState, told: Vec<center::Entry>) {
    if told.is_empty() || (state.push.is_none() && state.fcm.is_none()) {
        return;
    }
    let pool = state.db.clone();
    let send_push = state.push.clone().map(push::web_sender);
    let send_fcm = state.fcm.clone().map(fcm::sender);
    tokio::spawn(async move {
        let pushed = push_changes(&pool, send_push.as_ref(), send_fcm.as_ref(), &told);
        if let Err(e) = pushed.await {
            tracing::error!(error = ?e, "failed to push an agenda change");
        }
    });
}

fn added<'a>(author: Author<'a>, shown: &Shown<'a>) -> center::Event<'a> {
    center::Event::AgendaAdded {
        author: author.name,
        title: shown.visible.title,
        starts_at: shown.visible.starts_at,
        all_day: shown.visible.all_day,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn uid(n: u8) -> Uuid {
        Uuid::from_u128(u128::from(n))
    }

    fn shared(created_by: Uuid) -> Audience<'static> {
        Audience {
            is_private: false,
            created_by,
            assignees: &[],
        }
    }

    // -- AgendaSwitches::wants, wanting (#853) ----------------------------

    fn switches(added: bool, changed: bool, deleted: bool) -> AgendaSwitches {
        AgendaSwitches {
            added,
            changed,
            deleted,
        }
    }

    #[test]
    fn each_switch_lets_its_own_news_through() {
        let only = |news| {
            [
                (switches(true, false, false), News::Added),
                (switches(false, true, false), News::Changed),
                (switches(false, false, true), News::Deleted),
            ]
            .into_iter()
            .filter(|(s, _)| s.wants(news))
            .map(|(_, n)| n)
            .collect::<Vec<_>>()
        };
        assert_eq!(only(News::Added), [News::Added]);
        assert_eq!(only(News::Changed), [News::Changed]);
        assert_eq!(only(News::Deleted), [News::Deleted]);
        for news in [News::Added, News::Changed, News::Deleted, News::Imported] {
            assert!(switches(true, true, true).wants(news), "{news:?}");
            assert!(!switches(false, false, false).wants(news), "{news:?}");
        }
    }

    /// An import adds or changes events (#852): told to a member who keeps
    /// either switch on, never on the deletions' alone.
    #[test]
    fn an_import_is_news_for_the_additions_or_the_changes() {
        assert!(switches(true, false, false).wants(News::Imported));
        assert!(switches(false, true, false).wants(News::Imported));
        assert!(!switches(false, false, true).wants(News::Imported));
    }

    #[test]
    fn wanting_keeps_the_candidates_order_and_drops_the_others() {
        let candidates = [
            Candidate {
                id: uid(3),
                switches: switches(true, false, true),
            },
            Candidate {
                id: uid(1),
                switches: switches(false, true, false),
            },
            Candidate {
                id: uid(2),
                switches: switches(true, true, false),
            },
        ];
        assert_eq!(wanting(&candidates, News::Added), [uid(3), uid(2)]);
        assert_eq!(wanting(&candidates, News::Changed), [uid(1), uid(2)]);
        assert_eq!(wanting(&candidates, News::Deleted), [uid(3)]);
        assert_eq!(
            wanting(&candidates, News::Imported),
            [uid(3), uid(1), uid(2)]
        );
        assert_eq!(wanting(&[], News::Added), Vec::<Uuid>::new());
    }

    // -- Audience::sees ---------------------------------------------------

    #[test]
    fn everyone_sees_a_shared_event() {
        assert!(shared(uid(1)).sees(uid(9)));
    }

    #[test]
    fn a_private_event_is_seen_by_its_creator_and_assignees_alone() {
        let assignees = [uid(2)];
        let private = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &assignees,
        };
        assert!(private.sees(uid(1)));
        assert!(private.sees(uid(2)));
        assert!(!private.sees(uid(3)));
    }

    // -- recipients -------------------------------------------------------

    #[test]
    fn a_shared_event_tells_every_candidate_but_its_author() {
        let candidates = [uid(1), uid(2), uid(3)];
        assert_eq!(
            recipients(&candidates, uid(2), &shared(uid(2))),
            [uid(1), uid(3)]
        );
    }

    #[test]
    fn a_private_event_tells_its_participants_but_its_author() {
        let candidates = [uid(1), uid(2), uid(3), uid(4)];
        let assignees = [uid(3), uid(1)];
        let private = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &assignees,
        };
        assert_eq!(recipients(&candidates, uid(1), &private), [uid(3)]);
        // Written by an assignee, it tells its creator.
        assert_eq!(recipients(&candidates, uid(3), &private), [uid(1)]);
    }

    /// A participant who is no candidate (setting off, account gone) is
    /// not told: the candidates bound the recipients.
    #[test]
    fn only_candidates_are_told() {
        let assignees = [uid(2), uid(3)];
        let private = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &assignees,
        };
        assert_eq!(recipients(&[uid(1), uid(3)], uid(1), &private), [uid(3)]);
        assert_eq!(recipients(&[], uid(1), &shared(uid(1))), Vec::<Uuid>::new());
    }

    // -- import_counts (#852) ---------------------------------------------

    #[test]
    fn an_import_of_shared_events_tells_every_candidate_but_its_author_the_count() {
        let candidates = [uid(1), uid(2), uid(3)];
        let touched = vec![shared(uid(2)); 30];
        assert_eq!(
            import_counts(&candidates, uid(2), &touched),
            [(uid(1), 30), (uid(3), 30)]
        );
    }

    #[test]
    fn an_import_that_changed_nothing_tells_no_one() {
        let candidates = [uid(1), uid(2), uid(3)];
        assert_eq!(import_counts(&candidates, uid(2), &[]), []);
    }

    /// An imported event made private (#620) counts for its participants
    /// alone; a member who sees none of the events is not told.
    #[test]
    fn a_private_imported_event_counts_for_its_participants_alone() {
        let candidates = [uid(1), uid(2), uid(3)];
        let assignees = [uid(1), uid(2)];
        let private = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &assignees,
        };
        assert_eq!(
            import_counts(&candidates, uid(1), &[private, shared(uid(1))]),
            [(uid(2), 2), (uid(3), 1)]
        );
        assert_eq!(
            import_counts(&candidates, uid(1), &[private, private]),
            [(uid(2), 2)]
        );
    }

    // -- change_recipients -------------------------------------------------

    fn some_change() -> AgendaChanges {
        AgendaChanges {
            title: true,
            ..AgendaChanges::default()
        }
    }

    #[test]
    fn a_change_of_a_shared_event_tells_the_others_what_changed() {
        let candidates = [uid(1), uid(2), uid(3)];
        assert_eq!(
            change_recipients(
                &candidates,
                uid(1),
                &shared(uid(1)),
                &shared(uid(1)),
                some_change()
            ),
            ChangeRecipients {
                added: vec![],
                changed: vec![uid(2), uid(3)],
            }
        );
    }

    #[test]
    fn nothing_visible_changed_tells_no_one() {
        let candidates = [uid(1), uid(2), uid(3)];
        assert_eq!(
            change_recipients(
                &candidates,
                uid(1),
                &shared(uid(1)),
                &shared(uid(1)),
                AgendaChanges::default()
            ),
            ChangeRecipients::default()
        );
    }

    /// Made private: the ones who stop seeing it are told nothing, its
    /// participants what changed.
    #[test]
    fn made_private_it_tells_nothing_to_those_who_lose_it() {
        let candidates = [uid(1), uid(2), uid(3)];
        let assignees = [uid(2)];
        let private = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &assignees,
        };
        assert_eq!(
            change_recipients(
                &candidates,
                uid(1),
                &shared(uid(1)),
                &private,
                some_change()
            ),
            ChangeRecipients {
                added: vec![],
                changed: vec![uid(2)],
            }
        );
        // Nothing else changed: nobody at all.
        assert_eq!(
            change_recipients(
                &candidates,
                uid(1),
                &shared(uid(1)),
                &private,
                AgendaChanges::default()
            ),
            ChangeRecipients::default()
        );
    }

    /// Made shared again: the ones who see it now are told it was added,
    /// whatever else changed; its participants only what changed.
    #[test]
    fn made_shared_it_is_added_for_those_who_see_it_now() {
        let candidates = [uid(1), uid(2), uid(3)];
        let assignees = [uid(2)];
        let private = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &assignees,
        };
        assert_eq!(
            change_recipients(
                &candidates,
                uid(1),
                &private,
                &shared(uid(1)),
                AgendaChanges::default()
            ),
            ChangeRecipients {
                added: vec![uid(3)],
                changed: vec![],
            }
        );
        assert_eq!(
            change_recipients(
                &candidates,
                uid(1),
                &private,
                &shared(uid(1)),
                some_change()
            ),
            ChangeRecipients {
                added: vec![uid(3)],
                changed: vec![uid(2)],
            }
        );
    }

    /// Added to a private event's participants: told it was added; the
    /// other participants, that its participants changed.
    #[test]
    fn a_new_participant_of_a_private_event_is_told_it_was_added() {
        let candidates = [uid(1), uid(2), uid(3), uid(4)];
        let before_assignees = [uid(2)];
        let after_assignees = [uid(2), uid(3)];
        let before = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &before_assignees,
        };
        let after = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &after_assignees,
        };
        assert_eq!(
            change_recipients(
                &candidates,
                uid(1),
                &before,
                &after,
                AgendaChanges {
                    participants: true,
                    ..AgendaChanges::default()
                }
            ),
            ChangeRecipients {
                added: vec![uid(3)],
                changed: vec![uid(2)],
            }
        );
    }

    /// The author is never told, even when the change makes them see it.
    #[test]
    fn the_author_is_never_told_of_their_own_change() {
        let candidates = [uid(1), uid(2)];
        let assignees = [uid(1)];
        let private = Audience {
            is_private: true,
            created_by: uid(1),
            assignees: &assignees,
        };
        // uid(2), an admin outside the private event, makes it shared.
        assert_eq!(
            change_recipients(
                &candidates,
                uid(2),
                &private,
                &shared(uid(1)),
                some_change()
            ),
            ChangeRecipients {
                added: vec![],
                changed: vec![uid(1)],
            }
        );
    }

    // -- visible_changes ----------------------------------------------------

    fn visible(assignees: &[Uuid]) -> Visible<'_> {
        let at = Utc.with_ymd_and_hms(2026, 10, 3, 14, 30, 0).unwrap();
        Visible {
            title: "Dîner",
            location: Some("Maison"),
            starts_at: at,
            ends_at: at + chrono::Duration::hours(1),
            all_day: false,
            rrule: None,
            assignees,
        }
    }

    #[test]
    fn the_same_event_changed_nothing() {
        let a = [uid(1), uid(2)];
        assert!(visible_changes(&visible(&a), &visible(&a)).is_empty());
    }

    #[test]
    fn its_bounds_flag_or_rule_change_when_it_happens() {
        let a = [uid(1)];
        let before = visible(&a);
        let only_when = AgendaChanges {
            when: true,
            ..AgendaChanges::default()
        };
        for after in [
            Visible {
                starts_at: before.starts_at - chrono::Duration::minutes(30),
                ..before
            },
            Visible {
                ends_at: before.ends_at + chrono::Duration::minutes(30),
                ..before
            },
            Visible {
                all_day: true,
                ..before
            },
            Visible {
                rrule: Some("FREQ=WEEKLY"),
                ..before
            },
        ] {
            assert_eq!(visible_changes(&before, &after), only_when, "{after:?}");
        }
    }

    #[test]
    fn its_location_title_or_participants_change_each_their_own() {
        let a = [uid(1)];
        let b = [uid(1), uid(2)];
        let before = visible(&a);
        assert_eq!(
            visible_changes(
                &before,
                &Visible {
                    location: Some("Parc"),
                    ..before
                }
            ),
            AgendaChanges {
                location: true,
                ..AgendaChanges::default()
            }
        );
        assert_eq!(
            visible_changes(
                &before,
                &Visible {
                    title: "Déjeuner",
                    ..before
                }
            ),
            AgendaChanges {
                title: true,
                ..AgendaChanges::default()
            }
        );
        assert_eq!(
            visible_changes(&before, &visible(&b)),
            AgendaChanges {
                participants: true,
                ..AgendaChanges::default()
            }
        );
    }

    /// The edit form sends an empty location for « none », and the same
    /// assignees in any order: neither is a change.
    #[test]
    fn a_blank_location_and_reordered_assignees_are_no_change() {
        let a = [uid(1), uid(2)];
        let b = [uid(2), uid(1)];
        let before = Visible {
            location: None,
            ..visible(&a)
        };
        for location in [Some(""), Some("  ")] {
            assert!(visible_changes(
                &before,
                &Visible {
                    location,
                    ..visible(&b)
                }
            )
            .is_empty());
        }
    }

    #[test]
    fn several_changes_are_all_named() {
        let a = [uid(1)];
        let b = [uid(2)];
        let before = visible(&a);
        let after = Visible {
            title: "Déjeuner",
            location: None,
            all_day: true,
            ..visible(&b)
        };
        assert_eq!(
            visible_changes(&before, &after),
            AgendaChanges {
                when: true,
                location: true,
                title: true,
                participants: true,
            }
        );
    }
}
