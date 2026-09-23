# Cirrove OAuth website draft

This directory is the reviewable, static draft for Cirrove's Google OAuth
homepage, privacy-policy, removal and terms URLs. It is deliberately not deployed. Every page
contains a visible draft banner and `noindex`; the privacy page still contains
operator placeholders.

Preview it from the repository root:

```sh
python3 -m http.server 8000 --directory site
```

Then open <http://127.0.0.1:8000/>. Run the content and link check with:

```sh
python3 scripts/check-oauth-site.py
```

Before publication, the operator must:

1. choose a public domain they control and verify its DNS-level Domain property
   in Google Search Console with an owner of the Google Cloud project;
2. replace `[OPERATOR LEGAL NAME]`, `[CONTACT EMAIL]`, `[POSTAL ADDRESS IF
   REQUIRED]`, and `[EFFECTIVE DATE]` with approved public information;
3. complete a legal and policy review of the retention and removal wording;
4. add `site/CNAME` containing the verified custom domain, with no scheme or path;
5. remove the draft banners and `noindex` directives; and
6. run `python3 scripts/check-oauth-site.py --release`, which intentionally
   fails until those draft controls and placeholders are gone.

The homepage, privacy-policy and terms URLs entered in Google Auth Platform must
use that verified domain. Do not deploy this directory to the repository's
default GitHub Pages subdomain and claim that the domain is operator-controlled.
After the release check passes, the manually dispatched `OAuth site` workflow
deploys only this directory to GitHub Pages. It never publishes on an ordinary
push, so an internal draft cannot become public merely by merging it.
