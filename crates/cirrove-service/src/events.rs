//! What the daemon tells local desktop clients when something changes.
//!
//! See [ADR 0007](../../../docs/adr/0007-desktop-event-channel.md). A tray icon,
//! a file-manager badge and a notification all need an edge -- the moment a
//! thing changed -- and the control socket could only answer levels, one
//! request per connection. Reconstructing edges by polling means a missed
//! sample is a missed notification, so this carries the change itself.
//!
//! Events are coalesced levels, not a log. A client that stalls and resumes is
//! told it lagged and is re-primed with current state, rather than being handed
//! a backlog it would only discard. That is the same reasoning ADR 0003 applies
//! to the provider feed, where a generation counter coalesces wakes without
//! losing the last one.

use crate::manager::AccountStatus;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped when the shape of an [`Event`] changes in a way an existing client
/// would misread. Reported through the `capabilities` verb, never through
/// `STATUS_PROTOCOL_VERSION`: the desktop compares that one for equality, so
/// moving it makes every mismatched pair refuse each other over a feature the
/// older side never asked for.
pub const EVENT_PROTOCOL_VERSION: u32 = 1;

/// How many events a subscriber may fall behind before it is told it lagged.
///
/// Small on purpose. The queue exists to absorb a client that is briefly busy
/// painting, not to buffer history: everything here is a level, so the cure for
/// falling behind is current state and not the states that were missed. A
/// larger queue would only delay that cure.
pub const EVENT_QUEUE_DEPTH: usize = 64;

/// One change worth telling a desktop client about.
///
/// `account_id` rather than a path or a label is the identity, because that is
/// what the desktop already uses for actions and it is stable across a rename.
/// `label` rides along so a tray can render without a second lookup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Connection state changed. `sign_in_required` is the one a tray must
    /// surface: it is the only state a user can act on and the only one that
    /// stays until they do.
    Account {
        account_id: String,
        label: String,
        state: String,
        enabled: bool,
        mounted: bool,
    },
    /// A mount appeared or went away. Separate from `Account` because a file
    /// manager cares about exactly this and nothing else about the account, and
    /// because the mount path is what it needs to decide whether a directory it
    /// is showing belongs to Cirrove at all.
    Mount {
        account_id: String,
        label: String,
        mount_path: PathBuf,
        mounted: bool,
    },
    /// An account disappeared from settings. A tray that keeps a row per
    /// account would otherwise keep a stale one forever.
    AccountRemoved { account_id: String, label: String },
    /// Priming is complete and current state has been delivered in full.
    ///
    /// Always sent, including when there was nothing to prime with. That is the
    /// point of it: a daemon with no accounts sends no priming events, and
    /// without a marker a client cannot tell an established subscription from a
    /// service that is not answering. Running the tray against an account-less
    /// daemon is exactly how that was found -- it reported "the service did not
    /// answer in time" against a daemon that was answering perfectly.
    ///
    /// A consumer that would rather render once than once per account can wait
    /// for this instead of drawing each priming event.
    Ready,
    /// The subscriber fell behind and intermediate states were dropped. The
    /// events that follow re-prime it with current state.
    ///
    /// Sent rather than hidden because a client drawing progress needs to know
    /// it saw a jump: silently coalescing a sequence into its endpoint is
    /// correct for a level and wrong for anything that was animating.
    Lagged { dropped: u64 },
}

impl Event {
    /// The account this concerns, for a client that keys its own state by it.
    pub fn account_id(&self) -> Option<&str> {
        match self {
            Self::Account { account_id, .. }
            | Self::Mount { account_id, .. }
            | Self::AccountRemoved { account_id, .. } => Some(account_id),
            Self::Lagged { .. } | Self::Ready => None,
        }
    }
}

/// Identity for diffing. `account_id` is empty on responses from a daemon that
/// predates it, and the label is what the rest of the control surface falls back
/// to, so this falls back the same way rather than inventing a third rule.
fn identity(status: &AccountStatus) -> &str {
    if status.account_id.is_empty() {
        &status.label
    } else {
        &status.account_id
    }
}

/// Current state as the events that would have produced it.
///
/// A subscription opens with these, so a client never needs a `status` call to
/// prime itself and there is no window between subscribing and knowing the
/// state. It is also what re-primes a subscriber that lagged.
pub fn prime(current: &[AccountStatus]) -> Vec<Event> {
    let mut events = Vec::with_capacity(current.len() * 2);
    for status in current {
        events.push(account_event(status));
        events.push(mount_event(status));
    }
    events
}

/// The events that describe going from `previous` to `current`.
///
/// Returns nothing when nothing a client can see has changed, which is the
/// common case: the manager rewrites its status vector every five seconds
/// whether or not anything moved, and waking every tray on a timer is the
/// behaviour this channel exists to replace.
pub fn diff(previous: &[AccountStatus], current: &[AccountStatus]) -> Vec<Event> {
    let mut events = Vec::new();
    for status in current {
        let before = previous.iter().find(|p| identity(p) == identity(status));
        match before {
            None => {
                events.push(account_event(status));
                events.push(mount_event(status));
            }
            Some(before) => {
                if before.state != status.state
                    || before.enabled != status.enabled
                    || before.mounted != status.mounted
                {
                    events.push(account_event(status));
                }
                if before.mounted != status.mounted || before.mount_path != status.mount_path {
                    events.push(mount_event(status));
                }
            }
        }
    }
    for before in previous {
        if !current.iter().any(|c| identity(c) == identity(before)) {
            events.push(Event::AccountRemoved {
                account_id: before.account_id.clone(),
                label: before.label.clone(),
            });
        }
    }
    events
}

fn account_event(status: &AccountStatus) -> Event {
    Event::Account {
        account_id: status.account_id.clone(),
        label: status.label.clone(),
        state: status.state.clone(),
        enabled: status.enabled,
        mounted: status.mounted,
    }
}

fn mount_event(status: &AccountStatus) -> Event {
    Event::Mount {
        account_id: status.account_id.clone(),
        label: status.label.clone(),
        mount_path: status.mount_path.clone(),
        mounted: status.mounted,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn status(label: &str, state: &str, mounted: bool) -> AccountStatus {
        AccountStatus {
            account_id: format!("id-{label}"),
            label: label.into(),
            state: state.into(),
            mounted,
            enabled: true,
            mount_path: PathBuf::from(format!("/mnt/{label}")),
            ..Default::default()
        }
    }

    #[test]
    fn an_unchanged_status_produces_no_events() {
        let before = vec![status("work", "ready", true)];
        let after = before.clone();
        assert!(diff(&before, &after).is_empty());
    }

    #[test]
    fn losing_consent_is_reported_as_an_account_change() {
        let before = vec![status("work", "ready", true)];
        let after = vec![status("work", "sign_in_required", true)];
        let events = diff(&before, &after);
        assert_eq!(
            events,
            vec![Event::Account {
                account_id: "id-work".into(),
                label: "work".into(),
                state: "sign_in_required".into(),
                enabled: true,
                mounted: true,
            }]
        );
    }

    #[test]
    fn an_unmount_reports_both_the_account_and_the_mount() {
        let before = vec![status("work", "ready", true)];
        let after = vec![status("work", "ready", false)];
        let events = diff(&before, &after);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0], Event::Account { mounted: false, .. }));
        assert!(matches!(events[1], Event::Mount { mounted: false, .. }));
    }

    #[test]
    fn a_removed_account_is_reported_so_a_tray_can_drop_its_row() {
        let before = vec![status("work", "ready", true), status("home", "ready", true)];
        let after = vec![status("work", "ready", true)];
        assert_eq!(
            diff(&before, &after),
            vec![Event::AccountRemoved {
                account_id: "id-home".into(),
                label: "home".into(),
            }]
        );
    }

    #[test]
    fn a_new_account_primes_the_client_with_both_of_its_events() {
        let events = diff(&[], &[status("work", "starting", false)]);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0], Event::Account { .. }));
        assert!(matches!(events[1], Event::Mount { .. }));
    }

    #[test]
    fn priming_describes_every_account_twice_and_nothing_else() {
        let events = prime(&[
            status("work", "ready", true),
            status("home", "ready", false),
        ]);
        assert_eq!(events.len(), 4, "{events:?}");
        assert_eq!(events[0].account_id(), Some("id-work"));
        assert_eq!(events[2].account_id(), Some("id-home"));
    }

    #[test]
    fn an_account_without_an_id_still_diffs_by_label() {
        // An older settings file has no account_id. Diffing by an empty string
        // would make every such account look like the same one, so two accounts
        // would collapse into one row and a change to either would be reported
        // against whichever came first.
        let mut before = status("work", "ready", true);
        let mut after = status("work", "sign_in_required", true);
        before.account_id = String::new();
        after.account_id = String::new();
        let other = {
            let mut other = status("home", "ready", true);
            other.account_id = String::new();
            other
        };
        let events = diff(&[before, other.clone()], &[after, other]);
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(matches!(&events[0], Event::Account { label, .. } if label == "work"));
    }
}
