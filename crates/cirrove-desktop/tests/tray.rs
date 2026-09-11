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
use std::collections::{BTreeSet, HashMap};

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
async fn the_item_points_at_a_menu_a_shell_can_actually_read() {
    // The item only names an object path. A path with nothing serving
    // com.canonical.dbusmenu behind it is an empty right-click, which reads as a
    // broken tray rather than one without a menu -- so the two are checked
    // together rather than the property alone.
    let connection = tray::publish_for_test()
        .await
        .expect("could not publish the item on the session bus");
    let unique = connection.unique_name().expect("no unique name").clone();
    let item = zbus::Proxy::new(
        &connection,
        unique.clone(),
        "/StatusNotifierItem",
        "org.kde.StatusNotifierItem",
    )
    .await
    .expect("no item proxy");
    let path: zbus::zvariant::OwnedObjectPath =
        item.get_property("Menu").await.expect("no Menu property");

    let menu = zbus::Proxy::new(&connection, unique, path.as_str(), "com.canonical.dbusmenu")
        .await
        .expect("nothing serves a menu at the path the item advertises");
    let version: u32 = menu.get_property("Version").await.expect("no Version");
    assert!(
        version >= 2,
        "dbusmenu version {version} is too old to render"
    );
}

#[tokio::test]
#[ignore = "needs a session bus; CI provides a private one"]
async fn the_menu_offers_settings_and_quit_beside_the_accounts() {
    // Milestone 5: "no control or state may be reachable only through a tray".
    // These two are the shortcuts, and a menu that lost them would leave the
    // tray with one gesture again.
    let connection = tray::publish_for_test()
        .await
        .expect("could not publish the item on the session bus");
    let menu = zbus::Proxy::new(
        &connection,
        connection.unique_name().expect("no unique name").clone(),
        "/MenuBar",
        "com.canonical.dbusmenu",
    )
    .await
    .expect("no menu proxy");

    type Layout = (
        i32,
        HashMap<String, zbus::zvariant::OwnedValue>,
        Vec<zbus::zvariant::OwnedValue>,
    );
    let (_revision, root): (u32, Layout) = menu
        .call("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
        .await
        .expect("GetLayout failed");

    let labels: Vec<String> = root
        .2
        .iter()
        .filter_map(|child| child.try_clone().ok())
        .filter_map(|child| {
            let structure = zbus::zvariant::Structure::try_from(child).ok()?;
            let properties = HashMap::<String, zbus::zvariant::OwnedValue>::try_from(
                structure.fields().get(1)?.try_clone().ok()?,
            )
            .ok()?;
            let label = properties.get("label")?.try_clone().ok()?;
            String::try_from(label).ok()
        })
        .collect();

    assert!(
        labels.iter().any(|l| l.contains("settings")),
        "no settings entry: {labels:?}"
    );
    // The account row is a submenu now, and what is in it is the point: opening
    // the folder and changing the mount. A flat row would read as an account
    // with nothing you can do to it.
    let account_children: Vec<String> = root
        .2
        .iter()
        .filter_map(|child| {
            let structure = zbus::zvariant::Structure::try_from(child.try_clone().ok()?).ok()?;
            let nested = Vec::<zbus::zvariant::OwnedValue>::try_from(
                structure.fields().get(2)?.try_clone().ok()?,
            )
            .ok()?;
            Some(nested)
        })
        .flatten()
        .filter_map(|child| {
            let structure = zbus::zvariant::Structure::try_from(child).ok()?;
            let properties = HashMap::<String, zbus::zvariant::OwnedValue>::try_from(
                structure.fields().get(1)?.try_clone().ok()?,
            )
            .ok()?;
            String::try_from(properties.get("label")?.try_clone().ok()?).ok()
        })
        .collect();
    assert!(
        account_children.iter().any(|l| l == "Open folder"),
        "no open entry under the account: {account_children:?}"
    );
    // publish_for_test seeds a mounted account, so the offer must be to unmount.
    assert!(
        account_children.iter().any(|l| l == "Unmount"),
        "a mounted account must offer to unmount: {account_children:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("Quit")),
        "no quit entry: {labels:?}"
    );
    // publish_for_test seeds an account needing sign-in; its row must be there,
    // because the account list is the reason to open the menu at all.
    assert!(
        labels.iter().any(|l| l.contains("test")),
        "no account row: {labels:?}"
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
