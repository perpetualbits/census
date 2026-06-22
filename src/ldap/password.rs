//! Password hashing for the client-side `{CRYPT}` scheme.
//!
//! The default scheme is the server-side RFC 3062 exop (handled in
//! [`crate::ldap::client::LdapClient::set_password`]); this module backs the
//! `crypt` alternative for servers without the password-modify overlay.

use anyhow::Context;

/// SHA-512 crypt the plaintext and prefix it with `{CRYPT}` for OpenLDAP's
/// `userPassword` storage scheme. Produces `{CRYPT}$6$<salt>$<hash>`.
pub fn crypt_sha512(plaintext: &str) -> anyhow::Result<String> {
    let hash = pwhash::sha512_crypt::hash(plaintext)
        .map_err(|e| anyhow::anyhow!("SHA-512 crypt failed: {e}"))
        .context("hashing password")?;
    Ok(format!("{{CRYPT}}{hash}"))
}
