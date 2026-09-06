# OneDrive preview setup

Cirrove currently requires your own Microsoft app registration. No project-wide
registration is bundled, and Cirrove does not reuse another cloud client's identity
or credentials. The preview requests **read-only** access.

Starting with a personal Microsoft account and no development directory? Follow
[Personal Microsoft development setup](microsoft-developer-setup.md) first. It covers
Azure signup, the initial Entra directory, costs, registration and both private/work
connections. The shorter steps below assume you already have a suitable directory.

## Register the desktop application

1. Open the [Microsoft Entra admin center](https://entra.microsoft.com/), choose
   your tenant, then **Entra ID → App registrations → New registration**. Name it
   `Cirrove development`.
2. If the app and all intended work accounts belong to the **same tenant**, you can
   use **Single tenant** and that Directory (tenant) ID. For an app in your own
   development directory that should access personal OneDrive and work accounts in
   other tenants, choose **Any Entra ID Tenant + Personal Microsoft accounts**.
   In that case, pass `--tenant common` below; the development directory's ID is
   retained for administration, not used as the sign-in authority for those accounts.
3. Record the **Application (client) ID**. These IDs identify the application and
   tenant; they are not client secrets. See Microsoft's
   [registration instructions](https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app).
4. Open **Authentication (Preview)** (or **Authentication**), then **Redirect URI
   configuration → Add Redirect URI**. In the platform chooser, select **Mobile
   and desktop applications**, enter `http://localhost` as a custom redirect URI
   (or select it if offered), and select **Configure**. Save any pending changes.
   In the older portal, the equivalent button is **Platform configurations → Add
   a platform**. See Microsoft's [current redirect setup](https://learn.microsoft.com/en-us/entra/identity-platform/how-to-add-redirect-uri).
   Cirrove listens on an ephemeral loopback port. Microsoft ignores that port when
   matching a localhost redirect.
   See the [redirect rules](https://learn.microsoft.com/en-us/entra/identity-platform/reply-url).
5. Under **API permissions**, add Microsoft Graph **delegated** permissions
   `User.Read` and `Files.Read.All`. The latter includes files accessible to the
   signed-in user, which is needed for linked SharePoint libraries. Cirrove also
   requests `openid`, `profile` and `offline_access` during consent. Your tenant's
   policy may require an administrator to approve consent. See the
   [permissions reference](https://learn.microsoft.com/en-us/graph/permissions-reference#filesreadall).

This is a public desktop client using authorization code flow with PKCE. Do not
create a client secret, enable implicit grants, or add application-wide permissions.

## Build and sign in

Requires Linux, a working desktop Secret Service keyring, `xdg-open`, `/dev/fuse`
and `fusermount3`. On Arch the runtime helper is provided by `fuse3`. The kernel must
advertise `FUSE_DIRECT_IO_ALLOW_MMAP` for application memory-mapping support. Build
with the pinned Rust toolchain:

```sh
cargo build --workspace --locked
./target/debug/cirrove keyring-check
./target/debug/cirrove connect \
  --label work-onedrive \
  --client-id YOUR_APPLICATION_ID \
  --tenant YOUR_TENANT_ID \
  --mount-path "$HOME/Cloud/Cirrove-OneDrive"
```

The browser opens Microsoft's account selector. After sign-in, the terminal shows
the verified user and tenant, followed by drive choices with their types and web
addresses. Choose your actual Documents/OneDrive drive. The connection is saved
only after that drive's root is verified. The login command then exits.

Use a new, empty directory outside other cloud mounts. Existing Stratosync, rclone
or other clients are not changed. Configuration and metadata default to
`$XDG_STATE_HOME/cirrove` (usually `~/.local/state/cirrove`); secrets are stored only
in the desktop keyring. `--state-dir` supports a separate development installation.

## Start and control the service

```sh
./target/debug/cirroved
```

In another terminal:

```sh
./target/debug/cirrove status
./target/debug/cirrove accounts
./target/debug/cirrove disable work-onedrive
./target/debug/cirrove enable work-onedrive
./target/debug/cirrove reauth work-onedrive
```

The service notices account changes within about five seconds. Ejecting an enabled
mount is treated as accidental and triggers another mount attempt. Use `disable`
when you intend to leave it unmounted. Local files placed into an ejected mount
point are preserved: Cirrove refuses to mount over them.

`reauth` temporarily disables only that account, waits for its workers to stop,
and verifies the same Microsoft identity before replacing its credentials. If the
command is killed during sign-in, use `enable` to restore the desired mount state.
An unlocked desktop keyring is needed for sign-in and credential refresh.

The unit template in `packaging/systemd/cirroved.service` supports login startup;
it is not installed by building the project. Keep service installation for an
explicitly configured preview. Its daemon and FUSE sessions must remain in the
same user's desktop session. Pins, uploads, tray UI and Nautilus badges are still
planned; a read-only mount does not claim those capabilities.
