# Test keys for the native Google sign-in (#669)

Two RSA 2048 key pairs made for the tests of `auth::google_id_token`
(`src/auth/google_id_token.rs` and `tests/google_id_token_flow.rs`), which
sign ID tokens the way Google does and serve the public halves as a JWKS
from a local listener, so no test calls Google. They sign nothing else and
are not secrets.

- `key_a.der`, `key_b.der`: the private keys, PKCS#1 DER.
- `jwk_a.json`, `jwk_b.json`: their public halves as JWKs, `kid`
  `test-key-a` and `test-key-b`.

Regenerated with `openssl genrsa -traditional 2048`, then
`openssl rsa -traditional -outform DER` for the DER, and the modulus
(`openssl rsa -inform DER -noout -modulus`) as unpadded base64url for `n`
(`e` is `AQAB`).
