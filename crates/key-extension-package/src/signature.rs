use std::{error::Error, fmt};

use serde::Deserialize;

use crate::{Sha256Digest, digest::decode_hex, digest::decode_hex_32};

pub const CURRENT_SIGNATURE_SCHEMA: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureMetadata {
    schema_version: u16,
    algorithm: String,
    key_id: String,
    publisher: Option<String>,
    signed_content_hash: Sha256Digest,
    signature: Vec<u8>,
}

impl SignatureMetadata {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, SignatureMetadataError> {
        let source = std::str::from_utf8(bytes)
            .map_err(|_| SignatureMetadataError::Malformed("signature metadata is not UTF-8"))?;
        let raw: RawSignature = toml::from_str(source)
            .map_err(|_| SignatureMetadataError::Malformed("invalid signature metadata TOML"))?;
        if raw.schema_version != CURRENT_SIGNATURE_SCHEMA {
            return Err(SignatureMetadataError::Malformed(
                "unsupported signature metadata schema",
            ));
        }
        if !canonical_token(&raw.algorithm, 64) {
            return Err(SignatureMetadataError::Malformed(
                "signature algorithm is not canonical",
            ));
        }
        if !canonical_key_id(&raw.key_id) {
            return Err(SignatureMetadataError::Malformed(
                "signature key ID is not canonical",
            ));
        }
        if raw
            .publisher
            .as_ref()
            .is_some_and(|publisher| publisher.is_empty() || publisher.len() > 255)
        {
            return Err(SignatureMetadataError::Malformed(
                "signature publisher is invalid",
            ));
        }
        let signed_content_hash = decode_hex_32(&raw.signed_content_sha256)
            .map(Sha256Digest::from_bytes)
            .ok_or(SignatureMetadataError::Malformed(
                "signed content SHA-256 must contain 64 hexadecimal characters",
            ))?;
        let signature = decode_hex(&raw.signature_hex, 4_096)
            .filter(|signature| !signature.is_empty())
            .ok_or(SignatureMetadataError::Malformed(
                "signature must be non-empty bounded hexadecimal data",
            ))?;
        Ok(Self {
            schema_version: raw.schema_version,
            algorithm: raw.algorithm,
            key_id: raw.key_id,
            publisher: raw.publisher,
            signed_content_hash,
            signature,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    #[must_use]
    pub fn publisher(&self) -> Option<&str> {
        self.publisher.as_deref()
    }

    #[must_use]
    pub const fn signed_content_hash(&self) -> Sha256Digest {
        self.signed_content_hash
    }

    #[must_use]
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSignature {
    schema_version: u16,
    algorithm: String,
    key_id: String,
    publisher: Option<String>,
    signed_content_sha256: String,
    signature_hex: String,
}

fn canonical_token(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn canonical_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'/' | b'-' | b'_')
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureMetadataError {
    Malformed(&'static str),
}

impl fmt::Display for SignatureMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(message) => formatter.write_str(message),
        }
    }
}

impl Error for SignatureMetadataError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedSigner {
    pub key_id: String,
    pub identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignatureVerificationError {
    UnsupportedAlgorithm(String),
    UnknownKey(String),
    InvalidSignature,
    PolicyDenied(String),
}

impl fmt::Display for SignatureVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAlgorithm(algorithm) => {
                write!(formatter, "unsupported signature algorithm {algorithm}")
            }
            Self::UnknownKey(key) => write!(formatter, "signature key {key} is not trusted"),
            Self::InvalidSignature => formatter.write_str("package signature is invalid"),
            Self::PolicyDenied(message) => formatter.write_str(message),
        }
    }
}

impl Error for SignatureVerificationError {}

/// Cryptographic implementations live behind this narrow interface so product
/// builds can inject their trust store without package loading gaining network
/// or operating-system authority.
pub trait SignatureVerifier: Send + Sync {
    /// Verify `metadata.signature()` over `content_hash.as_bytes()` and return
    /// the trusted signer identity.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported algorithms, untrusted keys, invalid
    /// signatures, revoked identities, or any store-policy rejection.
    fn verify(
        &self,
        metadata: &SignatureMetadata,
        content_hash: &Sha256Digest,
    ) -> Result<VerifiedSigner, SignatureVerificationError>;
}

/// Default verifier used by callers that have not configured a trust store.
/// It intentionally accepts nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllSignatureVerifier;

impl SignatureVerifier for DenyAllSignatureVerifier {
    fn verify(
        &self,
        metadata: &SignatureMetadata,
        _content_hash: &Sha256Digest,
    ) -> Result<VerifiedSigner, SignatureVerificationError> {
        Err(SignatureVerificationError::UnknownKey(
            metadata.key_id().to_owned(),
        ))
    }
}
