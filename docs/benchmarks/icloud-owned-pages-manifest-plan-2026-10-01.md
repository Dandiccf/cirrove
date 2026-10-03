# Owned Pages manifest-only probe: plan, not a result

Question: can Cirrove's existing account-bound cookie session read the editor
manifest of the independently verified owned ec7 import, and does its declared
owner/zone/document match the retained uploaded receipt? This is not document
editing, a model download, or an SCMP session initialization.

The explicitly selected fixture is run
`ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc`, operation
`b534c269-8b9e-47e0-97a0-a512d0127765`, document
`8A909C8D-32D7-4E39-9202-DF1862E46C5A`. Its observed public route is
`https://www.icloud.com/pages/04d58BKwMzWS05eauV_IDr5BQ`.
No other account/document is selected by this probe.

## Grounding

Public unauthenticated source, not executed:
[Apple Pages main.js](https://www.icloud.com/applications/ix/15D120/editor/15D120/en-us/main.js),
7,205,041 bytes, SHA256
`59ad7e5a4b8115b34c2b1d623d2c0f4cb21bee7e1fcfdf3f7890dd9793fb01a0`.
Retained research source:
`/var/tmp/cirrove-public-pages-editor-g8h19rk8/main.js`.

`GSN_Manifest__getIwmbServiceUrl` at byte 5248670 builds the manifest URL;
`GSN_Manifest__sendRequest` at 5249701 sends the credentialed GET;
`GSN_Manifest__handleResponse` at 5251001 interprets HTTP 330 affinity and
HTTP 200 manifest. `GSAB openDocument` around 5655738 uses owned
`shareInfo.documentId`; CloudDocsDocumentInfo parses the docws/ndocws address
around 38649xx. Owned document access tokens are null (`getDocAccessToken`,
3864835). The observed inline static parseUrl assigns the unchanged Pages route
segment to documentId, and the observed inline BUILD_INFO buildNumber is
15D120. Neither was inferred from a private application global.

The initial server-rendered manifest producer remains unknown. This probe uses
the independently grounded **refresh** contract. The anonymously fetched page
was not saved; embedded manifest responses were not inspected.

## Preregistered arm

Parent must first validate the patch/tests and build the feature binary in its
unique target. Stop only the dedicated isolated public validation daemon before
running; the recovery journal owner lock refuses an active writer. Never restart
the installed daemon or an unrelated measurement.

```
<feature-binary>/cirrove-icloud-mounted-write-probe --public-native-manifest ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc
```

One invocation, no automatic retry. A new private verification directory and
manifest record precede account/network reads. It reuses the public verifier's
local source/semantic, account, isolated credential and durable receipt binding.
It checks exact imported metadata before and after the manifest request.

Network endpoints: two existing parent-folder metadata reads; initial GET
`https://iwmb.icloud.com/iwmb/pages/04d58BKwMzWS05eauV_IDr5BQ/manifest`
with `version=1540`, `clientType=G15.4`, `clientBuildNumber=15D120`.
At most three HTTP 330 affinity hops, only neutral iwmb or p1..p9999 iwmb hosts,
HTTPS port 443. Each response is bounded to 256 KiB; the manifest phase has a
60-second deadline and the command has a 120-second deadline. Existing generic
run-manifest helper records its conservative 900-second upper bound; the actual
command deadline is narrower. Ordinary redirects are refused. Session rejection
stops without sign-in. Cookies come only from the existing isolated Cirrove
session; no browser token or Apple authentication headers are copied.

Prediction: either exact owned CloudDocs identity is confirmed with service
schema booleans, or a static refusal identifies that editor access needs a
separate supported bootstrap. An authentication refusal is an informative limit,
not permission to invent a token exchange or broaden endpoint roles.

Only boolean identity/schema observations are persisted. Raw manifests, owner
IDs, documentPath values, cookies, sticky affinity, URLs and raw errors never
leave the adapter. documentPath is counted, not followed or interpreted. Service
URL role validation does not authorize model fetching. No zip/archive or
personal content is downloaded. No mutation, queue, allocation or retry occurs.

## Prepared validation and limits

No tests, builds, or account calls were executed while preparing this patch.
Parent should run `cargo test -p cirrove-icloud --features write-probe
owned_manifest` and the service feature test filtered by `public_manifest`,
with its unique target and serialized scheduling. Existing public verifier tests
must remain green after shared local preflight extraction.

Meaningful negative controls:
- Relax exact document/zone/owner comparisons: identity parser test must fail.
- Relax role_host suffix/role checks: affinity boundary test must fail.
- Follow ordinary redirects in the synthetic client: the HTTP 302 test must
  fail its exact `manifest response status refused` assertion. A subsequent
  transport timeout is not accepted as successful redirect refusal.
- Remove response byte bound: oversized HTTP response must be rejected by
  parser instead; to prove the specific bound also assert the static too-large
  category in the oversized case.
- Remove run fence: wrong-run service test must fail before any account call.

Synthetic HTTPS fixtures cover actual GET/330/200 flow, status/redirect refusal,
wrong role, oversize body and sanitized output. They do not demonstrate Apple
cookie suitability or actual response schema. A controlled live read is still
required. No same-ID document editing support follows from manifest success.

## Local validation

`owned-manifest-tests` passed all five adapter tests. Three meaningful negative
controls failed their intended tests: relaxing the document identity comparison,
enabling ordinary redirects in the synthetic client, and removing both response
size checks (`manifest-identity-negative`, `manifest-redirect-negative`,
`manifest-bound-negative`). The wrapper restored source after each arm.
`manifest-service-tests` passed all five public verification/binding tests,
including wrong-run refusal before any retained-state or network access.
No live manifest success is implied by these results.

## Initial live outcome: preflight metadata changed

`owned-manifest-live` stopped at the existing strict metadata comparison with
`public imported metadata changed or ambiguous`. The editor-manifest GET was
not reached. No mutation or automatic retry occurred. This is neither an editor
authentication result nor a manifest-schema result.

The verifier now reports separate static field categories for identity/name
ambiguity, kind, zone, parent, name, size and revision changes. It still requires
all original comparisons, and emits no metadata values. A second read-only arm
`owned-manifest-diagnostic` is preregistered with identical scope and bounds to
identify the refused condition after focused tests and a fresh build. If any
comparison fails, it must again stop before the manifest GET. No stale receipt
will be silently rebound to a current revision.

## Diagnostic outcome and current-read arm

`owned-manifest-diagnostic` refused specifically `public imported revision
changed`; all preceding identity/shape/size checks passed. The manifest endpoint
was not contacted. There is no evidence explaining the external revision change.

For the next `owned-manifest-current-read` arm, the manifest-only observer checks
all retained identity/shape/size predicates, requires a nonempty current ETag,
and compares postflight metadata to that exact preflight revision. It records
only whether the original import revision was unchanged. The original public
import verifier still requires the original receipt revision; no receipt is
modified or revalidated as current. This read-only observation grants no write
permission and makes no original-content equivalence claim. Endpoint/deadline,
account/document and sanitized-output bounds remain identical. Run focused tests,
a fresh binary and one bounded read; no automatic retry.

## Current-read outcome: editor session rejected

`manifest-current-tests` passed all six service tests, including unchanged strict
historical verification, current-revision read binding, foreign-identity refusal
and rejection of a postflight revision change. A fresh feature binary executed
`owned-manifest-current-read`: exact current metadata preflight succeeded, then
the editor request returned `manifest session rejected` (the static 401/403/421
category). No model fetch, editor initialization or mutation occurred. No success
observation was written. This demonstrates that this session/request did not
obtain a manifest; it does not identify which bootstrap or authentication step is
missing. Do not invent a token exchange or retry blindly. Public-source research
into the editor authentication contract is the next step.

## Grounded readiness follow-up

Static public editor source uses credentialed cookies for owned documents; its
owned-document access-token getter returns null. Its authentication controller
checks `pcsServiceIdentitiesIncluded` and calls setup `validate` for readiness.
This identifies a diagnostic path, not the cause of the observed rejection.
Detailed offsets are retained in
`/var/tmp/cirrove-public-pages-editor-g8h19rk8/authentication-bootstrap-findings.md`.

A fresh anonymous GET of https://www.icloud.com/ returned public HTML with
`data-cw-private-build-number` and `data-cw-private-mastering-number` both equal
to `2636Build34`. The inline bootstrap copies these root HTML attributes into
`__CW_BUILD_INFO` before removing them. The public external script is
`/system/icloud.com/2636Build34/en-us/main.js`. HTML SHA-256:
`89d0faa83f00a91b0ed30327047d4c117c3371fb299bc8da2d9efc6279a83bb2`.
Only extracted build constants and hash were retained, without browser state or
account responses. These CloudOS values are distinct from editor `15D120`.
A readiness probe is being prepared separately; none has run and no missing-PCS
or header-causation claim is established.

## Complete local check

`trash-vault-manifest-fullcheck` completed the full `scripts/check.sh` with exit 0
at 2026-10-01T12:44:26.369905+00:00: formatting, Clippy, workspace/feature tests,
actual FUSE groups, scripts, ledger and documentation. The existing broken
`Writeback::retry_stuck` rustdoc link remains a warning. Window scenarios and
installed validation are not covered. Independent review found no receipt rewrite
or permission expansion in the current-revision read-only observation path.
No native editing or successful editor authentication claim follows.
