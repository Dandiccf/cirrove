# Owned PACKAGE allocation: interpreting owner metadata

## Observed failure and scope

The first registered owned native PACKAGE source acquisition succeeded. Its
subsequent one-shot import stopped at allocation validation, before content
upload or registration. The retained checkpoint remains AllocationStarted with
no slot. The combined error does not establish which predicate failed, and this
change does not reconstruct a missing response or authorize replay of that run.
A new owned operation and fresh preregistration are required for another import.
See the separate owned-package-create evidence for the actual arm outcomes.

## Public code evidence

The inspected public asset is [Apple Drive Build2636Build19 main.js](https://www.icloud.com/applications/iclouddrive/2636Build19/en-us/main.js).
The retained file is 589,227 bytes, SHA-256
`28a365b0629aba447bf3d6048bbe45e8c69fc6f1803283dc51f9f0b5484a813d`.
All anchors below are zero-based UTF-8 byte offsets in that exact artifact.
These are independently described code observations, not copied application code.

- At byte 573010, module 2251 defines a one-element allocation tuple. Its
  document and owner identifiers are strings; owner is an optional string.
  There is no minimum string length or requirement that owner be absent.
- At byte 534648, the upload coordinator selects the allocated document ID,
  content URL and optional owner. The owner identifier is not selected for the
  subsequent content upload or registration operation.
- At byte 536104, owner annotation of package assets is conditional on the
  destination's sharing state being TO_ME as well as a returned owner being
  present. Presence of the allocation field alone therefore does not identify
  a shared destination.
- At byte 533510, endpoint selection uses destination sharing metadata to choose
  the ordinary zone path or a separate shared-folder path. Allocation owner
  metadata does not select that endpoint.

Cirrove's existing ordinary FILE allocator likewise consumes only the URL and
allocated document ID. It does not infer sharing from allocation owner fields.

## Narrow correction and retained boundaries

For the feature-only owned PACKAGE validator, permit an optional owner string
and a present but empty owner identifier. Bound each to 256 UTF-8 bytes as a
local resource policy, not an asserted Apple maximum. Keep the current document
ID character/length policy and the 8,192-byte URL cap plus Apple's HTTPS content
host allowlist unchanged until separately evidenced. Required fields and types,
unknown-field refusal, the one-slot count and the 64 KiB response cap remain.

Neither owner field is forwarded into registration, used to select an endpoint,
or allowed to override an account, parent or document identity. Exact owned
CloudDocs root-child/source guards remain. This adds no shared-target support.
The allocated ID still determines the one fresh created item, and registration
still forbids a name conflict.

Allocation refusals now carry only a fixed typed category: response shape,
result count, document identity, owner-identifier length, owner length, URL
length or content-location policy. Display text and serialization contain no
provider response, identifiers, owner values, URL, host or query data. The
service records only that category and the run UUID when this specific typed
error is present; arbitrary error context is not serialized.

## Prepared validation

Synthetic cases cover absent/empty/nonempty optional owner crossed with empty
and nonempty owner identifier, registration that does not forward owner
metadata, malformed/oversized identifiers and locations with static nonleaking
categories, malformed shape/count, and actual execute-path refusal retaining
AllocationStarted without a slot or body/registration request. A service test
checks that only the typed category is saved even when its error context holds
synthetic private text. Reinstating the old owner-absence/nonempty-owner-ID
predicates must fail the acceptance test; forwarding the owner must fail the
registration authority test.

These additions are prepared but have not been built or executed by their
implementing agent. They do not establish that either relaxed predicate caused
the original rejection, or that Apple accepts a complete PACKAGE import.

## Parent-run validation

The old owner restriction was reinstated as a negative control. The targeted
acceptance test failed with `legacy owner restriction` (08:27:56–08:28:09 UTC).
The corrected transport was restored; all 18 package-create tests passed
(08:31:21–08:31:24 UTC), and all three service artifact/diagnostic tests passed
(08:31:50–08:32:35 UTC). Independent review found no new authority, disclosure,
or replay regression. Complete package import acceptance remains open.
