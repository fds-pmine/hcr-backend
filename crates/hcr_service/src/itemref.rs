//! Signed item references.
//!
//! arona addresses items by `Vec` index and `Question` carries no identifier
//! (`arona/src/core/question.rs:159-165`), so the mapping from "the item I just
//! served" to "an item you can name" lives outside the library. That mapping has
//! to survive a round trip through an untrusted client.
//!
//! An `itemRef` is that mapping, sealed: the bank index, item identity and
//! version travel to the browser and back, and an HMAC makes them unforgeable.
//! Without the signature a client could claim any index and score against an item
//! it was never served.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::error::ServiceError;

type HmacSha256 = Hmac<Sha256>;

/// What a token asserts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRefClaims {
    /// Session the item was issued to.
    pub session_id: String,
    /// arona bank index, resolvable through `HcrDynamicBank::item_at`.
    pub bank_index: usize,
    /// Item identity.
    pub item_id: String,
    /// Item version, pinned at issue so recalibration cannot move it.
    pub challenge_version: u32,
    /// Issue time, epoch milliseconds.
    pub issued_at: u64,
}

/// Mints and verifies item references.
#[derive(Clone)]
pub struct ItemRefSigner {
    key: Vec<u8>,
}

impl std::fmt::Debug for ItemRefSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the key, even accidentally through a derived Debug on
        // some enclosing struct.
        f.debug_struct("ItemRefSigner")
            .field("key", &"<redacted>")
            .finish()
    }
}

impl ItemRefSigner {
    /// Build a signer from a secret.
    pub fn new(key: impl Into<Vec<u8>>) -> Self {
        Self { key: key.into() }
    }

    /// Serialize and sign claims into `payload.signature`.
    pub fn sign(&self, claims: &ItemRefClaims) -> Result<String, ServiceError> {
        let payload = serde_json::to_vec(claims)
            .map_err(|_| ServiceError::Internal("failed to encode item reference"))?;
        let encoded = BASE64.encode(&payload);
        let signature = self.mac(encoded.as_bytes());
        Ok(format!("{encoded}.{}", BASE64.encode(signature)))
    }

    /// Verify a token and return its claims.
    ///
    /// Only proves the token was minted here and is intact. The caller must still
    /// check it belongs to the right session and matches the item the bank
    /// actually last served — a valid token for a *different* item is still an
    /// invalid response.
    pub fn verify(&self, token: &str) -> Result<ItemRefClaims, ServiceError> {
        let (encoded, signature) = token
            .split_once('.')
            .ok_or(ServiceError::ItemRefInvalid("malformed token"))?;

        let expected = BASE64
            .decode(signature)
            .map_err(|_| ServiceError::ItemRefInvalid("malformed signature"))?;

        let mut mac = HmacSha256::new_from_slice(&self.key)
            .map_err(|_| ServiceError::Internal("invalid signing key"))?;
        mac.update(encoded.as_bytes());
        // Constant-time comparison; a byte-wise `==` here would leak the
        // signature one byte at a time under timing analysis.
        mac.verify_slice(&expected)
            .map_err(|_| ServiceError::ItemRefInvalid("signature mismatch"))?;

        let payload = BASE64
            .decode(encoded)
            .map_err(|_| ServiceError::ItemRefInvalid("malformed payload"))?;
        serde_json::from_slice(&payload)
            .map_err(|_| ServiceError::ItemRefInvalid("unreadable claims"))
    }

    fn mac(&self, message: &[u8]) -> Vec<u8> {
        let mut mac =
            HmacSha256::new_from_slice(&self.key).expect("HMAC accepts keys of any length");
        mac.update(message);
        mac.finalize().into_bytes().to_vec()
    }
}
