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
