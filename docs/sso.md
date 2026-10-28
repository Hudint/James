# Single sign-on (OpenID Connect)

Users can sign in with an identity provider such as Keycloak, Authentik, Authelia, Dex or Microsoft
Entra ID. Password logins keep working (e.g. for the first administrator).

1. Create the administrator account first (setup token, see [installation](installation.md)):
   single sign-on is refused until then.
2. Register a client at the provider:
   - type: confidential, authorization code flow (PKCE is used automatically)
   - redirect URI: `https://<your domain>/api/v1/auth/oidc/callback`
   - a `groups` claim in the ID token if you use `allowed_groups` or `admin_groups`
3. Configure James:

   ```toml
   [auth.oidc]
   enabled = true
   issuer = "https://auth.example.com/realms/main"
   client_id = "james"
   client_secret = "…"
   redirect_url = "https://james.example.com/api/v1/auth/oidc/callback"
   display_name = "Keycloak"
   create_users = true
   allowed_groups = ["james-users"]
   default_roles = ["dj"]
   # admin_groups = ["james-admins"]
   ```

4. Restart James. The login page shows **Sign in with Keycloak**.

## Accounts

- **Linked accounts** (`create_users = false`, the default): only accounts an administrator
  linked beforehand can sign in: `PATCH /api/v1/users/<id>` with
  `{"oidcSubject": "<issuer>#<subject>"}`. `<issuer>` must be exactly the `iss` of the ID token
  (watch for a trailing slash) and `<subject>` its `sub`.
- **Created accounts** (`create_users = true`): the first login creates an account (name from
  `preferred_username`, e-mail or name; a number is added if it is taken) with `default_roles`.
  Without `allowed_groups`, **everybody** who can log in at the provider gets an account — do not
  do this with public providers such as Google or a multi-tenant Entra ID app.
- **Allowed groups:** with `allowed_groups`, only members of these groups (or of `admin_groups`)
  can sign in, also with linked accounts.
- **Administrators:** with `admin_groups`, membership decides on every login whether the account
  is an administrator. Losing administrator rights ends the account's other sessions and tokens.
  The last administrator is never demoted this way.

Accounts created by single sign-on have no password and cannot set one themselves (an
administrator can); their API tokens expire after at most 30 days. Changing or removing an
account's SSO link ends its sessions. A login started in one tab is replaced when another tab
starts one (finish in the latest tab). Removing someone at the provider stops new SSO logins, but sessions (up to 30
days) and API tokens keep working: **disable the account in James** (**Users**) to lock someone
out immediately.

## Security

James checks the ID token's signature, issuer, audience, expiry and nonce, and binds the login to
the browser that started it (a short-lived HttpOnly cookie carries state, nonce and PKCE
verifier). The issuer must use HTTPS unless it runs on the same machine (`allow_insecure_http`
allows plain HTTP in trusted networks). James never follows redirects of the provider's endpoints
and accepts at most 1 MiB per response. Failed logins show a generic message; the reason is in
the log.
