//! Shared construction for blocking reqwest clients.

/// Install ring for reqwest's `rustls-no-provider` feature when no provider is
/// installed yet. A concurrent caller may win the one-time installation.
pub(crate) fn ensure_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none()
        && rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        && rustls::crypto::CryptoProvider::get_default().is_none()
    {
        panic!("failed to install the Rustls ring crypto provider");
    }
}

/// Build a blocking client builder after ensuring reqwest has a Rustls
/// provider. Keeping this setup here makes test clients safe too.
pub(crate) fn blocking_client_builder() -> reqwest::blocking::ClientBuilder {
    ensure_crypto_provider();
    reqwest::blocking::Client::builder()
}
