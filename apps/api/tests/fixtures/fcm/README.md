# Test service account for FCM (#1003)

A Google service account key file made for the tests of
`notifications::fcm` (`src/notifications/fcm.rs` and
`tests/fcm_push_flow.rs`), which sign the service account's assertion the
way FCM's sender does and check it with the public half. It names a
project that does not exist and signs nothing else: not a secret.

- `service_account.json`: the shape Google's console gives (`type`,
  `client_email`, `private_key` as PKCS#8 PEM, `token_uri`…).
- `public_jwk.json`: the public half of `private_key`, as a JWK (`n`, `e`).

Made with `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048`,
then the modulus (`openssl rsa -pubin -noout -modulus`) as unpadded
base64url for `n` (`e` is `AQAB`).
