//! Translation, bound once at start.
//!
//! Every user-visible string in the window and the tray goes through
//! [`gettext`], which returns the string unchanged when no catalogue matches --
//! so an untranslated build, a locale with no catalogue, and the test harness
//! all see the English the source contains. That property is what makes this
//! safe to introduce across a whole program at once: nothing can regress into
//! a blank label.
//!
//! Formatted sentences cannot use `format!`, which needs a literal and would
//! defeat extraction. They go through [`fill`] instead, which substitutes into
//! the translated template at runtime, so a translator may reorder the
//! placeholders.

use std::path::{Path, PathBuf};

pub use gettextrs::gettext;

/// The catalogue name: `<locale dir>/<lang>/LC_MESSAGES/cirrove.mo`.
pub const DOMAIN: &str = "cirrove";

/// Where to look for catalogues, in the order they are tried.
///
/// A packaged build finds them under `/usr/share/locale`; the developer
/// install puts everything under `~/.local`, so deriving the directory from
/// the running binary covers both without either having to be configured:
/// `/usr/bin/cirrove-desktop` and `~/.local/bin/cirrove-desktop` both sit one
/// level below a `share/locale`. `CIRROVE_LOCALE_DIR` overrides for a test or
/// a build that has not been installed at all.
fn locale_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CIRROVE_LOCALE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let beside_the_binary = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .and_then(|bin| bin.parent().map(|prefix| prefix.join("share/locale")));
    beside_the_binary
        .filter(|dir| dir.is_dir())
        .or_else(|| Some(PathBuf::from("/usr/share/locale")).filter(|dir| dir.is_dir()))
}

/// Bind the catalogue. Safe to call more than once, and safe to fail: without
/// a catalogue every string is the English in the source.
///
/// Call after `gtk::init` (or after the first `adw::Application` startup),
/// which is what sets the locale from the environment. gettext-rs marks
/// `setlocale` unsafe, the workspace forbids `unsafe_code`, and GTK has
/// already done it by then -- so this binds the catalogue and nothing else.
pub fn init() {
    if let Some(dir) = locale_dir() {
        let _ = gettextrs::bindtextdomain(DOMAIN, dir);
    }
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}

/// Mark a string for extraction without translating it here.
///
/// The standard `N_()` idiom. Some user-visible text lives in static tables --
/// a `description()` that returns `&'static str` for each state -- where
/// translating at the point of definition would mean returning `String` and
/// rippling through every caller and test. The English stays the message id,
/// this makes xgettext find it, and the render site calls `gettext` on it.
pub const fn n(text: &'static str) -> &'static str {
    text
}

/// Substitute into a translated template: `{}` in order, or `{0}` and `{1}` in
/// any order, because a translation may need them the other way round.
///
/// `format!` cannot be used on a translated string -- it needs a literal at
/// compile time, and a literal is exactly what the translator is replacing.
pub fn fill(template: &str, arguments: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut next = 0usize;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|i| open + i) else {
            break;
        };
        let slot = &rest[open + 1..close];
        let index = if slot.is_empty() {
            let i = next;
            next += 1;
            Some(i)
        } else {
            slot.parse::<usize>().ok()
        };
        match index.and_then(|i| arguments.get(i)) {
            Some(value) => {
                out.push_str(&rest[..open]);
                out.push_str(value);
            }
            // Not a placeholder we know: leave it exactly as the translator
            // wrote it rather than silently dropping their text.
            None => out.push_str(&rest[..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn placeholders_are_filled_in_order() {
        assert_eq!(fill("{} of {}", &["66 KB", "5 GB"]), "66 KB of 5 GB");
    }

    #[test]
    fn a_translation_may_reorder_them() {
        assert_eq!(
            fill("{1}, davon {0}", &["66 KB", "5 GB"]),
            "5 GB, davon 66 KB"
        );
    }

    #[test]
    fn text_that_is_not_a_placeholder_survives_untouched() {
        // A translator writing braces for their own reasons must not lose them.
        assert_eq!(fill("nothing {here} at all", &[]), "nothing {here} at all");
        assert_eq!(fill("{} and {9}", &["one"]), "one and {9}");
    }

    #[test]
    fn a_template_without_placeholders_is_returned_as_it_is() {
        assert_eq!(fill("Your clouds, in Files", &[]), "Your clouds, in Files");
    }
}
