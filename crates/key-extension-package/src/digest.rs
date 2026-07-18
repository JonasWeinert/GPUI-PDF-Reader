use std::fmt;

use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
        }
        output
    }

    pub(crate) fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

pub(crate) fn canonical_content_hash<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(b"KEYEXT-CONTENT-V1\0");
    for (path, bytes) in files {
        let path_len = u64::try_from(path.len()).expect("package path length is bounded");
        let data_len = u64::try_from(bytes.len()).expect("package file length is bounded");
        hasher.update(path_len.to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update(data_len.to_be_bytes());
        hasher.update(bytes);
    }
    Sha256Digest(hasher.finalize().into())
}

pub(crate) fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut output = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(output)
}

pub(crate) fn decode_hex(value: &str, maximum_bytes: usize) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || value.len() / 2 > maximum_bytes {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Some((hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?))
        .collect()
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
