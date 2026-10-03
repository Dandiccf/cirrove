# One-shot owned-account renewal and readiness: preregistration

Executed once; rejected by Apple (see result below). This is an explicit authentication-session refresh
experiment. Existing manifest-only and readiness-only contracts remain unchanged:
neither automatically replays login.

## Evidence and question

Public Pages main.js SHA256
59ad7e5a4b8115b34c2b1d623d2c0f4cb21bee7e1fcfdf3f7890dd9793fb01a0
(https://www.icloud.com/applications/ix/15D120/editor/15D120/en-us/main.js)
implements token-authenticated accountLogin at byte6607098.
processAuthenticationFinish at6619910 invokes it with an authentication payload;
its terms-accepted branch at6623209 calls it again with the same payload at
6623357, without intervening SRP. This proves fresh SRP is not mandatory before
every accountLogin. It neither proves indefinite token validity nor authorizes
terms acceptance in this experiment. Normal periodic renewal uses validate;
this is deliberately a separate arm.

Question: can the authenticated token already retained from the exact isolated
ec7 login establish a stable DSID anchor and yield account-bound PCS readiness,
without requesting another password or modifying the saved session?

## One arm

After serialized tests/build, with no other measurement and only the isolated
public fixture daemon stopped (exclusive journal ownership is required):

```
<feature-binary>/cirrove-icloud-mounted-write-probe --public-native-renewal-readiness ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc
```

Exact account/run/operation/imported-document binding reuses the retained public
verifier. Current exact metadata is compared before and after. Before network,
a fresh private run manifest records the arm and binary. The restored session is
serialized only into SecretString memory, restored again into an independent
cookie jar, and checked equal to its source snapshot before authentication.
The read-only fork/client clone is not used because it shares cookie storage.

Exactly one existing account_login uses the original authenticated token at
fixed setup.icloud.com/setup/ws/1/accountLogin. No redirects, retries, SRP,
password, 2FA push, terms acceptance or automatic follow-up authentication.
Failure stops immediately. Successful response must supply a bounded stable
identity; if the original snapshot already had a trusted anchor, it must match.
Only then one existing bounded setup/validate readiness observation is allowed.
Neither manifest nor document/model endpoints follow. Readiness refusal stops
without a second login. AccountLogin uses its existing bounded JSON reader and
30-second client deadline; the compound arm is capped at60 seconds and the CLI
at120 seconds including metadata checks. Its generic run marker retains the
existing conservative900-second maximum, narrower command timeout applies.

The newly refreshed cookie/session/anchor is discarded after observation.
Nothing writes it to the original sealed file or keyring. No installed daemon,
service or normal connection changes. Only safe boolean results/static phases
and numeric HTTP status diagnostics are emitted, never raw provider response,
account address, cookie, token, URL or refreshed session.

A cloned jar isolates LOCAL changes only. Authentication can rotate server-side
session state; public source does not prove previous tokens/cookies remain valid.
The original saved bytes stay unchanged, but continued validity is not promised.
This is an authentication-state operation, not a cloud-files mutation or a
purely read-only session check.

Prediction: either one authenticated token exchange captures the stable anchor
and readiness yields booleans, or a static/numeric refusal closes the arm.
Even success does not prove Pages authentication or native same-ID editing.

## Prepared tests and negative controls

No builds/tests/live calls run by the preparing agent. Adapter filter
`owned_renewal` has four tests: independent jar/original serialized snapshot
unchanged despite response Set-Cookie; login rejection/missing or mismatched
anchor stops before validate; readiness rejection cannot retry login; wrong
account/cancellation refuse before network. Service filter
`public_renewal_readiness` checks the wrong-run guard.

Negative controls: share cookie jar (pointer/original snapshot assertions fail);
remove accountLogin (success arm cannot acquire legacy anchor); skip old-anchor
comparison (mismatch arm fails exact refusal); add retry after validate rejection
(request count/exact error fails). Existing readiness tests prove no login occurs
on ordinary readiness, and trusted-anchor tests prove validate cannot adopt one.

## Local validation

Independent review found no concrete isolation or request-boundary blocker.
`owned-renewal-tests` passed all four synthetic HTTPS/guard tests;
`renewal-service-tests` passed the exact wrong-run guard. Selected probe Clippy
passed with warnings denied. A separate strengthened reused-session test first
failed against old code (`signin-session-reset-reproduce`), then all five trusted
identity tests passed after clearing completed authentication before any new
sign-in (`signin-session-reset-fixed`). This prevents exporting an old account's
authenticated token under a new submitted-login hash after a failed sign-in.
The HTTP client/DNS policy remains unchanged, so the failure test stays local.
No live renewal result is implied by these tests.

## Live result

The sole `owned-renewal-live` arm finished at 2026-10-01T13:22:17.871750Z
with exit 1. Exact document metadata preflight completed in 1.481 seconds;
accountLogin then rejected the retained session with HTTP 421. No readiness
request followed, no new anchor was accepted, and no refreshed credentials were
persisted. The private manifest/log are retained in
`.local-state/icloud-access-owned-renewal-live-2026-10-01/`.

This closes this token-renewal attempt, not native Pages support. The next
authentication arm requires fresh interactive sign-in in the isolated account.
No automatic retry or import replay is justified by this result.
