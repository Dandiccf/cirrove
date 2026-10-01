# Public Apple Webclient evidence: native package upload and registration

Date: 2026-10-01. Scope: public HTML/JavaScript GETs only, using a fresh urllib client without cookies, authorization headers, browser state, login, or Apple service mutations. No code was executed from these assets. The initial investigation changed no repository files; this reviewed copy accompanies a pure parser/mapper preparation only. The facts below describe shipped client code, not a successful Cirrove/Apple transaction or a supported public API.

## Finding

The deployed Drive client DOES have a separate package creation path: recognize an archive file, request an upload slot with type PACKAGE, POST the entire archive to the returned content URL, parse a package receipt, then register with add_package. This materially advances the earlier pyicloud/rclone review: their examined ordinary-file upload paths do not expose this protocol.

Important: this client does NOT generate package manifests or component checksums locally. The content-upload response supplies those fields. Its local mapper converts encodings and reshapes the receipt. A pure receipt parser/registration mapper is therefore justified by the code; a locally invented package-manifest algorithm is not.

## Public provenance and retained files

Anonymous GETs of https://www.icloud.com/iclouddrive/ and https://www.icloud.com/pages/ reference the same shell asset. The shell constructs application URLs as /applications/{applicationName}/current/{locale}; its application configurations name iclouddrive and pages. Both corresponding current/en-us/ application HTML responses referenced the same Drive Build2636Build19 assets and were byte-identical. The Drive main bundle explicitly maps dynamic chunk 32 to uploadFiles~5ad45e42.js; that referenced chunk was fetched. There was no guessed mutating endpoint invocation.

Retained public files and machine-readable hashes: /var/tmp/cirrove-public-icloud-js-x3eqdejj/manifest.json. Search anchors: offsets.json. All offsets below are ZERO-BASED UTF-8 BYTE offsets in the exact saved files, not character indices. Initial investigation output used character offsets; this report corrects that distinction.

| Local file | Public asset URL | Bytes | SHA-256 |
| --- | --- | ---: | --- |
| shell-main.js | https://www.icloud.com/system/icloud.com/2636Build34/en-us/main.js | 3953909 | d50713ea804e6538252e64fff0d050d96bd02fa69ac0ec9bbbb829691e8a4363 |
| drive-main.js | https://www.icloud.com/applications/iclouddrive/2636Build19/en-us/main.js | 589227 | 28a365b0629aba447bf3d6048bbe45e8c69fc6f1803283dc51f9f0b5484a813d |
| uploadFiles~5ad45e42.js | https://www.icloud.com/applications/iclouddrive/2636Build19/en-us/uploadFiles~5ad45e42.js | 11520 | f9f8247f579fc8a61623b68fc81b356f6b21bc6c79ae037b7157a00c358312ee |
| uploadFiles-retryUpload~31632a63.js | https://www.icloud.com/applications/iclouddrive/2636Build19/en-us/uploadFiles/retryUpload~31632a63.js | 2320 | 06619f0e0dffb90ac3387eb950ad2cc5d452f9dea35f9ca4187bf484611fdf9e |

The assets carry Apple's license notice. This report records protocol facts in independently written prose; it does not propose copying or porting Apple's implementation.

## 1. Detection and input contract (observed code)

Drive main module 1880 occupies bytes [55020,55366); module 1881 occupies [55367,55787). Module 1881 export a / minified function r examines a native File's name and MIME type. A package candidate has a filename ending in .{recognized extension}.zip and a MIME string matching zip, compressed, or octet case-insensitively. The extension allowlist includes pages, pages-tef, template, numbers, numbers-tef, nmbtemplate, key, key-tef, keynote, keynote-tef, kth, plus other package types. This is upload UI classification, NOT proof that arbitrary ZIP content is a valid document. The regex is case-insensitive, but the captured extension is looked up directly in a lower-case object; do not infer robust case normalization from this branch.

Module 1879 at byte 54629 splits presentation names and removes the outer ZIP extension for recognized package suffixes. The upload allocation branch independently removes trailing .zip for a detected package.

Upload chunk module 2211, validation function _ at byte 10319, reads at most the first 1,000,000 bytes through FileReader to reject unreadable/folder selections. It checks filename restrictions, excluded system/sync artifacts, enclosing APP_LIBRARY supportedExtensions (when present), and a file-size ceiling of 10,737,418,240 bytes (byte 11244). It does not unzip, inspect member paths, produce manifests, or check native document semantics in this path. That ceiling is Apple's UI policy, not a tested Cirrove capacity claim.

Missing evidence: ZIP internal root layout, required native-document members, preservation of resource forks/xattrs/symlinks, compression/ZIP64 acceptance, and whether Cirrove's timestamp-normalized read artifact is accepted for a roundtrip.

## 2. Slot allocation and authentication/zone context (observed code)

Drive main module 2214 occupies bytes [531961,538016); export b is the async upload coordinator C. Endpoint builders start at byte 532610. Its allocation request is POST to the authenticated docws service path ws/{targetFolder.zone}/upload/web. The JSON body (byte 533898) contains filename, type, content_type, and size. Filename is encodeURI(nativeFile.name), with trailing .zip removed for the package branch. type is PACKAGE for that branch and FILE otherwise. content_type is the native File MIME and size is the archive byte length. Request Content-Type is text/plain.

The slot URL has a token query parameter derived from the existing X-APPLE-WEBAUTH-VALIDATE cookie: module 1191 getter at byte 26518 and module 1190 envelope parser at byte 26296. These are public code identifiers only; no cookie/token values were read. The shell's Drive and Pages application configurations at bytes 1529883 and 1534160 declare isPCSRequired and an application-specific pcsKeyIdentifier. No separate package-only PCS exchange is present in the inspected upload modules. The concrete authenticated service URL and generic FetchService behavior come from injected services; this inspection alone does not establish all runtime credential requirements.

Shared-folder allocation is distinct: when the destination is SHARED_FOLDER or FOLDER_IN_SHARED_FOLDER with shareID, the path becomes ws/{shareID.zoneID.zoneName}/upload/web/{ownerRecordName}/{recordName}. Do not infer that using the ordinary zone path works for shared documents.

Module 2251 schema WireCWSUploadUrlDetails (byte 573010) expects one array entry containing url, document_id, owner_id, and optional owner. The coordinator uses the returned URL/document_id and optional owner. This is evidence of an allocated identity before registration, not proof of server idempotency.

## 3. Archive transfer and package receipt (observed code)

The upload coordinator calls the injected transport at main byte 534713. Upload chunk module 2211 defines that transport as async E at byte 3567. It opens an XMLHttpRequest POST to the returned URL, sets Content-Type from nativeFile.type, attempts to set Content-Length from nativeFile.size, then sends the File object itself (byte 4732). This inspection does not establish whether the browser honors that attempted Content-Length assignment; it is not a separate upload-contract guarantee. The salient contract is a raw whole-file body, NOT multipart form-data and NOT a browser-side sequence of component uploads.

The UI maps HTTP 413 during slot allocation or content transfer to its out-of-storage-quota message. This is a code classification, not observed evidence of Apple's actual quota failure response; it does not establish that all 413 responses mean quota rather than input size. A 2xx response is parsed as WireDocWSUploadWebResponse from main module 2251. Ordinary response: singleFile. Package response fields:
- hexBrSyntheticChecksum: nonempty even-length hexadecimal.
- brManifest: optional asset receipt.
- ckManifest: optional asset receipt.
- ckSectionAssets: array of asset receipts (required, with no minimum/bound declared in this UI schema).

Each asset receipt has hexFileChecksum, hexReferenceChecksum, hexWrappingKey (even-length hexadecimal); receiptToken (standard or URL-safe Base64); and size (number). The browser additionally refuses a package response with missing/empty hexBrSyntheticChecksum; ordinary singleFile responses are checked against input byte size. It does NOT compare a package synthetic checksum with a browser-computed archive hash.

Strong inference, not independently observed server implementation: the content endpoint consumes the ZIP and constructs the package assets/manifests whose receipts it returns. Directly proven: the browser sends one File body and consumes the returned component receipt set; there is no local manifest/checksum construction in this path.

No digest algorithm for hexBrSyntheticChecksum is exposed here. Its name does not justify assuming SHA-256 or equating it to the raw archive SHA-256.

## 4. Registration transform and add_package (observed code)

Module 2251 occupies [571914,573743). Export b / minified g at byte 573418 maps the package receipt. Helper s hex-decodes bytes then produces standard Base64. Helper l normalizes URL-safe Base64 characters to standard Base64 and supplies padding. Helper j at byte 573564 maps an asset receipt:
- signature from hexFileChecksum (hex bytes to Base64).
- reference_signature from hexReferenceChecksum (same conversion).
- wrapping_key from hexWrappingKey (same conversion).
- receipt from receiptToken (Base64 normalization).
- size unchanged.
Absent optional brManifest or ckManifest becomes null, not an invented empty receipt.

The mapper returns pkgSignature from hexBrSyntheticChecksum, brManifest as the mapped br receipt, and package containing sections (mapped ckSectionAssets in original order) and manifest (mapped ckManifest).

The coordinator builds registration at byte 536079. It RENAMES mapper brManifest to the TOP-LEVEL manifest key. Final package-specific registration keys are command=add_package, pkgSignature, manifest, package.sections, package.manifest. The sibling branch is command=add_file with data from singleFile. The branch is selected by returned receipt shape, not by rechecking the requested slot type. A safe independent Cirrove implementation should explicitly bind expected representation to the selected response variant rather than inherit a potential silent fallback.

Common registration fields: allocated document_id; path.starting_document_id (target folder docwsid, or shared-root recordName); path.path (final filename without outer ZIP suffix); allow_conflict=true; file_flags is_writable=true, is_executable=false, is_hidden=false; mtime and btime from File last-modified time in milliseconds.

For targetFolder.share==TO_ME and a returned owner, helper A at byte 532707 annotates the top-level manifest, package manifest and each package section with that owner. This is an explicit cross-reference binding; it is not a user-supplied replacement owner. A missing/null manifest remains null.

Module 2250 class C.updateDocument at byte 567386 sends registration to docws ws/{zone}/update/documents, with errorBreakdown=true. Its shared-item branch uses ws/{shareID.zoneID.zoneName}/update/shared/{ownerRecordName}; it obtains ancestor share metadata through module 1188. Body Content-Type remains text/plain. The retry branch handles HTTP409 only when a parsed failure says ZONE_BUSY. Do not transfer Apple's allow_conflict=true or request retries into Cirrove's exact-id/reconciliation safety policy without separate analysis.

## 5. Response identity chain and missing replacement contract

Wire DocwsUpdateDocument in module 2250 requires a top-level status with numeric status_code/string error_message and results containing document + status. The document schema (byte 565961) has item_id, document_id, etag, size, optional name and short_guid. The upload coordinator takes results[0].document, reconstructs a FILE::zone::document_id identity using the target zone, sets parentId from the destination, carries returned item_id/etag/size/short_guid, and uses FILE_IN_SHARED_FOLDER for a shared target. Conversion begins at byte 536735.

The inspected UI shape checks do not establish Cirrove-grade confirmation: they do not demonstrate an allocated-document-id equality check, expected-version compare-and-swap, independent content readback, or persistent crash checkpoint. Those remain requirements for Cirrove's adapter, not claims about this source.

This branch proves package CREATION/IMPORT support in public client code. It does not prove safe in-place replacement of an existing native document. The registration body contains a newly allocated document_id and add_package, not an expected original ETag. A package-aware create-under-staging followed by Cirrove's conservative identity-bound replacement/handoff may be viable, but its package verification, original recovery, and interruption semantics still need implementation and evidence. Do not remove existing package/AppLibrary mutation protections based on this source inspection.

## Smallest justified next implementation

Prepare a pure, bounded typed package receipt parser and registration mapper, with synthetic checks for malformed hex/Base64, asset counts/total size, absent optional manifests, preserved section order, explicit Data-versus-Package response binding, and account/operation/zone binding supplied by the enclosing checkpoint. Keep transport disabled behind a validator until a separately authorized own-document arm establishes ZIP input layout and response/identity/content behavior. Do NOT compute server manifest checksums locally from the raw archive using an assumed algorithm.

No live acceptance has been performed. No native Pages/Numbers/Keynote editing reliability, shared-package support, or package overwrite support is established by this report.


## Prepared pure parser: not yet executed or provider-validated

The follow-up adds feature/test-gated `package_upload.rs`: no ordinary upload routing,
HTTP request, package mutation guard, installed service or account changes. It
parses only the Package response shape, rejects unknown/duplicate fields and
ordinary/mixed singleFile responses, and returns package-specific registration
JSON as SecretString. It does not return a remote-success receipt or claim an
account/operation binding the server response does not contain.

Initial local safety bounds: 64 KiB source JSON and output fragment, 64 sections,
256 decoded bytes per hex field, 4096 encoded bytes per receipt token, and a
checked aggregate asset size no larger than i64::MAX. These are deliberately
conservative validator limits, not inferred Apple maxima. At least one asset
(manifest or section) is required by local policy; Apple's schema alone does not
establish that requirement. Missing optional manifests map to null in registration;
explicit null/malformed manifests in the upload response are rejected. Unknown
fields fail closed, so a changed server schema will require deliberate review.

No checksum algorithm or checksum length is inferred from field names. Asset
sizes are not equated with uploaded archive size. Ordinary and replacement
checkpoint limits (8 KiB / 32 KiB) and sealed storage bounds remain unchanged;
future network integration must explicitly design the package checkpoint budget.

### Planned tests and exact negative controls (not run)

The eight `package_upload::tests` scenarios check distinct manifests and ordered
sections, optional versus malformed manifests, ordinary/mixed/duplicate fields,
hex conversion and limits, Base64 canonicalization, raw-byte/section boundaries,
integral/aggregate sizes, and nonleaking errors/no invented identity authority.

Parent validation must first demonstrate behavioral failures with controlled
edits in an isolated checkout:

1. In `registration_data`, set `Registration.manifest` to `&self.ck_manifest`
   instead of `&self.br_manifest`. The distinct-manifest test must fail at exact
   output equality. Restore before other controls.
2. In `hex_base64`, replace `STANDARD.encode(bytes)` with
   `STANDARD.encode(value.as_bytes())` (remove the unused decoded binding in this
   negative only if clippy is part of the command). The exact mapping and hex
   tests must fail because encoded text is not decoded checksum bytes.
3. Remove `deny_unknown_fields` from `RawPackageReceipt`. The ordinary/mixed-field
   test must fail at the mixed receipt assertion (ordinary-only still lacks fields).
4. Change `bytes.len() > MAX_RECEIPT_BYTES` to `bytes.len() > MAX_RECEIPT_BYTES + 1`.
   The byte-boundary test must fail on one extra trailing whitespace byte.

Prepared code has been formatted and its patch application checked only. No test,
build, real receipt capture or Cloud operation accompanies this preparation.

## Parser validation, 2026-10-01

All four prescribed negative controls failed at their intended assertions:
BR/CK manifest substitution, encoding hex text instead of decoded bytes, allowing
mixed Package/singleFile fields, and accepting one byte over the input cap. Each
was restored separately. All eight package parser tests then passed, with 158
unrelated library tests filtered out; the probe binary had no matching tests.
Logs/manifests: `.local-state/icloud-access-package-{manifest,bound,mixed,hex}-red-2026-10-01`
and `.local-state/icloud-access-package-parser-green-2026-10-01`.
An independent source review confirmed the mapping and nonleaking error boundary.
The optional-null comment now explicitly describes the inspected client schema,
not an observed server guarantee. No real Package receipt or import was tested.
