# Owned Pages authentication readiness: one bounded observation

Prepared, not executed. This observes the setup session readiness of the exact
ec7 validation account. It does not diagnose the editor rejection by inference,
repeat accountLogin, ask for credentials, initialize SCMP or fetch model content.

## Grounded request

The retained public Pages main.js SHA256
59ad7e5a4b8115b34c2b1d623d2c0f4cb21bee7e1fcfdf3f7890dd9793fb01a0
contains fetchAuthenticationDetails at byte6606282: bodyless credentialed POST
`https://setup.icloud.com/setup/ws/1/validate`. addRequestIdQueryArgs at6608797
adds a fresh requestId. The CloudOS wrapper at6304091 adds clientId,
clientBuildNumber, clientMasteringNumber and DSID only when already known.
Source: https://www.icloud.com/applications/ix/15D120/editor/15D120/en-us/main.js

Anonymous https://www.icloud.com/ HTML, SHA256
89d0faa83f00a91b0ed30327047d4c117c3371fb299bc8da2d9efc6279a83bb2,
exposes static build/mastering attributes both2636Build34; the inline bootstrap
assigns these to __CW_BUILD_INFO. Parent retained only safe build fields/hash in
/var/tmp/cirrove-public-pages-editor-g8h19rk8/public-cloudos-build.json.
The probe uses these values, fresh UUID client/request IDs and no DSID query
because the native snapshot does not retain one. It uses fixed www.icloud.com
Origin/Referer and existing isolated session cookies, no browser credentials.

## Arm and prediction

After tests/build and no concurrent measurement, with only the isolated public
validation daemon stopped (journal lock enforces no writer):

```
<feature-binary>/cirrove-icloud-mounted-write-probe --public-native-readiness ec7f82e1-3c33-4a1f-bf01-d6e3f2ca80cc
```

The command keeps original receipt/account/operation binding and brackets the
readiness request with current exact-document metadata snapshots. It creates a
private run manifest before requests. One setup POST, no redirect or retry;
256KiB response bound, 30-second readiness deadline, 120-second command deadline.
No returned service URL is followed. The generic run manifest has its existing
conservative 900-second upper bound; the command deadline is narrower.

Prediction: a200 response binds dsInfo.appleId to the isolated session account
hash and supplies optional readiness booleans, or a numeric status/schema/identity
refusal stops the arm. Apple-ID aliases are not silently accepted; an alternate
address therefore requires separate identity-contract investigation. A false
PCS-readiness flag is observation, not proof that it caused the editor failure.
Absent/null flags are omitted, never presented as known false. Type-mismatched
flags are refused. No default readiness is inferred from HTTP200.

Persisted observation values are booleans only: account match, PCS identities,
web access, ICDRS disabled, PCS device consent, HSA challenge and per-service PCS
required when supplied. No raw response, account value, cookie, URL, token or
provider body is printed or persisted. The local run marker includes its known
run UUID and fixed nonmutation facts. Session cookie updates remain in memory;
this probe does not persist a refreshed sign-in or call manifest automatically.

## Prepared tests, not results

Adapter filter `owned_readiness`: identity/schema/boolean output; fixed query;
wrong local account and cancellation before network; actual synthetic HTTPS POST;
exact rejection of302/421/oversize/wrong account with one request only.
Service filter `public_readiness`: wrong run refused before retained state.
Parent must serialize builds/tests in its own target.

Negative controls: remove response account comparison (identity tests must fail);
remove local account binding (wrong-local-account test must fail); enable fixture
redirects (exact302 category must fail); remove byte limit (oversize category must
fail); convert omitted optional fields to false (absence assertion must fail).
No tests, builds or live requests were run during preparation.

## Parent local validation

`owned-readiness-tests` and restored-source `readiness-restored-tests` each passed
all five adapter tests. `readiness-service-tests` passed the wrong-run guard.
Clippy for the selected feature probe passed with warnings denied.
Independent review found no production safety blocker and confirmed one test
weakness: invalid oversized JSON could still fail parsing after guard removal.
The final fixture is valid matching-account JSON with an oversized ignored field.

`readiness-account-bypass-negative` failed when the response-account comparison
was bypassed; `readiness-redirect-negative` failed after enabling automatic
redirects; `readiness-bound-negative` failed after removing both shared response
size guards. The earlier inverted-comparison arm also failed but is not the
stronger bypass proof. All production source was restored; the corrected five
adapter tests passed afterward. No live readiness result is inferred.

## Live outcome: identity mismatch, readiness unconfirmed

`owned-readiness-live` ended with exit 1 at 2026-10-01T12:50:22.311626+00:00.
Preflight exact current-document metadata succeeded. Setup returned HTTP 200,
but `dsInfo.appleId` did not hash to the saved login address; the adapter refused
with `readiness account identity mismatch` before returning any readiness flags.
No raw account value or response was retained or printed, no cookie refresh was
persisted, and no manifest/model request or login replay followed.

An Apple login alias is a hypothesis, not an established identity match. Do not
accept an arbitrary returned account or silently rewrite the saved account hash.
Next investigate explicit public alias/identity contracts or an already trusted
stable account anchor. This arm establishes neither PCS readiness nor a cause
for the editor-session rejection.

## Trusted identity preparation

Public authentication schema exposes stable `dsInfo.dsid` as a string, while
`appleIdEntries` contains type/primary descriptors rather than an established
address-alias list. Cirrove now captures an optional domain-separated DSID hash
only after successful `accountLogin` and valid Drive/docs endpoints. It persists
only that hash in the encrypted snapshot; the submitted-login hash remains a
separate required binding. Legacy snapshots restore without a stable anchor.
Readiness uses an existing trusted anchor when present and refuses mismatches
without falling back to address matching. Validation cannot create or update it.
Starting another sign-in clears the prior anchor before network IO.

`trusted-identity-tests` passed four tests; the extended
`trusted-identity-lifecycle` passed five, including a failed new sign-in whose
network stays on loopback. Removing the reset caused that test to fail in
`trusted-signin-reset-negative`; source was restored. Actual synthetic HTTPS
covers accountLogin capture, canonical-address validation with the same DSID,
wrong-DSID refusal even when the address matches, legacy no-upgrade and rejected
login. New/old snapshot, wrong submitted account and malformed hash cases pass.
All 261 adapter tests then passed in `identity-bound-refusal-restored`.
These are synthetic checks; the existing live legacy session has not acquired
an anchor and was not silently migrated or reauthenticated.
