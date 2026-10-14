# rcgen 0.13.2 CA fixture generator

This standalone crate pins rcgen 0.13.2 and generates the persisted root CA
fixture used by `http::tls::tests::rcgen_014_leaf_chains_to_persisted_rcgen_013_root`.
Its root parameters match the pre-upgrade `build_root_ca_params` values.

Run from the repository root:

```powershell
cargo run --locked --manifest-path tools/rcgen-013-ca-fixturegen/Cargo.toml -- src-tauri/src/http/fixtures
```

The generator creates a random key pair. Commit the certificate and matching
key together because the compatibility test loads both persisted files.
