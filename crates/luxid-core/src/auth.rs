//! Authentication: password hashing, tokens, and the request's identity.
//!
//! `Auth` carries *who the request is*, not the user record. Loading a model
//! from that identity belongs to the Lucid layer, so `ctx.auth.user()` arrives
//! with the ORM; everything a token-based API needs is here today.

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::context::HttpContext;
use crate::error::{Error, Result};
use crate::http::{Cookie, Response};
use crate::middleware::{BoxFuture, Middleware, Next};
use crate::session::{Session, SessionData, SessionStore, new_id};

/// Who the current request is, plus whatever claims came with them.
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    subject: String,
    claims: Map<String, Value>,
}

impl Identity {
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            claims: Map::new(),
        }
    }

    /// Attach a claim. Reserved names (`sub`, `exp`, `iat`) are set by the
    /// signer and cannot be overridden here.
    #[must_use]
    pub fn with_claim(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        let name = name.into();

        if !matches!(name.as_str(), "sub" | "exp" | "iat") {
            self.claims.insert(name, value.into());
        }
        self
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// The subject parsed into the app's id type.
    pub fn id<T: FromStr>(&self) -> Result<T> {
        self.subject.parse().map_err(|_| {
            Error::internal(format!(
                "identity subject `{}` is not the expected id type",
                self.subject
            ))
        })
    }

    pub fn claim<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        match self.claims.get(name) {
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|err| Error::internal(format!("claim `{name}`: {err}"))),
            None => Ok(None),
        }
    }

    pub fn claims(&self) -> &Map<String, Value> {
        &self.claims
    }
}

/// The authentication state of the current request.
///
/// Present on every context, authenticated or not.
#[derive(Debug, Default, Clone)]
pub struct Auth {
    identity: Option<Identity>,
}

impl Auth {
    /// Middleware that requires a valid bearer token.
    ///
    /// Resolves [`Jwt`] from the container, so the signing key is configured
    /// once in `providers()`.
    pub fn jwt() -> JwtGuard {
        JwtGuard { required: true }
    }

    /// As [`Auth::jwt`], but allows anonymous requests through. Useful for
    /// endpoints that render differently when signed in.
    pub fn optional_jwt() -> JwtGuard {
        JwtGuard { required: false }
    }

    /// Cookie-backed sessions.
    ///
    /// Loads the session, exposes it as `ctx.session`, sets `ctx.auth` from the
    /// session's subject, and persists any changes afterwards. Anonymous
    /// requests pass through — a session is how a user *becomes*
    /// authenticated, so refusing them would leave nowhere to log in.
    ///
    /// Requires a `SessionStore` bound in `providers()`.
    pub fn session() -> SessionGuard {
        SessionGuard {
            ttl: SessionGuard::DEFAULT_TTL,
            cookie: SessionGuard::DEFAULT_COOKIE,
            secure: false,
        }
    }

    pub fn check(&self) -> bool {
        self.identity.is_some()
    }

    /// The identity, or a 401.
    pub fn identity(&self) -> Result<&Identity> {
        self.identity.as_ref().ok_or(Error::Unauthorized)
    }

    pub fn try_identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// The authenticated subject parsed into the app's id type, or a 401.
    pub fn id<T: FromStr>(&self) -> Result<T> {
        self.identity()?.id()
    }

    pub fn set(&mut self, identity: Identity) {
        self.identity = Some(identity);
    }

    pub fn forget(&mut self) {
        self.identity = None;
    }
}

/// Argon2id password hashing.
pub struct Hash;

impl Hash {
    /// Hash a password with a fresh random salt.
    pub fn make(password: &str) -> Result<String> {
        let salt = SaltString::generate(&mut OsRng);

        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|err| Error::internal(format!("could not hash password: {err}")))
    }

    /// Verify a password against a stored hash.
    ///
    /// A malformed stored hash is a failed verification, not an error, so a
    /// corrupt row cannot be distinguished from a wrong password by timing or
    /// by response.
    pub fn verify(password: &str, hash: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(hash) else {
            return false;
        };

        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
    iat: u64,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// HS256 token signing and verification.
///
/// Register once in `providers()`; [`Auth::jwt`] resolves it from the
/// container.
pub struct Jwt {
    encoding: jsonwebtoken::EncodingKey,
    decoding: jsonwebtoken::DecodingKey,
    ttl: Duration,
}

impl std::fmt::Debug for Jwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        f.debug_struct("Jwt")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl Jwt {
    /// Default lifetime for issued tokens.
    pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);

    pub fn new(secret: impl AsRef<[u8]>) -> Self {
        Self {
            encoding: jsonwebtoken::EncodingKey::from_secret(secret.as_ref()),
            decoding: jsonwebtoken::DecodingKey::from_secret(secret.as_ref()),
            ttl: Self::DEFAULT_TTL,
        }
    }

    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Issue a token that expires after the configured TTL.
    pub fn sign(&self, identity: &Identity) -> Result<String> {
        self.sign_expiring_at(identity, now() + self.ttl.as_secs())
    }

    /// Issue a token with an explicit expiry, for short-lived links and the
    /// like.
    pub fn sign_expiring_at(&self, identity: &Identity, expires_at: u64) -> Result<String> {
        let claims = Claims {
            sub: identity.subject.clone(),
            exp: expires_at,
            iat: now(),
            extra: identity.claims.clone(),
        };

        jsonwebtoken::encode(&jsonwebtoken::Header::default(), &claims, &self.encoding)
            .map_err(|err| Error::internal(format!("could not sign token: {err}")))
    }

    /// Verify a token. Any failure — bad signature, expiry, malformed — is a
    /// 401 and never explains which, so tokens cannot be probed.
    pub fn verify(&self, token: &str) -> Result<Identity> {
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.leeway = 0;

        let data = jsonwebtoken::decode::<Claims>(token, &self.decoding, &validation)
            .map_err(|_| Error::Unauthorized)?;

        Ok(Identity {
            subject: data.claims.sub,
            claims: data.claims.extra,
        })
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Bearer-token guard produced by [`Auth::jwt`] / [`Auth::optional_jwt`].
pub struct JwtGuard {
    required: bool,
}

impl Middleware for JwtGuard {
    fn handle<'a>(&'a self, mut ctx: HttpContext, next: Next) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            // `bearer_token` borrows `ctx.request`; `ctx.auth` is a different
            // field, so both borrows coexist and the token needs no copy. That
            // is a per-request allocation avoided on every authenticated route.
            match ctx.request.bearer_token() {
                Some(token) => {
                    let jwt: Arc<Jwt> = ctx.services.get::<Jwt>()?;
                    let identity = jwt.verify(token)?;

                    ctx.auth.set(identity);
                }
                None if self.required => return Err(Error::Unauthorized),
                None => {}
            }

            next.run(ctx).await
        })
    }
}

/// Cookie-backed session middleware, produced by [`Auth::session`].
pub struct SessionGuard {
    ttl: Duration,
    cookie: &'static str,
    secure: bool,
}

impl SessionGuard {
    pub const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 14);
    pub const DEFAULT_COOKIE: &'static str = "luxid_session";

    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    #[must_use]
    pub fn cookie(mut self, name: &'static str) -> Self {
        self.cookie = name;
        self
    }

    /// Send the cookie only over HTTPS. Turn this on in production.
    #[must_use]
    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }
}

impl Middleware for SessionGuard {
    fn handle<'a>(&'a self, mut ctx: HttpContext, next: Next) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            let store = ctx.services.get_dyn::<dyn SessionStore>().map_err(|_| {
                Error::internal(
                    "sessions need a store. Bind one in `providers()`, e.g. \
                     `.bind::<dyn SessionStore, _>(|_| Arc::new(MemoryStore::new()))`.",
                )
            })?;

            let presented = ctx.request.cookie(self.cookie).map(str::to_owned);

            // An unknown or expired id starts a fresh session rather than
            // failing: a stale cookie is ordinary, not an error.
            let (id, data) = match presented.as_deref() {
                Some(id) => match store.load(id).await? {
                    Some(data) => (id.to_owned(), data),
                    None => (new_id(), SessionData::new()),
                },
                None => (new_id(), SessionData::new()),
            };

            let session = Session::attached(id, data);

            if let Some(subject) = session.subject() {
                ctx.auth.set(Identity::new(subject));
            }

            // The middleware keeps a handle so it can read back whatever the
            // action did, after the context has been consumed.
            let handle = session.clone();
            ctx.session = session;

            let response = next.run(ctx).await?;

            let (id, data, dirty, destroyed) = handle.snapshot();

            if destroyed {
                store.destroy(&id).await?;

                return Ok(response.cookie(Cookie::removal(self.cookie)));
            }

            let rotated = presented.as_deref() != Some(id.as_str());

            if dirty {
                store.save(&id, data, self.ttl).await?;

                // The old entry is dead once the id rotates; leaving it would
                // keep a logged-in session alive under the previous cookie.
                if let Some(previous) = presented.as_deref()
                    && previous != id
                {
                    store.destroy(previous).await?;
                }
            }

            if rotated {
                return Ok(response.cookie(
                    Cookie::new(self.cookie, id)
                        .max_age(self.ttl.as_secs() as i64)
                        .secure(self.secure),
                ));
            }

            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify_against_the_original_password() {
        let hash = Hash::make("correct horse battery staple").expect("hashes");

        assert!(Hash::verify("correct horse battery staple", &hash));
        assert!(!Hash::verify("wrong password", &hash));
    }

    #[test]
    fn the_same_password_hashes_differently_each_time() {
        let first = Hash::make("secret").expect("hashes");
        let second = Hash::make("secret").expect("hashes");

        assert_ne!(first, second, "a fresh salt is used per hash");
        assert!(Hash::verify("secret", &first));
        assert!(Hash::verify("secret", &second));
    }

    #[test]
    fn a_corrupt_stored_hash_fails_verification_rather_than_erroring() {
        assert!(!Hash::verify("secret", "not-a-phc-string"));
        assert!(!Hash::verify("secret", ""));
    }

    #[test]
    fn tokens_round_trip_subject_and_claims() {
        let jwt = Jwt::new("test-secret");
        let identity = Identity::new("42")
            .with_claim("role", "admin")
            .with_claim("team", 7);

        let token = jwt.sign(&identity).expect("signs");
        let decoded = jwt.verify(&token).expect("verifies");

        assert_eq!(decoded.subject(), "42");
        assert_eq!(decoded.id::<i64>().expect("parses"), 42);
        assert_eq!(
            decoded.claim::<String>("role").expect("claim"),
            Some("admin".into())
        );
        assert_eq!(decoded.claim::<i64>("team").expect("claim"), Some(7));
        assert_eq!(decoded.claim::<String>("absent").expect("claim"), None);
    }

    #[test]
    fn reserved_claims_cannot_be_overridden() {
        let jwt = Jwt::new("test-secret");
        let identity = Identity::new("1")
            .with_claim("sub", "999")
            .with_claim("exp", 0);

        let decoded = jwt
            .verify(&jwt.sign(&identity).expect("signs"))
            .expect("verifies");

        assert_eq!(
            decoded.subject(),
            "1",
            "sub comes from the identity, not a claim"
        );
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let jwt = Jwt::new("test-secret");
        let identity = Identity::new("1");

        let token = jwt.sign_expiring_at(&identity, now() - 60).expect("signs");

        assert_eq!(jwt.verify(&token).unwrap_err().status_code().as_u16(), 401);
    }

    #[test]
    fn a_token_signed_with_another_secret_is_rejected() {
        let issuer = Jwt::new("secret-a");
        let verifier = Jwt::new("secret-b");

        let token = issuer.sign(&Identity::new("1")).expect("signs");

        assert_eq!(
            verifier.verify(&token).unwrap_err().status_code().as_u16(),
            401
        );
    }

    #[test]
    fn a_tampered_token_is_rejected() {
        let jwt = Jwt::new("test-secret");
        let token = jwt.sign(&Identity::new("1")).expect("signs");

        let mut tampered = token.clone();
        tampered.push('x');

        assert!(jwt.verify(&tampered).is_err());
    }

    #[test]
    fn verification_failures_do_not_explain_themselves() {
        let jwt = Jwt::new("test-secret");
        let expired = jwt
            .sign_expiring_at(&Identity::new("1"), now() - 60)
            .expect("signs");
        let forged = Jwt::new("other").sign(&Identity::new("1")).expect("signs");

        // Both render identically, so a caller cannot probe which failed.
        assert_eq!(
            jwt.verify(&expired).unwrap_err().problem(),
            jwt.verify(&forged).unwrap_err().problem()
        );
    }

    #[test]
    fn an_anonymous_auth_reports_unauthorized() {
        let auth = Auth::default();

        assert!(!auth.check());
        assert!(auth.try_identity().is_none());
        assert_eq!(auth.identity().unwrap_err().status_code().as_u16(), 401);
        assert_eq!(auth.id::<i64>().unwrap_err().status_code().as_u16(), 401);
    }

    #[test]
    fn setting_and_forgetting_an_identity() {
        let mut auth = Auth::default();
        auth.set(Identity::new("7"));

        assert!(auth.check());
        assert_eq!(auth.id::<i64>().expect("id"), 7);

        auth.forget();
        assert!(!auth.check());
    }

    #[test]
    fn the_debug_output_never_leaks_key_material() {
        let rendered = format!("{:?}", Jwt::new("super-secret-value"));
        assert!(!rendered.contains("super-secret"), "{rendered}");
    }
}
