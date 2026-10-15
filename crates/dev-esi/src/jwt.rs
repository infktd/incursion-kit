//! RS256 JSON Web Tokens: dev-esi's signing key, its JWKS and token signing/verification.
//!
//! Access tokens look like EVE SSO's (same header and claims) and are signed like them (RS256,
//! `kid` `JWT-Signature-Key`), so a server that verifies tokens against a configurable JWKS URL
//! can be pointed at `/oauth/jwks`. Keeping the key in a file (`--state-dir`) keeps issued access
//! tokens valid across dev-esi restarts.

use std::path::Path;

use aws_lc_rs::encoding::AsDer as _;
use aws_lc_rs::rsa::{KeyPair, KeySize, PublicKeyComponents};
use aws_lc_rs::signature::{
    KeyPair as _, RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256, UnparsedPublicKey,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

/// The key ID EVE SSO uses for its RS256 key.
pub const KEY_ID: &str = "JWT-Signature-Key";

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("could not generate a signing key")]
    Generate,
    #[error("{path} is not a PKCS#8 RSA private key: {message}")]
    Invalid { path: String, message: String },
    #[error("could not access the key file {path}: {error}")]
    Io { path: String, error: std::io::Error },
}

/// The signing key.
pub struct Signer {
    key: KeyPair,
    public_der: Vec<u8>,
    jwk: Value,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer").finish_non_exhaustive()
    }
}

impl Signer {
    /// A fresh 2048-bit key (tokens die with the process).
    pub fn generate() -> Result<Self, KeyError> {
        Self::from_key(KeyPair::generate(KeySize::Rsa2048).map_err(|_| KeyError::Generate)?)
    }

    /// The key in `path` (PKCS#8 DER), created there if the file doesn't exist.
    pub fn load_or_create(path: &Path) -> Result<Self, KeyError> {
        let io = |error| KeyError::Io {
            path: path.display().to_string(),
            error,
        };
        match std::fs::read(path) {
            Ok(der) => {
                let key = KeyPair::from_pkcs8(&der).map_err(|e| KeyError::Invalid {
                    path: path.display().to_string(),
                    message: e.to_string(),
                })?;
                Self::from_key(key)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = KeyPair::generate(KeySize::Rsa2048).map_err(|_| KeyError::Generate)?;
                let der = key.as_der().map_err(|_| KeyError::Generate)?;
                if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir).map_err(io)?;
                }
                std::fs::write(path, der.as_ref()).map_err(io)?;
                Self::from_key(key)
            }
            Err(e) => Err(io(e)),
        }
    }

    fn from_key(key: KeyPair) -> Result<Self, KeyError> {
        let public = key.public_key();
        let components = PublicKeyComponents::<Vec<u8>>::from(public);
        let jwk = json!({
            "alg": "RS256",
            "e": URL_SAFE_NO_PAD.encode(&components.e),
            "kid": KEY_ID,
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(&components.n),
            "use": "sig",
        });
        Ok(Self {
            public_der: public.as_ref().to_vec(),
            key,
            jwk,
        })
    }

    /// The JSON Web Key Set (`GET /oauth/jwks`), shaped like EVE SSO's.
    pub fn jwks(&self) -> Value {
        json!({ "keys": [self.jwk], "SkipUnresolvedJsonWebKeys": true })
    }

    /// A signed JWT with these claims.
    pub fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg": "RS256", "kid": KEY_ID, "typ": "JWT"});
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0; self.key.public_modulus_len()];
        let rng = aws_lc_rs::rand::SystemRandom::new();
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &rng,
                signing_input.as_bytes(),
                &mut signature,
            )
            .expect("RS256 signing with a valid key and a correctly sized buffer");
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    /// The claims of a token this key signed (`None` if it isn't one). Expiry is the caller's
    /// business.
    pub fn verify(&self, token: &str) -> Option<Value> {
        let mut parts = token.split('.');
        let (header, claims, signature) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
        if header.get("alg")? != "RS256" {
            return None;
        }
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        let signing_input = &token[..token.rfind('.')?];
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &self.public_der)
            .verify(signing_input.as_bytes(), &signature)
            .ok()?;
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).ok()?).ok()
    }
}

/// A random URL-safe string with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    for byte in &mut buf {
        *byte = rand::random();
    }
    URL_SAFE_NO_PAD.encode(buf)
}

/// `BASE64URL(SHA256(verifier))`: the PKCE `S256` challenge of a verifier.
pub fn pkce_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(aws_lc_rs::digest::digest(
        &aws_lc_rs::digest::SHA256,
        verifier.as_bytes(),
    ))
}

/// A stable, owner-hash-looking value for a character (28 characters of base64, like EVE's).
pub fn default_owner_hash(character_id: i64) -> String {
    let digest = aws_lc_rs::digest::digest(
        &aws_lc_rs::digest::SHA256,
        format!("dev-esi owner {character_id}").as_bytes(),
    );
    base64::engine::general_purpose::STANDARD.encode(&digest.as_ref()[..20])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_verifies() {
        let signer = Signer::generate().unwrap();
        let token = signer.sign(&json!({"sub": "CHARACTER:EVE:1", "scp": ["a", "b"]}));
        let claims = signer.verify(&token).unwrap();
        assert_eq!(claims["sub"], "CHARACTER:EVE:1");

        // Tampering with the claims breaks the signature.
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(json!({"sub": "CHARACTER:EVE:2"}).to_string());
        parts[1] = &forged;
        assert_eq!(signer.verify(&parts.join(".")), None);
        // Another key's tokens are not ours.
        assert_eq!(Signer::generate().unwrap().verify(&token), None);
        assert_eq!(signer.verify("not.a.jwt"), None);

        let jwks = signer.jwks();
        assert_eq!(jwks["keys"][0]["kid"], KEY_ID);
        assert_eq!(jwks["keys"][0]["e"], "AQAB");
    }

    #[test]
    fn keys_persist_in_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys/dev-esi.key");
        let first = Signer::load_or_create(&path).unwrap();
        let token = first.sign(&json!({"sub": "x"}));
        let second = Signer::load_or_create(&path).unwrap();
        assert!(second.verify(&token).is_some());

        std::fs::write(&path, b"garbage").unwrap();
        assert!(matches!(
            Signer::load_or_create(&path),
            Err(KeyError::Invalid { .. })
        ));
    }

    #[test]
    fn pkce_challenges_and_owner_hashes() {
        // base64url(sha256(verifier)), computed independently.
        assert_eq!(
            pkce_s256("dBjftJeZ4CVP-mB92K27uhbUJU2p1r0bf4R7fNlvEuQ"),
            "C3rn3V34UohL5cUfqq5dHWKvI7qbAed9XN_EK_LiGLI"
        );
        assert_eq!(default_owner_hash(1).len(), 28);
        assert_ne!(default_owner_hash(1), default_owner_hash(2));
    }
}
