//! Single sign-on with OpenID Connect (authorization code flow with PKCE).
//!
//! `GET /api/v1/auth/oidc/login` redirects to the identity provider; it sends the user back to
//! `GET /api/v1/auth/oidc/callback`, where the ID token is verified and the account (matched by
//! the token's subject) gets a session like a password login.
//!
//! The login in progress (state, nonce, PKCE verifier) lives in an HttpOnly cookie of the
//! browser that started it, not on the server, so nobody can fill up a server-side table.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use base64::Engine as _;
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet, EndpointSet,
    IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Pending logins expire after this long (the user has to finish at the provider).
pub const PENDING_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Provider metadata (including signing keys) is fetched again after this long.
const METADATA_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// After a failed discovery, logins fail fast for this long.
const DISCOVERY_RETRY: Duration = Duration::from_secs(30);
/// Largest response accepted from the provider.
const MAX_RESPONSE: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OidcConfig {
    pub enabled: bool,
    /// Issuer URL, e.g. `https://auth.example.com/realms/main`.
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// The public URL of `/api/v1/auth/oidc/callback`, as registered at the provider.
    pub redirect_url: String,
    /// Scopes besides `openid`.
    pub scopes: Vec<String>,
    /// Shown on the login button ("Sign in with …").
    pub display_name: String,
    /// Claim used as the user name of new accounts.
    pub username_claim: String,
    /// Create accounts for unknown users (otherwise an administrator links them first).
    /// Combine with `allowed_groups` unless everybody at the provider may use James.
    pub create_users: bool,
    /// Roles of accounts created on first login.
    pub default_roles: Vec<String>,
    /// Claim listing the user's groups.
    pub groups_claim: String,
    /// Only members of one of these groups (or of `admin_groups`) may sign in; empty = all.
    pub allowed_groups: Vec<String>,
    /// Groups that make an account administrator (checked on every login; empty = never set
    /// by SSO).
    pub admin_groups: Vec<String>,
    /// Accept an `http://` issuer that is not on this machine (its signing keys could be
    /// swapped on the way: trusted networks only).
    pub allow_insecure_http: bool,
}

impl Default for OidcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: None,
            redirect_url: String::new(),
            scopes: vec!["profile".into(), "email".into()],
            display_name: "SSO".into(),
            username_claim: "preferred_username".into(),
            create_users: false,
            default_roles: Vec::new(),
            groups_claim: "groups".into(),
            allowed_groups: Vec::new(),
            admin_groups: Vec::new(),
            allow_insecure_http: false,
        }
    }
}

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// A login in progress, kept in the cookie of the browser that started it.
#[derive(Serialize, Deserialize)]
struct Pending {
    state: String,
    nonce: String,
    verifier: String,
    /// Start (seconds since the epoch).
    created: u64,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The login was started in another browser, expired or was tampered with.
#[derive(Debug)]
pub struct Expired;

impl std::fmt::Display for Expired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the login was started in another browser or expired")
    }
}

impl std::error::Error for Expired {}

/// The identity the provider vouched for.
#[derive(Debug, Clone, PartialEq)]
pub struct OidcIdentity {
    /// `iss` + `sub` (unique per provider).
    pub subject: String,
    pub username: Option<String>,
    pub groups: Vec<String>,
}

pub struct Oidc {
    pub config: OidcConfig,
    http: Http,
    metadata: tokio::sync::Mutex<Option<(Client, Instant)>>,
    /// The last failed discovery (not retried for a while, the provider may be down).
    failed_at: parking_lot::Mutex<Option<Instant>>,
}

impl Oidc {
    pub fn new(config: OidcConfig) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(!config.issuer.is_empty(), "auth.oidc.issuer is required");
        anyhow::ensure!(!config.client_id.is_empty(), "auth.oidc.client_id is required");
        RedirectUrl::new(config.redirect_url.clone()).context("auth.oidc.redirect_url is not a URL")?;
        let issuer = url::Url::parse(&config.issuer).context("auth.oidc.issuer is not a URL")?;
        let local = issuer.host_str().is_some_and(is_loopback);
        anyhow::ensure!(
            issuer.scheme() == "https"
                || (issuer.scheme() == "http" && (local || config.allow_insecure_http)),
            "auth.oidc.issuer must use https (or set auth.oidc.allow_insecure_http)"
        );
        if config.create_users && config.allowed_groups.is_empty() {
            tracing::warn!(
                "single sign-on creates accounts for everybody at the identity provider; \
                 set auth.oidc.allowed_groups to limit it"
            );
        }
        let http = reqwest::Client::builder()
            // Following redirects would turn the provider URLs into an SSRF vector.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Arc::new(Self {
            config,
            http: Http(http),
            metadata: tokio::sync::Mutex::new(None),
            failed_at: parking_lot::Mutex::new(None),
        }))
    }

    /// The client built from the provider's discovery document (cached; `refresh` fetches it
    /// again, e.g. after the provider rotated its signing keys).
    async fn client(&self, refresh: bool) -> anyhow::Result<Client> {
        let mut cached = self.metadata.lock().await;
        if let Some((client, fetched)) = cached.as_ref() {
            if !refresh && fetched.elapsed() < METADATA_LIFETIME {
                return Ok(client.clone());
            }
        }
        if self
            .failed_at
            .lock()
            .is_some_and(|t| t.elapsed() < DISCOVERY_RETRY)
        {
            anyhow::bail!("the identity provider cannot be reached (retrying shortly)");
        }
        let metadata = match CoreProviderMetadata::discover_async(
            IssuerUrl::new(self.config.issuer.clone())?,
            &self.http,
        )
        .await
        {
            Ok(metadata) => metadata,
            Err(e) => {
                *self.failed_at.lock() = Some(Instant::now());
                return Err(anyhow::Error::new(e).context("the identity provider cannot be reached"));
            }
        };
        *self.failed_at.lock() = None;
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.config.client_id.clone()),
            self.config.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(RedirectUrl::new(self.config.redirect_url.clone())?);
        *cached = Some((client.clone(), Instant::now()));
        Ok(client)
    }

    /// Starts a login: returns the provider URL and the value of the login cookie.
    pub async fn begin(&self) -> anyhow::Result<(String, String)> {
        let client = self.client(false).await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge);
        for scope in &self.config.scopes {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let (url, state, nonce) = request.url();
        let pending = Pending {
            state: state.secret().clone(),
            nonce: nonce.secret().clone(),
            verifier: verifier.secret().clone(),
            created: unix_now(),
        };
        let cookie = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&pending)?);
        Ok((url.to_string(), cookie))
    }

    /// Finishes a login: checks the state (query against the login cookie), redeems the code
    /// and verifies the ID token.
    pub async fn finish(
        &self,
        code: &str,
        state: &str,
        cookie: Option<&str>,
    ) -> anyhow::Result<OidcIdentity> {
        let pending = pending_from_cookie(cookie, state).ok_or(Expired)?;
        let mut client = self.client(false).await?;
        let response = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))?
            .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
            .request_async(&self.http)
            .await
            .map_err(|e| anyhow!("the identity provider refused the login: {e}"))?;
        let id_token = response
            .id_token()
            .ok_or_else(|| anyhow!("the identity provider sent no ID token"))?;
        let nonce = Nonce::new(pending.nonce);
        if id_token.claims(&client.id_token_verifier(), &nonce).is_err() {
            // Maybe signed with a new key: verify again with fresh metadata.
            client = self.client(true).await?;
        }
        let claims = id_token
            .claims(&client.id_token_verifier(), &nonce)
            .map_err(|e| anyhow!("invalid ID token: {e}"))?;
        // The token is verified: read the remaining claims from its payload.
        let raw = raw_claims(&id_token.to_string())?;
        // The configured claim, else the usual ones (providers differ in what they send).
        let username = [
            self.config.username_claim.as_str(),
            "preferred_username",
            "email",
            "name",
        ]
        .iter()
        .find_map(|claim| raw.get(*claim).and_then(Value::as_str).filter(|v| !v.is_empty()))
        .map(str::to_owned);
        let groups = match raw.get(&self.config.groups_claim) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|g| g.as_str().map(str::to_owned))
                .collect(),
            Some(Value::String(group)) => vec![group.clone()],
            _ => Vec::new(),
        };
        Ok(OidcIdentity {
            subject: format!("{}#{}", claims.issuer().as_str(), claims.subject().as_str()),
            username,
            groups,
        })
    }

    /// Whether the identity's groups make the account an administrator (`None` = not managed).
    pub fn is_admin(&self, identity: &OidcIdentity) -> Option<bool> {
        (!self.config.admin_groups.is_empty()).then(|| {
            identity
                .groups
                .iter()
                .any(|g| self.config.admin_groups.contains(g))
        })
    }

    /// Whether the identity may sign in at all (`allowed_groups`).
    pub fn is_allowed(&self, identity: &OidcIdentity) -> bool {
        self.config.allowed_groups.is_empty()
            || identity
                .groups
                .iter()
                .any(|g| self.config.allowed_groups.contains(g) || self.config.admin_groups.contains(g))
    }
}

/// The pending login of the cookie, if it belongs to `state` and has not expired.
fn pending_from_cookie(cookie: Option<&str>, state: &str) -> Option<Pending> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cookie?)
        .ok()?;
    let pending: Pending = serde_json::from_slice(&bytes).ok()?;
    (constant_time_eq(pending.state.as_bytes(), state.as_bytes())
        && unix_now().saturating_sub(pending.created) < PENDING_LIFETIME.as_secs())
    .then_some(pending)
}

/// HTTP client for the provider (reqwest without redirects).
struct Http(reqwest::Client);

impl<'c> openidconnect::AsyncHttpClient<'c> for Http {
    type Error = reqwest::Error;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<openidconnect::HttpResponse, reqwest::Error>> + Send + 'c,
        >,
    >;

    fn call(&'c self, request: openidconnect::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let mut response = self
                .0
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .body(body)
                .send()
                .await?;
            let mut builder = http::Response::builder().status(response.status());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            // Bounded: a broken or hostile provider cannot exhaust memory (a cut response
            // fails to parse).
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if body.len() + chunk.len() > MAX_RESPONSE {
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            Ok(builder.body(body).expect("a valid response"))
        })
    }
}

fn raw_claims(token: &str) -> anyhow::Result<serde_json::Map<String, Value>> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow!("malformed ID token"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('='))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A user name for a new account from the provider's claim: allowed characters only.
pub fn username_from(claim: Option<&str>) -> String {
    let cleaned: String = claim
        .unwrap_or_default()
        .split('@')
        .next()
        .unwrap_or_default()
        .chars()
        // ASCII only: no look-alikes of other names (Cyrillic "а" for "a").
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(32)
        .collect();
    if cleaned.is_empty() {
        "user".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_cleaned() {
        assert_eq!(username_from(Some("alice@example.com")), "alice");
        assert_eq!(username_from(Some("Bob Smith!")), "BobSmith");
        assert_eq!(
            username_from(Some("\u{0430}dmin")),
            "dmin",
            "no look-alike letters"
        );
        assert_eq!(username_from(Some("")), "user");
        assert_eq!(username_from(None), "user");
        assert_eq!(username_from(Some(&"x".repeat(50))).len(), 32);
    }

    #[test]
    fn raw_claims_are_read_from_the_payload() {
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"sub":"1","groups":["a"]}"#);
        let claims = raw_claims(&format!("h.{payload}.s")).unwrap();
        assert_eq!(claims["groups"][0], "a");
        assert!(raw_claims("nodots").is_err());
    }

    fn cookie(pending: &Pending) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(pending).unwrap())
    }

    #[test]
    fn the_login_cookie_must_match_and_be_fresh() {
        let fresh = Pending {
            state: "abc".into(),
            nonce: "n".into(),
            verifier: "v".into(),
            created: unix_now(),
        };
        assert!(pending_from_cookie(Some(&cookie(&fresh)), "abc").is_some());
        assert!(
            pending_from_cookie(Some(&cookie(&fresh)), "abd").is_none(),
            "other state"
        );
        assert!(pending_from_cookie(None, "abc").is_none(), "no cookie");
        assert!(pending_from_cookie(Some("garbage"), "abc").is_none());
        let old = Pending {
            created: unix_now() - PENDING_LIFETIME.as_secs() - 1,
            ..fresh
        };
        assert!(
            pending_from_cookie(Some(&cookie(&old)), "abc").is_none(),
            "expired"
        );
    }

    fn config(issuer: &str) -> OidcConfig {
        OidcConfig {
            enabled: true,
            issuer: issuer.into(),
            client_id: "james".into(),
            redirect_url: "https://james.example.com/api/v1/auth/oidc/callback".into(),
            ..Default::default()
        }
    }

    #[test]
    fn issuers_must_use_https_unless_local() {
        assert!(Oidc::new(config("https://auth.example.com")).is_ok());
        assert!(Oidc::new(config("http://127.0.0.1:5556/dex")).is_ok());
        assert!(Oidc::new(config("http://localhost:8080")).is_ok());
        assert!(Oidc::new(config("http://auth.example.com")).is_err());
        let mut insecure = config("http://keycloak:8080/realms/main");
        insecure.allow_insecure_http = true;
        assert!(Oidc::new(insecure).is_ok());
    }

    #[test]
    fn allowed_groups_limit_who_signs_in() {
        let mut config = config("https://auth.example.com");
        config.allowed_groups = vec!["music".into()];
        config.admin_groups = vec!["admins".into()];
        let oidc = Oidc::new(config).unwrap();
        let identity = |groups: &[&str]| OidcIdentity {
            subject: "x".into(),
            username: None,
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
        };
        assert!(oidc.is_allowed(&identity(&["music"])));
        assert!(oidc.is_allowed(&identity(&["admins"])));
        assert!(!oidc.is_allowed(&identity(&["other"])));
        assert!(!oidc.is_allowed(&identity(&[])));
    }
}
