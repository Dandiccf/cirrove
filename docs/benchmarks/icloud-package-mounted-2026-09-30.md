# Captured iCloud package through a read-only FUSE mount

## Registration

Question: does the source-checked Pages archive already validated on private disk
and through the shared block cache also survive the normal Engine directory
publication path, a real FUSE full read, and an offline Engine/FUSE remount?
Prediction: the mounted file reports the actual archive size and its full SHA-256
matches the download receipt in both arms; staging happens once and no fallback
provider content read occurs. The source's logical package size must not be used
as the archive's file length.

Use a fresh UUID and the isolated session from fixture
`21d05f56-70c4-47a8-a5e3-577ab68994ac`. The feature-only command
`--account-package-mounted SESSION_RUN RUN_UUID` downloads one Pages sample
with a 64 MiB bound and before/after source revision checks. It then repeats the
private disk/block-cache verification from the preceding registered arm. A
captured provider exposes only this verified archive through the new streaming
staging hook, with a neutral filename. Normal Engine and CloudFs publish and
mount it read-only. Check the real mount type and OS read-only flag, then stat
and SHA-256 the complete file with an external Python reader. Stop and unmount.
Reopen Engine and mount the same state with all captured-provider metadata and
content access refused. Read and hash the entire file again. Both mounts must
detach; the staging counter must remain one and fallback content requests zero.

No user document is written, extracted or opened in an editor. Private source
identity and content stay in mode-0700 staging, not public reports. No installed
service/account changes. Retain private artifacts and run manifest. Record command,
PID, binary digest and disk-backed TMPDIR before execution; no competing local
build/measurement. Outer timeout 900 seconds, external reader timeout 180 seconds
per arm. This is functional validation, not a performance comparison. It proves
a captured generated artifact through FUSE, not automatic iCloud package discovery,
native editor compatibility, package editing or power-loss durability.

## Result

Run `f94a6398-882d-40be-9281-a89fd8635ea6` passed in 9.649 seconds, including
fresh download, private disk/cache verification and both mount arms. The actual
21,759,419-byte archive passed full SHA-256 reads through read-only `fuse.cirrove`
and through a second Engine/FUSE mount with provider access disabled. Staging
occurred once; fallback provider content reads were zero. Both mounts detached.
Evidence: `icloud-package-mounted-live-2026-09-30.json`. This single functional
run is not a latency or reliability estimate. The normal iCloud adapter does not
yet use this generated-artifact path; account package discovery, eviction/refetch
and native editor behavior remain separate integration work.
