//! Access-token verification for the WebSocket handshake.
//!
//! The gateway used to read the user id straight out of the query string:
//! `?token=user_123` was parsed with `strip_prefix("user_")` and anything else
//! became `0`. No signature was checked, so every client could claim any user
//! id and subscribe to `user_bets:<id>` / `user_balance:<id>` of somebody else —
//! `is_topic_visible_to` only compared the forged id with itself.
//!
//! Tokens are now verified the same way `betting-engine` verifies its REST
//! calls: EdDSA (Ed25519), issuer pinned to the Go auth service, `token_type`
//! pinned to `access`, and the user id taken from the verified `sub` claim.
//!
//! A browser cannot put an `Authorization` header on a WebSocket handshake, so
//! the token is accepted from three places, in order: the `Authorization`
//! header (native clients), the `token` query parameter (kept for clients that
//! cannot send cookies), and the `access_token` HttpOnly cookie, which the
//! browser attaches on its own when the socket is opened for the same origin.

use std::sync::Arc;

use axum::http::{header, HeaderMap};
use base64::Engine as _;
use jwt_simple::{
    algorithms::{Ed25519PublicKey, EdDSAPublicKeyLike},
    common::VerificationOptions,
};
use serde::{Deserialize, Serialize};

/// Issuer every token of the Go auth service carries.
pub const TOKEN_ISSUER: &str = "opus-casino-auth";

/// Cookie the web app receives on login (`services/go/auth/cookies.go`).
pub const ACCESS_COOKIE_NAME: &str = "access_token";

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("access-token verification is not configured")]
    NotConfigured,
    #[error("missing access token")]
    Missing,
    #[error("invalid access token")]
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CustomClaims {
    #[serde(default)]
    token_type: String,
}

/// Verified identity of a WebSocket client.
#[derive(Debug, Clone)]
pub struct WsUser {
    pub user_id: i64,
}

/// Holds the Ed25519 public key used to verify access tokens.
#[derive(Clone)]
pub struct TokenVerifier {
    key: Arc<Ed25519PublicKey>,
}

impl std::fmt::Debug for TokenVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenVerifier").finish_non_exhaustive()
    }
}

impl TokenVerifier {
    /// Build a verifier from the base64 Ed25519 public key of the auth service.
    ///
    /// Returns `NotConfigured` for an empty value and `Invalid` when the value
    /// is not a 32-byte Ed25519 key, so the gateway can fail closed instead of
    /// accepting every handshake.
    pub fn from_base64(encoded: &str) -> Result<Self, TokenError> {
        let encoded = encoded.trim();
        if encoded.is_empty() {
            return Err(TokenError::NotConfigured);
        }

        // The key is documented as standard base64, but an operator may paste
        // the URL-safe form; accept both rather than silently refusing to boot.
        let raw = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .or_else(|_| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(encoded.trim_end_matches('='))
            })
            .map_err(|_| TokenError::Invalid)?;

        let key = Ed25519PublicKey::from_bytes(&raw).map_err(|_| TokenError::Invalid)?;
        Ok(Self { key: Arc::new(key) })
    }

    /// Verify an access token and return the claims the gateway relies on.
    pub fn verify(&self, token: &str) -> Result<WsUser, TokenError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(TokenError::Missing);
        }

        let options = VerificationOptions {
            allowed_issuers: Some([TOKEN_ISSUER.to_string()].into_iter().collect()),
            ..Default::default()
        };

        let claims = self
            .key
            .verify_token::<CustomClaims>(token, Some(options))
            .map_err(|_| TokenError::Invalid)?;

        // A refresh token is long-lived and must never open a socket.
        if claims.custom.token_type != "access" {
            return Err(TokenError::Invalid);
        }

        // The Go auth service sets `sub` to the numeric user id.
        let user_id = claims
            .subject
            .as_deref()
            .ok_or(TokenError::Invalid)?
            .parse::<i64>()
            .map_err(|_| TokenError::Invalid)?;

        Ok(WsUser { user_id })
    }
}

/// Extract the raw access token from the handshake.
///
/// Order matters: an explicit `Authorization` header always wins, then the
/// query parameter (clients without cookie support), then the session cookie.
pub fn token_from_handshake(headers: &HeaderMap, query_token: Option<&str>) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some((scheme, token)) = value.split_once(' ') {
            if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }

    if let Some(token) = query_token {
        if !token.trim().is_empty() {
            return Some(token.trim().to_string());
        }
    }

    cookie_value(headers, ACCESS_COOKIE_NAME)
}

/// Read one cookie out of the `Cookie` header without pulling in a cookie crate
/// for a single lookup.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jwt_simple::algorithms::{Ed25519KeyPair, EdDSAKeyPairLike};
    use jwt_simple::claims::Claims;
    use jwt_simple::prelude::Duration as JwtDuration;

    fn key_pair() -> Ed25519KeyPair {
        Ed25519KeyPair::generate()
    }

    fn public_key_b64(pair: &Ed25519KeyPair) -> String {
        base64::engine::general_purpose::STANDARD.encode(pair.public_key().to_bytes())
    }

    fn sign(pair: &Ed25519KeyPair, subject: &str, token_type: &str) -> String {
        let claims = Claims::with_custom_claims(
            CustomClaims {
                token_type: token_type.into(),
            },
            JwtDuration::from_secs(900),
        )
        .with_issuer(TOKEN_ISSUER)
        .with_subject(subject);
        pair.sign(claims).expect("token signs")
    }

    #[test]
    fn accepts_a_token_signed_by_the_auth_service() {
        let pair = key_pair();
        let verifier = TokenVerifier::from_base64(&public_key_b64(&pair)).expect("verifier");

        let user = verifier
            .verify(&sign(&pair, "42", "access"))
            .expect("valid token");
        assert_eq!(user.user_id, 42);
    }

    #[test]
    fn rejects_tokens_from_another_key() {
        let issuer = key_pair();
        let attacker = key_pair();
        let verifier = TokenVerifier::from_base64(&public_key_b64(&issuer)).expect("verifier");

        assert!(matches!(
            verifier.verify(&sign(&attacker, "42", "access")),
            Err(TokenError::Invalid)
        ));
    }

    #[test]
    fn rejects_refresh_tokens() {
        let pair = key_pair();
        let verifier = TokenVerifier::from_base64(&public_key_b64(&pair)).expect("verifier");

        assert!(matches!(
            verifier.verify(&sign(&pair, "42", "refresh")),
            Err(TokenError::Invalid)
        ));
    }

    #[test]
    fn rejects_the_legacy_user_prefix_format() {
        let pair = key_pair();
        let verifier = TokenVerifier::from_base64(&public_key_b64(&pair)).expect("verifier");

        // The old handler accepted this shape and trusted it.
        assert!(verifier.verify("user_42").is_err());
    }

    #[test]
    fn an_empty_key_is_not_configured_rather_than_permissive() {
        assert!(matches!(
            TokenVerifier::from_base64("   "),
            Err(TokenError::NotConfigured)
        ));
        assert!(matches!(
            TokenVerifier::from_base64("not-base64!!"),
            Err(TokenError::Invalid)
        ));
    }

    #[test]
    fn reads_the_token_from_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "theme=dark; access_token=abc.def.ghi; other=1".parse().unwrap(),
        );

        assert_eq!(
            token_from_handshake(&headers, None).as_deref(),
            Some("abc.def.ghi")
        );
    }

    #[test]
    fn the_authorization_header_wins_over_the_query_string() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer from-header".parse().unwrap());

        assert_eq!(
            token_from_handshake(&headers, Some("from-query")).as_deref(),
            Some("from-header")
        );
    }
}
