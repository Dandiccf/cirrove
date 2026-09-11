//! The tray item must offer a shell the whole property set, not most of it.
//!
//! The first version of this tray registered with the watcher, answered every
//! property it declared, reported `NeedsAttention`, and drew nothing at all. It
//! was missing `AttentionIconName`, and the spec switches icons with the status:
//! a host in the attention state does not read `IconName`. Passive hid the item
//! and NeedsAttention had nothing to draw, so both states the daemon could
//! produce were invisible, and every unit test passed throughout. Only a person
//! looking at their own panel found it.
//!
//! What is assertable without a panel is that the item offers what a host reads.
//! That is this file.

use cirrove_desktop::tray;
use std::collections::BTreeSet;

/// Everything a StatusNotifierItem host may read. Taken from the properties a
/// known-good tray on this desktop exposes, which is a stricter and more useful
/// bar than the spec's prose: it is what implementations actually ask for.
const REQUIRED: &[&str] = &[
    "AttentionIconName",
    "AttentionIconPixmap",
    "AttentionMovieName",
    "Category",
    "IconName",
    "IconPixmap",
    "Id",
    "ItemIsMenu",
    "Menu",
    "OverlayIconName",
    "OverlayIconPixmap",
    "Status",
    "Title",
    "ToolTip",
    "WindowId",
];

#[tokio::test]
#[ignore = "needs a session bus; CI provides a private one"]
async fn the_item_offers_a_host_every_property_it_may_read() {
    let connection = tray::publish_for_test()
        .await
        .expect("could not publish the item on the session bus");
    let properties = zbus::Proxy::new(
        &connection,
        connection.unique_name().expect("no unique name").clone(),
        "/StatusNotifierItem",
        "org.freedesktop.DBus.Properties",
    )
    .await
    .expect("no properties proxy");

    let all: std::collections::HashMap<String, zbus::zvariant::OwnedValue> = properties
        .call("GetAll", &("org.kde.StatusNotifierItem"))
        .await
        .expect("GetAll failed");

    let offered: BTreeSet<&str> = all.keys().map(String::as_str).collect();
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|name| !offered.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "a host would find these missing and may draw nothing: {missing:?}. Offered: {offered:?}"
    );
}

#[tokio::test]
#[ignore = "needs a session bus; CI provides a private one"]
async fn the_attention_icon_is_answered_and_not_empty() {
    // The specific failure: NeedsAttention with no attention icon draws nothing.
    // An empty string here is as invisible as an absent property.
    let connection = tray::publish_for_test()
        .await
        .expect("could not publish the item on the session bus");
    let item = zbus::Proxy::new(
        &connection,
        connection.unique_name().expect("no unique name").clone(),
        "/StatusNotifierItem",
        "org.kde.StatusNotifierItem",
    )
    .await
    .expect("no item proxy");

    for property in ["IconName", "AttentionIconName"] {
        let value: String = item
            .get_property(property)
            .await
            .unwrap_or_else(|error| panic!("{property} could not be read: {error}"));
        assert!(
            !value.is_empty(),
            "{property} is empty, so nothing is drawn"
        );
    }
}
