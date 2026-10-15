use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let output_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: rcgen013-fixturegen <output-directory>")?;

    // Keep these values aligned with build_root_ca_params from the rcgen
    // 0.13.2 version of the TLS module so the fixture models a real install.
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params
        .distinguished_name
        .push(DnType::CommonName, "Buildmesh Dev Root CA");
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2035, 1, 1);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);

    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    fs::create_dir_all(&output_dir)?;
    fs::write(output_dir.join("rcgen-0.13.2-ca.der"), cert.der().as_ref())?;
    fs::write(
        output_dir.join("rcgen-0.13.2-ca.key.der"),
        key.serialize_der(),
    )?;
    Ok(())
}
