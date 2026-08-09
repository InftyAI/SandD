//! Daemon-token verification.
//!
//! A daemon proves who it is with a short-lived EdDSA (Ed25519) JWT minted by the
//! Nebula manager at instance-provision time and delivered through the instance
//! bootstrap. This module is the verifying half.
//!
//! WHY ASYMMETRIC: verification is purely LOCAL — the public key is enough, so a
//! controller never calls back to the manager to admit a daemon (no network hop on the
//! connect path, and no dependency on the manager being reachable). The private key
//! never leaves the manager process, which matters because this controller runs in the
//! WORKLOAD's namespace: everything handed to it is readable by that namespace's
//! tenants. A shared HMAC secret could not work here — the key that verifies would also
//! mint, so any tenant who read it could forge a token for any daemon.
//!
//! WHAT A TOKEN ASSERTS, and what each claim defends against:
//!   - `sub`  the daemon id. The caller binds it to the id in `Register`, so a leaked
//!            token cannot be used to impersonate a DIFFERENT daemon.
//!   - `aud`  this controller's id. One controller serves one workload, so a token
//!            minted for another workload's controller is refused here even though it
//!            carries a valid signature from the same manager.
//!   - `iss`  the minting system, so a token signed by an unrelated system whose key we
//!            happen to trust cannot pass as a daemon token.
//!   - `exp`  bounds a leak: a stolen token stops working on its own.
//!   - `kid`  (header) names the key, so a rotation can hold old and new keys at once
//!            instead of needing a flag day.
//!
//! The `alg` is PINNED to EdDSA rather than read from the token header. A verifier that
//! trusts the header's alg can be walked down to `"alg":"none"` — the classic JWT
//! bypass, where an attacker strips the signature and the library obligingly accepts an
//! unsigned token.

use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// The claim set the manager mints. Mirrors `pkg/sandd.DaemonClaims` on the Nebula
/// side; `tenant` is optional there and absent in single-tenant deployments.
///
/// Only the claims this controller acts on are declared. `iat`/`nbf` are validated by
/// the library without needing fields here.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct DaemonClaims {
    /// The daemon id this token was minted for. Authoritative: the caller requires
    /// `Register.daemon_id == sub`.
    pub sub: String,
    /// Cross-tenant scope. Optional; empty/absent in a single-tenant cluster.
    #[serde(default)]
    pub tenant: String,
}

/// Why a token was refused. Kept coarse ON PURPOSE — it is what goes back to an
/// unauthenticated caller, and a precise reason ("bad signature" vs "wrong audience")
/// tells a prober which part of its forgery to fix. The detail goes to the log instead,
/// where the operator can see it and the caller cannot.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// No `Authorization: Bearer <jwt>` header, or it was malformed.
    MissingToken,
    /// Present but not acceptable: bad signature, wrong aud/iss, expired, unknown kid.
    InvalidToken,
}

impl AuthError {
    /// The log line for this failure. Deliberately NOT the client's response body.
    pub fn detail(&self) -> &'static str {
        match self {
            AuthError::MissingToken => "missing or malformed Authorization: Bearer header",
            AuthError::InvalidToken => "token rejected",
        }
    }
}

/// Verifies daemon tokens against one public key.
///
/// Built once at startup and shared: `decode` needs only `&self`, so this holds no
/// per-connection state and never mutates.
pub struct TokenVerifier {
    key: DecodingKey,
    /// Pre-built so the aud/iss/exp rules cannot drift between call sites — there is
    /// exactly one place the policy is expressed.
    validation: Validation,
    /// The `kid` this key answers to. Empty means "accept any kid", which is the
    /// single-key case; see `verify`.
    kid: String,
}

impl TokenVerifier {
    /// Builds a verifier from the PKIX PEM public key the Nebula manager hands the
    /// controller (`SANDD_SIGNING_PUBLIC_KEY`), the controller's own id
    /// (`SANDD_CONTROLLER_ID`, which is the only `aud` it admits), the required issuer,
    /// and the key id.
    ///
    /// Fails on a key that will not parse rather than degrading to "accept everything":
    /// a controller that admits every caller is worse than one that never started,
    /// because the failure is silent and looks healthy.
    pub fn new(
        public_key_pem: &str,
        controller_id: &str,
        issuer: &str,
        kid: &str,
    ) -> Result<Self, String> {
        if public_key_pem.trim().is_empty() {
            return Err("public key is empty".to_string());
        }
        if controller_id.trim().is_empty() {
            // Without this, `aud` validation would accept the empty audience and the
            // per-workload isolation boundary would silently vanish.
            return Err("controller id (the required audience) is empty".to_string());
        }
        let key = DecodingKey::from_ed_pem(public_key_pem.as_bytes())
            .map_err(|e| format!("public key is not a PKIX PEM Ed25519 key: {}", e))?;

        // EdDSA is pinned here, not taken from the token header — see the module docs on
        // the "alg":"none" downgrade.
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_audience(&[controller_id]);
        // set_issuer/set_audience only take effect because the corresponding required
        // claims are enforced: a token that OMITS aud or iss must be rejected, not
        // treated as trivially matching.
        validation.set_issuer(&[issuer]);
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);

        Ok(Self {
            key,
            validation,
            kid: kid.to_string(),
        })
    }

    /// Verifies a raw token, returning its claims.
    ///
    /// The caller must still bind `claims.sub` to the id the daemon registers as; this
    /// function proves the token is authentic and addressed to this controller, not that
    /// the bearer is who it later says it is.
    pub fn verify(&self, token: &str) -> Result<DaemonClaims, AuthError> {
        // Check the kid BEFORE the signature so a rotation mismatch is distinguishable
        // in the logs from a forgery — both are refused, but they need different fixes.
        if !self.kid.is_empty() {
            match jsonwebtoken::decode_header(token) {
                Ok(header) => match header.kid {
                    Some(ref k) if k == &self.kid => {}
                    Some(ref k) => {
                        tracing::warn!(
                            "rejecting token with unknown kid {:?} (this controller holds {:?}); \
                             a key rotation needs the new public key deployed here too",
                            k,
                            self.kid
                        );
                        return Err(AuthError::InvalidToken);
                    }
                    None => {
                        tracing::warn!("rejecting token with no kid header");
                        return Err(AuthError::InvalidToken);
                    }
                },
                Err(e) => {
                    tracing::warn!("rejecting token with an unparseable header: {}", e);
                    return Err(AuthError::InvalidToken);
                }
            }
        }

        match decode::<DaemonClaims>(token, &self.key, &self.validation) {
            Ok(data) => Ok(data.claims),
            Err(e) => {
                // The error kind is safe to LOG (it is what an operator needs) but must
                // not reach the caller — see AuthError.
                tracing::warn!("rejecting daemon token: {}", e);
                Err(AuthError::InvalidToken)
            }
        }
    }
}

/// Extracts the bearer token from an `Authorization` header value.
///
/// Split out so it is testable without a request, and so the scheme handling has one
/// definition. The scheme match is case-INSENSITIVE ("Bearer" per RFC 6750, but the
/// scheme is case-insensitive per RFC 7235 and clients do vary); the token itself is
/// compared byte-for-byte.
pub fn bearer_token(header: Option<&str>) -> Result<&str, AuthError> {
    let value = header.ok_or(AuthError::MissingToken)?;
    let rest = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .ok_or(AuthError::MissingToken)?;
    let token = rest.trim();
    if token.is_empty() {
        return Err(AuthError::MissingToken);
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;
    use std::time::{SystemTime, UNIX_EPOCH};

    // A fixed Ed25519 keypair in the exact formats the two sides exchange: PKCS#8 for
    // the manager's private key, PKIX for the public key it hands the controller.
    // Generated with `openssl genpkey -algorithm ed25519`.
    const PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIM7uPMqQFHrM7SxKZmYSSDgY4KGYQVMzEc2Yb3FUJqrO\n-----END PRIVATE KEY-----\n";

    // A DIFFERENT keypair, for the "signed by a key we do not trust" case.
    const OTHER_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEILrLXQ+DsvT7WkQvJ3xCn5V1F0VUxfNMcM0dbT8jRMLM\n-----END PRIVATE KEY-----\n";

    const CONTROLLER_ID: &str = "sandd-abc-uid";
    const ISSUER: &str = "nebula";
    const KID: &str = "kid-1";

    #[derive(Serialize)]
    struct TestClaims {
        sub: String,
        aud: String,
        iss: String,
        exp: u64,
        iat: u64,
        #[serde(skip_serializing_if = "String::is_empty")]
        tenant: String,
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Derives the PKIX public PEM from a PKCS#8 private PEM, the way the manager's
    /// `Signer.PublicKeyPEM()` does — so the test exercises real key material rather
    /// than a hardcoded pair that could silently drift apart.
    fn public_pem(private_pem: &str) -> String {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("openssl")
            .args(["pkey", "-pubout"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("openssl must be on PATH for these tests");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(private_pem.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "openssl pkey -pubout failed");
        String::from_utf8(out.stdout).unwrap()
    }

    fn verifier() -> TokenVerifier {
        TokenVerifier::new(&public_pem(PRIVATE_PEM), CONTROLLER_ID, ISSUER, KID).unwrap()
    }

    /// Mints a token the way the manager does, with overridable parts so each test can
    /// vary exactly one thing.
    fn mint(
        private_pem: &str,
        sub: &str,
        aud: &str,
        iss: &str,
        kid: Option<&str>,
        exp_offset: i64,
    ) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = kid.map(|k| k.to_string());
        let n = now();
        let claims = TestClaims {
            sub: sub.to_string(),
            aud: aud.to_string(),
            iss: iss.to_string(),
            exp: (n as i64 + exp_offset) as u64,
            iat: n,
            tenant: String::new(),
        };
        let key = EncodingKey::from_ed_pem(private_pem.as_bytes()).unwrap();
        encode(&header, &claims, &key).unwrap()
    }

    fn valid_token() -> String {
        mint(
            PRIVATE_PEM,
            "default--my-pod",
            CONTROLLER_ID,
            ISSUER,
            Some(KID),
            3600,
        )
    }

    // The happy path: a token minted by the manager for THIS controller is admitted,
    // and `sub` survives so the caller can bind it to the registering daemon id.
    #[test]
    fn accepts_a_token_minted_for_this_controller() {
        let claims = verifier().verify(&valid_token()).unwrap();

        assert_eq!(claims.sub, "default--my-pod");
    }

    // The isolation boundary between workloads. One controller serves one workload, so a
    // token minted for a DIFFERENT controller must be refused even though its signature
    // is perfectly valid — this is the check that stops a compromised workload from
    // driving another workload's daemons.
    #[test]
    fn rejects_a_token_minted_for_another_controller() {
        let token = mint(
            PRIVATE_PEM,
            "default--my-pod",
            "sandd-SOMEONE-ELSE",
            ISSUER,
            Some(KID),
            3600,
        );

        assert_eq!(verifier().verify(&token), Err(AuthError::InvalidToken));
    }

    // A token signed by a key this controller does not hold. Without the signature
    // check, anyone could mint their own tokens with the right aud and walk in.
    #[test]
    fn rejects_a_token_signed_by_an_untrusted_key() {
        let token = mint(
            OTHER_PRIVATE_PEM,
            "default--my-pod",
            CONTROLLER_ID,
            ISSUER,
            Some(KID),
            3600,
        );

        assert_eq!(verifier().verify(&token), Err(AuthError::InvalidToken));
    }

    // exp is what bounds a leak. A stolen token has to stop working on its own, since
    // nothing revokes it.
    #[test]
    fn rejects_an_expired_token() {
        let token = mint(
            PRIVATE_PEM,
            "default--my-pod",
            CONTROLLER_ID,
            ISSUER,
            Some(KID),
            -3600, // expired an hour ago
        );

        assert_eq!(verifier().verify(&token), Err(AuthError::InvalidToken));
    }

    // A valid signature from an unrelated system whose key we happen to trust must not
    // pass as a daemon token.
    #[test]
    fn rejects_a_token_from_an_unexpected_issuer() {
        let token = mint(
            PRIVATE_PEM,
            "default--my-pod",
            CONTROLLER_ID,
            "some-other-system",
            Some(KID),
            3600,
        );

        assert_eq!(verifier().verify(&token), Err(AuthError::InvalidToken));
    }

    // THE classic JWT bypass: strip the signature and claim the token is unsigned. It
    // must fail because the algorithm is pinned, not read from the header.
    #[test]
    fn rejects_an_unsigned_none_alg_token() {
        // {"alg":"none","kid":"kid-1"} with the real token's claims and no signature.
        let token = valid_token();
        let claims_segment = token.split('.').nth(1).unwrap();
        let header = base64_url(br#"{"alg":"none","kid":"kid-1"}"#);
        let forged = format!("{}.{}.", header, claims_segment);

        assert_eq!(verifier().verify(&forged), Err(AuthError::InvalidToken));
    }

    // Tampering with the payload (e.g. swapping `sub` to another daemon's id) must
    // invalidate the signature.
    #[test]
    fn rejects_a_tampered_payload() {
        let token = valid_token();
        let parts: Vec<&str> = token.split('.').collect();
        let forged_claims = base64_url(
            format!(
                r#"{{"sub":"default--victim","aud":"{}","iss":"{}","exp":{},"iat":{}}}"#,
                CONTROLLER_ID,
                ISSUER,
                now() + 3600,
                now()
            )
            .as_bytes(),
        );
        let forged = format!("{}.{}.{}", parts[0], forged_claims, parts[2]);

        assert_eq!(verifier().verify(&forged), Err(AuthError::InvalidToken));
    }

    // A rotation the operator only half-finished: the manager mints under a new kid
    // while this controller still holds the old public key. Refusing is correct, and the
    // log must name both ids — otherwise this is indistinguishable from a forgery and an
    // operator has nothing to act on.
    #[test]
    fn rejects_an_unknown_kid() {
        let token = mint(
            PRIVATE_PEM,
            "default--my-pod",
            CONTROLLER_ID,
            ISSUER,
            Some("kid-2"),
            3600,
        );

        assert_eq!(verifier().verify(&token), Err(AuthError::InvalidToken));
    }

    // A token carrying no kid at all cannot be matched to a key, so it is refused
    // rather than optimistically tried against the only key we hold.
    #[test]
    fn rejects_a_missing_kid() {
        let token = mint(
            PRIVATE_PEM,
            "default--my-pod",
            CONTROLLER_ID,
            ISSUER,
            None,
            3600,
        );

        assert_eq!(verifier().verify(&token), Err(AuthError::InvalidToken));
    }

    // Garbage in the Authorization header must be refused, not panic. This is an
    // unauthenticated code path — anything reachable from the internet gets fed junk.
    #[test]
    fn rejects_garbage_instead_of_panicking() {
        let v = verifier();
        for junk in ["", "not-a-jwt", "a.b.c", "....", "eyJhbGciOiJFZERTQSJ9"] {
            assert_eq!(
                v.verify(junk),
                Err(AuthError::InvalidToken),
                "input {:?} must be refused",
                junk
            );
        }
    }

    // A key that will not parse must fail construction. Starting with auth silently
    // disabled would look healthy while admitting everyone.
    #[test]
    fn refuses_to_build_without_a_usable_key() {
        assert!(TokenVerifier::new("", CONTROLLER_ID, ISSUER, KID).is_err());
        assert!(TokenVerifier::new("   \n ", CONTROLLER_ID, ISSUER, KID).is_err());
        assert!(TokenVerifier::new(
            "-----BEGIN PUBLIC KEY-----\nnope\n-----END PUBLIC KEY-----\n",
            CONTROLLER_ID,
            ISSUER,
            KID
        )
        .is_err());
        // A PRIVATE key where a public one belongs: a misconfiguration worth catching
        // loudly, since it means private material was routed into a tenant namespace.
        assert!(TokenVerifier::new(PRIVATE_PEM, CONTROLLER_ID, ISSUER, KID).is_err());
    }

    // An empty controller id would make `aud` validation vacuous and dissolve the
    // per-workload boundary, so it must be refused at construction.
    #[test]
    fn refuses_to_build_without_a_controller_id() {
        let pem = public_pem(PRIVATE_PEM);

        assert!(TokenVerifier::new(&pem, "", ISSUER, KID).is_err());
        assert!(TokenVerifier::new(&pem, "  ", ISSUER, KID).is_err());
    }

    #[test]
    fn extracts_a_bearer_token() {
        assert_eq!(bearer_token(Some("Bearer abc.def.ghi")), Ok("abc.def.ghi"));
        // Scheme is case-insensitive per RFC 7235; clients vary.
        assert_eq!(bearer_token(Some("bearer abc.def.ghi")), Ok("abc.def.ghi"));
    }

    #[test]
    fn rejects_a_missing_or_malformed_authorization_header() {
        for header in [
            None,
            Some(""),
            Some("abc.def.ghi"),
            Some("Basic dXNlcjpwdw=="),
            Some("Bearer"),
            Some("Bearer    "),
        ] {
            assert_eq!(
                bearer_token(header),
                Err(AuthError::MissingToken),
                "header {:?} must be refused",
                header
            );
        }
    }

    /// Minimal base64url-no-pad encoder, so a test can hand-forge a JWT segment.
    fn base64_url(raw: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    }
}
