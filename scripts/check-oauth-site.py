#!/usr/bin/env python3
"""Validate the internal Google OAuth website draft without network access."""

from __future__ import annotations

import argparse
from html.parser import HTMLParser
from pathlib import Path
import re
import sys
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site"
PAGES = ("index.html", "privacy.html", "remove-data.html", "terms.html")
PLACEHOLDERS = (
    "[OPERATOR LEGAL NAME]",
    "[CONTACT EMAIL]",
    "[POSTAL ADDRESS IF REQUIRED]",
    "[EFFECTIVE DATE]",
)


class PageParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.links: list[str] = []
        self.assets: list[str] = []
        self.lang: str | None = None
        self.title_parts: list[str] = []
        self.text_parts: list[str] = []
        self._in_title = False
        self.viewport = False
        self.robots: str | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "html":
            self.lang = values.get("lang")
        elif tag == "a" and values.get("href"):
            self.links.append(values["href"] or "")
        elif tag in ("img", "script") and values.get("src"):
            self.assets.append(values["src"] or "")
        elif tag == "link" and values.get("href"):
            self.assets.append(values["href"] or "")
        elif tag == "meta" and values.get("name") == "viewport":
            self.viewport = True
        elif tag == "meta" and values.get("name") == "robots":
            self.robots = values.get("content")
        elif tag == "title":
            self._in_title = True

    def handle_endtag(self, tag: str) -> None:
        if tag == "title":
            self._in_title = False

    def handle_data(self, data: str) -> None:
        self.text_parts.append(data)
        if self._in_title:
            self.title_parts.append(data)


def local_target(page: Path, value: str) -> Path | None:
    parsed = urlparse(value)
    if parsed.scheme or parsed.netloc or value.startswith(("#", "mailto:")):
        return None
    return (page.parent / parsed.path).resolve()


def check_page(page: Path, release: bool) -> list[str]:
    errors: list[str] = []
    source = page.read_text(encoding="utf-8")
    parser = PageParser()
    parser.feed(source)

    if parser.lang != "en":
        errors.append(f"{page.name}: html lang must be en")
    if not parser.viewport:
        errors.append(f"{page.name}: viewport metadata is missing")
    if not "".join(parser.title_parts).strip():
        errors.append(f"{page.name}: title is missing")

    for value in parser.links + parser.assets:
        target = local_target(page, value)
        if target is not None and not target.exists():
            errors.append(f"{page.name}: local target does not exist: {value}")

    if release:
        if "draft-banner" in source or "Internal review draft" in source:
            errors.append(f"{page.name}: draft banner remains")
        if parser.robots and "noindex" in parser.robots.lower():
            errors.append(f"{page.name}: noindex remains")
        for placeholder in PLACEHOLDERS:
            if placeholder in source:
                errors.append(f"{page.name}: unresolved placeholder {placeholder}")
    else:
        if "draft-banner" not in source:
            errors.append(f"{page.name}: internal draft banner is missing")
        if not parser.robots or "noindex" not in parser.robots.lower():
            errors.append(f"{page.name}: internal draft must remain noindex")

    return errors


def visible_text(source: str) -> str:
    parser = PageParser()
    parser.feed(source)
    return " ".join(" ".join(parser.text_parts).split())


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--release",
        action="store_true",
        help="reject draft controls and unresolved operator placeholders",
    )
    args = parser.parse_args()

    errors: list[str] = []
    page_sources: dict[str, str] = {}
    for name in PAGES:
        page = SITE / name
        if not page.is_file():
            errors.append(f"missing page: site/{name}")
            continue
        page_sources[name] = page.read_text(encoding="utf-8")
        errors.extend(check_page(page, args.release))

    required_index_links = (
        'href="privacy.html"',
        'href="remove-data.html"',
        'href="terms.html"',
    )
    for link in required_index_links:
        if link not in page_sources.get("index.html", ""):
            errors.append(f"index.html: required link is missing: {link}")

    privacy = visible_text(page_sources.get("privacy.html", ""))
    for phrase in (
        "Google data Cirrove accesses",
        "How Cirrove uses Google data",
        "Local storage and retention",
        "Sharing and transfers",
        "Limited Use requirements",
        "desktop keyring",
        "does not operate a cloud service",
    ):
        if phrase not in privacy:
            errors.append(f"privacy.html: required disclosure is missing: {phrase}")

    removal = visible_text(page_sources.get("remove-data.html", ""))
    for phrase in (
        "cirrove forget",
        "cirrove local-data --discard-removed",
        "desktop's password and keyring manager",
        "Google Account's third-party connections",
        "does not delete files from Google Drive",
    ):
        if phrase not in removal:
            errors.append(f"remove-data.html: required removal step is missing: {phrase}")

    terms = visible_text(page_sources.get("terms.html", ""))
    for phrase in (
        "Terms of use",
        "Allow changes",
        "pre-release software",
        "Google's terms",
        "privacy policy",
        "data-removal guide",
    ):
        if phrase not in terms:
            errors.append(f"terms.html: required term is missing: {phrase}")

    if args.release:
        cname = SITE / "CNAME"
        if not cname.is_file():
            errors.append("site/CNAME: operator-controlled custom domain is missing")
        else:
            host = cname.read_text(encoding="utf-8").strip()
            if not re.fullmatch(
                r"(?=.{1,253}$)(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,63}",
                host,
            ):
                errors.append("site/CNAME: custom domain is invalid")
            elif host.lower().endswith("github.io"):
                errors.append("site/CNAME: default GitHub Pages domains do not prove operator control")

    if errors:
        print("OAuth site check failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    mode = "release" if args.release else "internal draft"
    print(f"OAuth site check passed ({mode}; {len(PAGES)} pages)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
