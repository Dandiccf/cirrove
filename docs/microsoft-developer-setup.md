# Personal Microsoft development setup for Cirrove

Use this guide to create a Microsoft identity and an Entra directory that you manage
personally, register Cirrove there, and connect your existing OneDrive accounts.
The registration can belong to your personal project while the files belong to
your private Microsoft account or an employer's Microsoft 365 account.

Microsoft documentation checked **6 September 2026**. Portal labels and signup
eligibility can change. These instructions have been checked against the sources
linked below and Cirrove's current CLI; a fresh-account signup has not been completed
as part of writing this guide.

Cirrove is a read-only development preview. This setup enables its browser sign-in;
it does not establish that real-provider reliability testing is complete.

Follow [personal login](#1-create-your-personal-project-login),
[Azure signup](#2-sign-up-for-azure-using-that-login),
[Entra directory](#3-find-and-verify-your-new-entra-directory),
[app registration](#4-register-cirrove-for-private-and-work-accounts),
[desktop callback](#5-configure-the-native-desktop-callback),
[permissions](#6-add-cirroves-read-only-microsoft-graph-permissions), then
[connect in Cirrove](#7-connect-the-accounts-from-cirrove).

## What you will create

| Component | Purpose | Example |
| --- | --- | --- |
| Personal Microsoft account | Your login for managing the project environment | A dedicated Outlook.com address you choose |
| Personal Azure account/subscription | Signup and billing relationship associated with your directory | The Azure offer you select during signup |
| Microsoft Entra workforce tenant | Directory containing the Cirrove app registration | A personally managed directory, often initially called Default Directory |
| Cirrove app registration | Identifies the Linux application during Microsoft sign-in | `Cirrove Development` and its Application (client) ID |
| Account connected in Cirrove | Owns or can access the files you mount | Your existing private OneDrive or goodguys work account |

The app registration does not supply a new SharePoint subscription. Its directory
does not need to contain copies of your cloud files. Microsoft explains the
[registration and sign-in relationship across tenants](https://learn.microsoft.com/en-us/entra/identity-platform/howto-convert-app-to-be-multi-tenant).

## 1. Create your personal project login

1. Open a separate browser profile for Cirrove administration so the portal uses
   the intended account. An incognito window also works for the initial signup.
2. Open [Microsoft account signup](https://signup.live.com/). Create a personal
   account using an email you control, or create a new Outlook.com address. An
   existing personal Microsoft account is also usable if you prefer it.
3. Complete Microsoft's identity/email verification and account information prompts.
4. Sign in at [Microsoft account management](https://account.microsoft.com/) and
   confirm that the displayed account is your personal project identity.
5. Set up its recovery and sign-in methods under **Security**. Complete MFA
   registration when Azure/Entra asks for it; those administration portals enforce
   multifactor authentication.

Microsoft documents [personal account creation](https://support.microsoft.com/en-us/account-billing/how-to-create-a-new-microsoft-account-a84675c3-3e9e-17cf-2911-3d56b15c0aaf)
and [MFA for administration portals](https://learn.microsoft.com/en-us/entra/identity/authentication/concept-mandatory-multifactor-authentication).

You now have a personal login. Continue below to obtain the directory needed for
app registration. A Microsoft Store developer account or Partner Center enrollment
is not part of this procedure.

## 2. Sign up for Azure using that login

1. Open [Azure account signup and offer comparison](https://azure.microsoft.com/en-us/pricing/purchase-options/azure-account).
2. Choose **Try Azure for free** if Microsoft considers you eligible. The free
   offer is for new Azure customers; creating another email address does not
   establish eligibility. If the portal offers only pay-as-you-go, review its
   billing terms before deciding to continue.
3. Sign in with the personal identity from step 1. If asked to choose an account
   type for that identity, choose **Personal account**.
4. Supply the requested contact and billing information, phone verification and
   payment method. Use your actual details and billing country; a project display
   name does not replace legal billing information.
5. Complete signup, then open [Azure portal](https://portal.azure.com/).

Expect a phone number and an accepted non-prepaid payment card to be required.
Microsoft may place a temporary verification hold. Its free offer does not
automatically charge the card; continuing Azure services beyond its free-credit
period requires choosing pay-as-you-go. See the [Azure account FAQ](https://azure.microsoft.com/en-us/pricing/purchase-options/azure-account).

### What this setup costs and provisions

Microsoft Entra ID Free is included with the relevant Azure billing account.
This guide uses basic directory/application features and does not require an
Entra P1/P2 trial. [Entra ID Free](https://learn.microsoft.com/en-us/azure/cost-management-billing/manage/microsoft-entra-id-free)

Cirrove runs on your Linux computer. These instructions create no Azure VM, App
Service, storage account, database or other hosted Cirrove workload. If you later
enable paid Azure services, their usage is billed under your chosen subscription.
Keep the subscription and directory in good standing and review any lifecycle
notices; a free signup offer is not a promise of permanent, maintenance-free access.

## 3. Find and verify your new Entra directory

1. Open [Microsoft Entra admin center](https://entra.microsoft.com/) in the same
   browser profile, completing any MFA prompt.
2. Use the account/directory switcher or **Settings → Directories + subscriptions**
   to select the directory associated with your personal Azure signup. It may be
   named **Default Directory**. Confirm it is your own environment before creating
   the app; the name alone is not sufficient identification.
3. Open **Entra ID → Overview**. Record its **Tenant ID** and **Primary domain**
   (usually ending in `.onmicrosoft.com`) in your private project notes.
4. Open **Entra ID → App registrations** and check that **New registration** is
   available. Microsoft's current quickstart calls for an account with at least
   the **Application Developer** directory role. An Azure billing/subscription role
   is a separate permission system; if registration is denied, check your Entra
   permissions in this directory.

The [app registration quickstart](https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app)
explicitly allows using the Default Directory. For this procedure, use a normal
**workforce** directory; Cirrove's current authentication code targets Microsoft's
standard identity endpoints, not an External ID customer/CIAM tenant.

### If the portal says only paid customers can create a tenant

First look for the directory already associated with your new Azure signup.
You do not need a second directory just to register Cirrove. Microsoft's current
restriction applies to creating **additional** workforce tenants from the Entra
admin center with a free/trial account. Its guidance points new users to Azure free
account signup instead. [Tenant creation rules](https://learn.microsoft.com/en-us/entra/fundamentals/create-new-tenant)

If your personal signup completed but no usable directory appears, check the signed-in
account and directory filter, then use Azure signup/support guidance. Do not select
an unrelated company tenant or an External ID customer tenant to get past the message.

If you already have an eligible paid Azure environment and deliberately want a
second directory, Microsoft's tenant guide describes **Manage tenants → Create →
Microsoft Entra ID**, followed by its display name, available initial domain and
region. That additional-directory route is optional and eligibility-dependent.

### Optional: a directory-native developer login

You can keep using the signup identity when it has the required access. If you want
a separate directory-native identity, an administrator of your personal directory
can use **Entra ID → Users → New user → Create new user**, choose a username such
as `developer` under the directory's actual `.onmicrosoft.com` domain, and complete
the account setup. Assign **Application Developer** for app-registration work,
then sign in with that new account and complete its password/MFA prompts.

This new organizational login is distinct from your Outlook.com login. It does not
automatically come with a licensed OneDrive for Business or SharePoint environment;
Graph's [OneDrive provisioning](https://learn.microsoft.com/en-us/graph/api/drive-get?view=graph-rest-1.0)
depends on the user's license. Keep the original administration access available.
See [create a directory user](https://learn.microsoft.com/en-us/entra/fundamentals/how-to-create-delete-users)
and the [Application Developer role](https://learn.microsoft.com/en-us/entra/identity/role-based-access-control/permissions-reference#application-developer).

## 4. Register Cirrove for private and work accounts

In your personal development directory, open **Entra ID → App registrations →
New registration** and use these settings:

| Setting | Value for this guide |
| --- | --- |
| Name | `Cirrove Development` |
| Supported account types | **Any Entra ID Tenant + Personal Microsoft accounts** |
| Redirect URI on this first form | Leave blank; configure the desktop platform in step 5 |

Depending on the portal version, the account-type label may spell out accounts in
any organizational directory plus personal Microsoft accounts. This is the option
that includes **both** business and consumer identities. A personal-account-only
registration cannot sign into goodguys; a single-tenant registration in your new
directory does not provide normal sign-in for accounts in goodguys's directory.
[Supported account types](https://learn.microsoft.com/en-us/entra/identity-platform/single-and-multi-tenant-apps)

Select **Register**. On the resulting **Overview** page, record:

| Value | How we use it |
| --- | --- |
| **Application (client) ID** | Pass to Cirrove as `--client-id` |
| **Directory (tenant) ID** | Identifies the directory managing this registration; keep for administration |
| **Object ID** | Not the value Cirrove needs for sign-in |

For the mixed private/work setup below, Cirrove uses **`--tenant common`**, not the
development directory's tenant ID. Microsoft discovers the account's actual tenant
during sign-in. These IDs are identifiers, not passwords or client secrets.
[Microsoft registration instructions](https://learn.microsoft.com/en-us/graph/auth-register-app-v2)

## 5. Configure the native desktop callback

1. In the registration, open **Authentication → Add a platform**.
2. Select **Mobile and desktop applications**.
3. Add the custom redirect URI **`http://localhost`** and save/configure the platform.
4. Verify that URI appears under the desktop platform.

Cirrove opens your browser and temporarily listens on a local port. It uses
authorization code flow with PKCE. Microsoft ignores the port when matching a
localhost redirect, so you do not register a different port for every login.
[Redirect rules](https://learn.microsoft.com/en-us/entra/identity-platform/reply-url)

Leave unrelated authentication settings at their defaults. This flow does not need
a client secret, implicit access/ID-token grants, or a hosted redirect webpage. Use
the desktop platform rather than a Web or SPA platform. The callback must return
to the computer running the Cirrove command; a browser on your phone cannot use
its own localhost to reach your Linux session.

## 6. Add Cirrove's read-only Microsoft Graph permissions

1. Open **API permissions → Add a permission → Microsoft Graph**.
2. Choose **Delegated permissions**.
3. Add **`User.Read`** if it is not already present.
4. Add **`Files.Read.All`**.
5. Verify both are listed as **Delegated**.

`User.Read` supplies the signed-in identity. Delegated `Files.Read.All` permits
reading files accessible to that user, including linked libraries; it does not
provide unattended access as the app to every account in an organization. The
permission is also available for personal Microsoft accounts. [Graph permissions](https://learn.microsoft.com/en-us/graph/permissions-reference#filesreadall)

The current Cirrove build additionally requests `openid`, `profile` and
`offline_access` during browser consent. You do not need to add write permissions
or application permissions for this preview. A green grant in the developer
directory does not grant access inside a different company directory.

### Consent when connecting goodguys

Creating the app in your own directory does not change goodguys's consent policy.
You must sign in as your goodguys user to access that user's business files.
If the browser shows **Need admin approval**, use the company's consent process.
Approval, if required, must come from an authorized administrator of the tenant
containing those business accounts. [User and administrator consent](https://learn.microsoft.com/en-us/entra/identity-platform/howto-convert-app-to-be-multi-tenant#understand-user-and-admin-consent-and-make-appropriate-code-changes)

A new personally managed app may show an **unverified publisher**. Some tenants
block user consent to such apps, especially for permissions beyond basic profile
access. That can require admin approval even though `Files.Read.All` is a delegated
permission. Publisher verification is a separate process with partner-account
requirements; an open-source repository does not confer verified status.
[Publisher verification and consent restrictions](https://learn.microsoft.com/en-us/entra/identity-platform/publisher-verification-overview)

## 7. Connect the accounts from Cirrove

Run these commands on the Linux desktop. For this workspace:

```sh
cd "$HOME/Work/cirrove"
cargo build --workspace --locked
./target/debug/cirrove keyring-check
```

The keyring check writes, reads and removes a uniquely named synthetic credential.
An unlocked desktop Secret Service keyring is required. Complete the runtime
prerequisites in [OneDrive setup](onedrive-setup.md#build-and-sign-in) if necessary.

Set the client ID copied from **Overview**, replacing the placeholder:

```sh
CIRROVE_CLIENT_ID='PASTE-YOUR-APPLICATION-CLIENT-ID-HERE'
```

To connect your private OneDrive:

```sh
./target/debug/cirrove connect \
  --label personal-onedrive \
  --client-id "$CIRROVE_CLIENT_ID" \
  --tenant common \
  --mount-path "$HOME/Cloud/Cirrove-Personal"
```

Choose the Microsoft account that actually contains your personal files. That can
be your existing private account, even if you created a different one to manage
the project. The new project's empty OneDrive is a different drive.

To add goodguys with the **same app registration**:

```sh
./target/debug/cirrove connect \
  --label work-onedrive \
  --client-id "$CIRROVE_CLIENT_ID" \
  --tenant common \
  --mount-path "$HOME/Cloud/Cirrove-Work"
```

Choose your **goodguys work account** in Microsoft's account picker. For each
connection, read the identity and tenant printed by Cirrove, then select the
intended drive from its list. The login command saves that connection and exits.
Use distinct, empty mount locations outside existing cloud-client mounts.

Start the daemon in one terminal:

```sh
./target/debug/cirroved
```

In another terminal, check the saved accounts and running service:

```sh
cd "$HOME/Work/cirrove"
./target/debug/cirrove accounts
./target/debug/cirrove status
```

Open the configured mount paths in Files/Nautilus. Matching the account and visible
files is the first check; successful registration alone does not validate linked
SharePoint access, token renewal or long-running recovery. Those remain part of
Cirrove's [acceptance testing](validation.md#required-before-calling-stages-13-complete).

## Common setup problems

| What you see | What to check |
| --- | --- |
| Only goodguys appears in the directory picker | Check which identity completed Azure signup. Use your personal administration browser profile and its directory. |
| New registration is disabled or access is denied | Confirm the selected directory and your Entra app-registration permissions. Check the account's Application Developer role or ask that directory's administrator. |
| Tenant creation is limited to paid customers | Use the directory from initial Azure signup; see step 3 before trying to create another tenant. |
| Personal Microsoft login is rejected by the app | Check that the registration supports organizational **and** personal accounts, and that the command uses `--tenant common`. |
| The app is not found in the selected tenant | Check the client ID, supported account types and authority. The Object ID is not the client ID; the development tenant ID is not the right authority for this mixed-account walkthrough. |
| Redirect mismatch | Check the desktop platform contains `http://localhost`, then start a fresh `connect` command. Keep the callback on the Linux computer running Cirrove. |
| Need admin approval / unverified publisher | Follow consent policy in the company tenant being accessed. Approval in your personal development directory does not approve the app for goodguys. |
| Login succeeds but OneDrive is empty or unavailable | Check the account and drive selected. A fresh project account may have no files; an unlicensed `.onmicrosoft.com` developer user may have no OneDrive for Business. |
| Browser sign-in works but credentials cannot be saved | Check/unlock the desktop keyring and run `cirrove keyring-check` before reconnecting. |

## Why this guide does not start with the Microsoft 365 Developer Program

Its E5 sandbox is useful when eligible developers need a disposable Microsoft 365
test environment, including SharePoint. Eligibility is conditional; signing up
with a personal Microsoft account or maintaining an open-source repository does
not by itself guarantee a sandbox. Sandbox lifecycle and renewal requirements
also make it unsuitable as an assumed permanent home for a project's registration.
[Microsoft 365 Developer Program FAQ](https://learn.microsoft.com/en-us/office/developer-program/microsoft-365-developer-program-faq)

For this guide, a personally managed Azure/Entra directory identifies the application
and existing private/work accounts supply the test drives. A separate E5 sandbox
can be considered later if you qualify and need isolated business test data.

## What to keep and what to share for Cirrove setup

Keep the administration login, recovery methods, home tenant ID and client ID in
your project records. For the commands in this guide, the needed setup values are:

```text
Application (client) ID: the UUID from the Cirrove registration
Sign-in authority: common
Supported accounts: organizational + personal
Redirect URI: http://localhost (Mobile and desktop applications)
Graph permissions: User.Read and Files.Read.All (Delegated)
```

The client ID can be supplied to whoever is helping configure Cirrove. Do not send
passwords, recovery codes, browser callback URLs or token values. No client secret
is required. Registration ownership and private credentials stay with you even
though Cirrove's source code is public.
