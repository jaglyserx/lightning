use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{
    FromRow,
    types::chrono::{DateTime, Utc},
};
use uuid::Uuid;

const TOKEN_PREFIX: &str = "ltn";
const SECRET_BYTES: usize = 32;

pub(crate) struct PresentedToken {
    pub(crate) hash: [u8; 32],
}

impl PresentedToken {
    pub(crate) fn parse(token: &str) -> Option<Self> {
        let secret = token.strip_prefix("ltn_")?;
        let decoded = URL_SAFE_NO_PAD.decode(secret).ok()?;
        if decoded.len() != SECRET_BYTES {
            return None;
        }

        Some(Self {
            hash: Sha256::digest(token.as_bytes()).into(),
        })
    }
}

#[derive(Debug, FromRow, Serialize)]
pub struct ApiTokenRecord {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct CreatedApiToken {
    pub record: ApiTokenRecord,
    pub token: String,
}

pub(crate) struct TokenMaterial {
    pub(crate) plaintext: String,
    pub(crate) hash: [u8; 32],
}

impl TokenMaterial {
    pub(crate) fn generate() -> Self {
        let mut secret = [0_u8; SECRET_BYTES];
        OsRng.fill_bytes(&mut secret);
        let plaintext = format!("{TOKEN_PREFIX}_{}", URL_SAFE_NO_PAD.encode(secret));
        let hash = Sha256::digest(plaintext.as_bytes()).into();
        Self { plaintext, hash }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_have_the_expected_format_and_full_entropy_secret() {
        let material = TokenMaterial::generate();

        let presented =
            PresentedToken::parse(&material.plaintext).expect("generated token should be accepted");
        assert_eq!(
            material.hash,
            <[u8; 32]>::from(Sha256::digest(material.plaintext.as_bytes()))
        );
        assert_eq!(presented.hash, material.hash);
        assert!(!material.plaintext.contains('='));
    }

    #[test]
    fn malformed_tokens_are_rejected_before_database_access() {
        for token in ["", "ltn", "ltn_short_secret", "other_12345678_secret"] {
            assert!(PresentedToken::parse(token).is_none());
        }
    }
}
