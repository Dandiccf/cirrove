#!/usr/bin/env bash
# Compile the translation catalogues, and refresh the template from the source.
#
# The template is regenerated rather than edited: a string that is in the
# program and not in po/cirrove.pot is a string nobody can translate, and the
# only way to be sure is to ask the source. xgettext has no Rust mode, so it
# reads them as C -- which mistakes a lifetime for a character constant and
# says so, harmlessly.
#
# Usage: scripts/build-translations.sh [output-dir]   default: target/locale/
set -euo pipefail

repo=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
out=${1:-$repo/target/locale}

# `n` is the extraction marker for strings that live in static tables and are
# translated where they are shown; see crates/cirrove-desktop/src/i18n.rs.
xgettext --language=C --from-code=UTF-8 \
  --keyword=gettext --keyword=n \
  --add-comments=TRANSLATORS --package-name=cirrove \
  --copyright-holder="Cirrove contributors" \
  --msgid-bugs-address="https://github.com/Dandiccf/cirrove/issues" \
  -o "$repo/po/cirrove.pot" \
  $(find "$repo/crates/cirrove-desktop/src" -name '*.rs' | sort) 2>/dev/null

# The Files extension is Python and was left out of this entirely, so for as
# long as it existed none of its strings could be translated: the owner looked
# past an English "Keep offline" in a German menu three times on 2026-09-16
# before finding it. Joined rather than regenerated, because xgettext takes one
# language per run.
xgettext --language=Python --from-code=UTF-8 --join-existing \
  --keyword=_ \
  --add-comments=TRANSLATORS --package-name=cirrove \
  -o "$repo/po/cirrove.pot" \
  "$repo/packaging/nautilus/cirrove.py"

# Dolphin is a KF6/C++ plugin and uses KI18n's i18n() with the same gettext
# domain. Join it into the one catalogue so the two file managers use the same
# words for the same operation.
xgettext --language=C++ --from-code=UTF-8 --join-existing \
  --keyword=i18n \
  --add-comments=TRANSLATORS --package-name=cirrove \
  -o "$repo/po/cirrove.pot" \
  "$repo/packaging/dolphin/actionplugin.cpp"

for po in "$repo"/po/*.po; do
  lang=$(basename "$po" .po)
  # Merge first, so a string added since the last translation shows up as
  # untranslated rather than silently missing.
  msgmerge --quiet --update --backup=none "$po" "$repo/po/cirrove.pot"
  mkdir -p "$out/$lang/LC_MESSAGES"
  msgfmt --check --output-file "$out/$lang/LC_MESSAGES/cirrove.mo" "$po"
  printf '  %-6s %s\n' "$lang" "$(msgfmt --statistics -o /dev/null "$po" 2>&1)"
done
echo "catalogues in $out"
