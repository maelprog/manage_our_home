use anyhow::{anyhow, Result};
use argon2::password_hash::{
    rand_core::{OsRng, RngCore},
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("password hashing failed: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(|e| anyhow!("invalid password hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// A hash for the login branches that have none of their own (#178).
///
/// `POST /auth/login` used to answer an unknown email in ~0,22 ms and a
/// known one in ~256 ms, because only the second reached argon2id — three
/// orders of magnitude that told any stopwatch whether an address has an
/// account here. Verifying the submitted password against this decoy makes
/// the branch with no stored hash do exactly the same work as the branch
/// with one.
///
/// It is a hash of 32 bytes from the OS random source, produced by
/// [`hash_password`] itself, so its cost parameters are by construction the
/// ones real passwords are hashed with — pinning a literal here would let
/// the two drift apart the day `Argon2::default()` changes, and the drift
/// would reopen the oracle silently. Nothing can match it: the preimage is
/// drawn once per process and never leaves this function.
///
/// Computed on first use. Call it once at startup (`build_router` does) so
/// that one cost is not charged to whichever login happens to be first.
pub fn decoy_hash() -> &'static str {
    static DECOY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        let secret: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        hash_password(&secret).expect("hashing a random secret cannot fail")
    });
    &DECOY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_uses_argon2id() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$"));
    }

    #[test]
    fn verify_roundtrip() {
        let hash = hash_password("hunter2").unwrap();
        assert!(verify_password("hunter2", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn hash_is_salted_differently_each_time() {
        let a = hash_password("same-password").unwrap();
        let b = hash_password("same-password").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn the_decoy_costs_what_a_real_hash_costs() {
        // The whole point of the decoy is that the branch using it is
        // indistinguishable from the branch using a stored hash. Different
        // algorithm, version or cost parameters and the two branches
        // separate again on a stopwatch — which is issue #178.
        let decoy = PasswordHash::new(decoy_hash()).unwrap();
        let real = hash_password("hunter2").unwrap();
        let real = PasswordHash::new(&real).unwrap();

        assert_eq!(decoy.algorithm, real.algorithm);
        assert_eq!(decoy.version, real.version);
        assert_eq!(decoy.params, real.params);
    }

    #[test]
    fn nothing_verifies_against_the_decoy() {
        for candidate in ["", "hunter2", "correct horse battery staple", decoy_hash()] {
            assert!(!verify_password(candidate, decoy_hash()).unwrap());
        }
    }

    #[test]
    fn the_decoy_is_one_hash_for_the_whole_process() {
        // Re-deriving it per call would put a second argon2id on the
        // unknown-email branch and make it the *slow* one.
        assert_eq!(decoy_hash().as_ptr(), decoy_hash().as_ptr());
    }
}
