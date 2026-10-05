//! Bearer tokens (#222, #335): the session cookie, the invitation link, the
//! password reset link and the email verification link each carry one.
//! A token is 32 bytes from the OS CSPRNG, handed out in unpadded base64url;
//! the database keeps only its SHA-256 (`sessions.token_hash`,
//! `invitations.token_hash`, `password_reset_tokens.token_hash`,
//! `email_verification_tokens.token_hash`). A copy of the database therefore
//! opens nothing. The hash is computed here, never in SQL, so the token
//! itself does not travel in a statement or in its logged parameters.
//!
//! A fast hash is enough: the token has 256 bits of entropy, nothing to
//! brute-force, and a slow hash would be paid on every request.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Raw bytes of a bearer token.
const TOKEN_BYTES: usize = 32;

/// A fresh bearer token: the value handed to its holder, and the hash the
/// database keeps. Neither `Debug` nor `Display`, so the value does not end
/// up in a log by accident.
pub struct BearerToken {
    value: String,
    hash: [u8; 32],
}

impl BearerToken {
    /// What the holder gets: [`TOKEN_BYTES`] random bytes, unpadded
    /// base64url (43 characters).
    pub fn value(&self) -> &str {
        &self.value
    }

    /// What the database keeps: [`token_hash`] of [`BearerToken::value`].
    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
}

pub fn new_token() -> BearerToken {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    BearerToken {
        value: URL_SAFE_NO_PAD.encode(bytes),
        hash: Sha256::digest(bytes).into(),
    }
}

/// The hash the database keeps for a token as its holder presents it: the
/// SHA-256 of the token's raw bytes. `None` for anything that is not a token
/// as [`new_token`] spells it — exactly [`TOKEN_BYTES`] bytes in canonical
/// unpadded base64url — so a malformed one is refused without a query.
pub fn token_hash(value: &str) -> Option<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD.decode(value).ok()?;
    if bytes.len() != TOKEN_BYTES {
        return None;
    }
    Some(Sha256::digest(bytes).into())
}

/// A hash in lowercase hex, the spelling `encode(token_hash, 'hex')` gives
/// in SQL. `app.invitation_token` carries the hash this way (#335): a
/// setting is text, and the `invitations` policy compares it to the hex of
/// the row's hash, never to the token.
pub fn hash_hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_token_is_32_random_bytes_in_unpadded_base64url() {
        let token = new_token();
        assert_eq!(token.value().len(), 43, "{}", token.value());
        assert!(token
            .value()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    #[test]
    fn a_new_token_carries_the_hash_of_its_own_value() {
        let token = new_token();
        assert_eq!(token_hash(token.value()), Some(token.hash()));
    }

    #[test]
    fn two_new_tokens_differ() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a.value(), b.value());
        assert_ne!(a.hash(), b.hash());
    }

    /// The hash is SHA-256 of the 32 decoded bytes: 43 `A`s are 32 zero
    /// bytes, whose SHA-256 is the published constant below.
    #[test]
    fn the_hash_is_sha256_of_the_decoded_bytes() {
        let zeros = "A".repeat(43);
        let expected = "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925";
        let hash = token_hash(&zeros).expect("a well-formed token");
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, expected);
    }

    /// Whatever is not a token as `new_token` spells it is refused before
    /// any query: the former format (a UUID), a hash in hex, padded or
    /// standard base64, a token one character short or long, and a
    /// non-canonical spelling of a valid one (the last character's spare
    /// bits set).
    #[test]
    fn anything_but_a_well_formed_token_has_no_hash() {
        let uuid = uuid::Uuid::new_v4().to_string();
        let hex = "66".repeat(32);
        let padded = format!("{}=", "A".repeat(43));
        let standard = format!("{}+A", "A".repeat(41));
        let short = "A".repeat(42);
        let long = "A".repeat(44);
        let non_canonical = format!("{}B", "A".repeat(42));
        for value in [
            "",
            uuid.as_str(),
            hex.as_str(),
            padded.as_str(),
            standard.as_str(),
            short.as_str(),
            long.as_str(),
            non_canonical.as_str(),
        ] {
            assert_eq!(token_hash(value), None, "{value:?}");
        }
    }

    // -- hash_hex (#335) ----------------------------------------------------

    /// Same known answer as above, spelled the way Postgres's
    /// `encode(..., 'hex')` spells it: 64 lowercase digits.
    #[test]
    fn hash_hex_is_lowercase_and_64_digits() {
        let hash = token_hash(&"A".repeat(43)).unwrap();
        assert_eq!(
            hash_hex(&hash),
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
    }

    #[test]
    fn hash_hex_keeps_leading_zeros() {
        let mut hash = [0u8; 32];
        hash[31] = 0x0a;
        assert_eq!(hash_hex(&hash), format!("{}0a", "0".repeat(62)));
    }
}
