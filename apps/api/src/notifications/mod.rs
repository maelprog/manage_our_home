//! How a member receives their reminders (#306): by notification on the
//! devices they subscribed (the default for a new account), by email, or
//! both. One choice per account, `users.reminder_channel`, read by the
//! reminder worker at send time — so switching to email covers the
//! reminders already set, not only the next ones.
//!
//! No fallback from one channel to the other (controller's decision of
//! 2026-10-01): a member who chose notifications and has no device
//! subscribed receives nothing, and is told so in the application instead
//! (`apps/web`: the warning on the reminder forms and on
//! `/account/notifications`).

pub mod center;
pub mod preferences;
pub mod push;
pub mod upcoming;

/// The value of `users.reminder_channel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderChannel {
    Push,
    Email,
    Both,
}

impl ReminderChannel {
    /// The column's value, as the `CHECK` of migration 0020 spells it.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "push" => Some(ReminderChannel::Push),
            "email" => Some(ReminderChannel::Email),
            "both" => Some(ReminderChannel::Both),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ReminderChannel::Push => "push",
            ReminderChannel::Email => "email",
            ReminderChannel::Both => "both",
        }
    }

    pub fn includes_push(self) -> bool {
        matches!(self, ReminderChannel::Push | ReminderChannel::Both)
    }

    pub fn includes_email(self) -> bool {
        matches!(self, ReminderChannel::Email | ReminderChannel::Both)
    }
}

/// The hour of the daily expiry recap a member may choose
/// (`users.expiry_digest_hour`, #700): 0 to 23, Europe/Paris, as the
/// `CHECK` of migration 0049 spells it. Anything else is refused.
pub fn parse_expiry_digest_hour(hour: i64) -> Option<i16> {
    i16::try_from(hour)
        .ok()
        .filter(|hour| (0..=23).contains(hour))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hour_of_the_day_is_a_recap_hour() {
        for hour in 0..=23 {
            assert_eq!(parse_expiry_digest_hour(hour), Some(hour as i16), "{hour}");
        }
    }

    #[test]
    fn an_hour_outside_the_day_is_refused() {
        for hour in [
            -1,
            24,
            25,
            100,
            -24,
            i64::from(i16::MAX) + 1,
            i64::MIN,
            i64::MAX,
        ] {
            assert_eq!(parse_expiry_digest_hour(hour), None, "{hour}");
        }
    }

    #[test]
    fn the_three_values_round_trip() {
        for (text, channel) in [
            ("push", ReminderChannel::Push),
            ("email", ReminderChannel::Email),
            ("both", ReminderChannel::Both),
        ] {
            assert_eq!(ReminderChannel::parse(text), Some(channel));
            assert_eq!(channel.as_str(), text);
        }
    }

    #[test]
    fn anything_else_is_not_a_channel() {
        for text in ["", "Push", "EMAIL", "sms", "push,email", " push"] {
            assert_eq!(ReminderChannel::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn each_channel_says_what_it_sends() {
        use ReminderChannel::*;
        assert_eq!((Push.includes_push(), Push.includes_email()), (true, false));
        assert_eq!(
            (Email.includes_push(), Email.includes_email()),
            (false, true)
        );
        assert_eq!((Both.includes_push(), Both.includes_email()), (true, true));
    }
}
