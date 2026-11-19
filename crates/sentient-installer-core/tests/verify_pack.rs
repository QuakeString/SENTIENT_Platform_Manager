// Copyright © 2016-2026 The SENTIENT Authors

//! A real offline pack (scripts/package-offline-windows.sh) checked with the
//! Platform Manager's own code: the signature of pack.json, the target
//! (as a Windows 10 21H2 x64 computer), every file's size and SHA-256, and
//! that each install path finds what it takes from the pack.
//!
//!   SPM_VERIFY_PACK=<unzipped pack folder> \
//!   SPM_VERIFY_PACK_KEY=<public key: .pub file or its base64 line> \
//!   cargo test -p sentient-installer-core --test verify_pack -- --nocapture
//!
//! Without SPM_VERIFY_PACK_KEY the release key (signing::RELEASE_PUBLIC_KEY)
//! is used, as Setup does. Without SPM_VERIFY_PACK the test does nothing.

use std::path::PathBuf;

use sentient_installer_core::distro::pack_use;
use sentient_installer_core::distro::DeployConfig;
use sentient_installer_core::pack::{self, Host, Kind};
use sentient_installer_core::stack::Flavor;

fn key() -> String {
    match std::env::var("SPM_VERIFY_PACK_KEY") {
        Ok(k) if std::path::Path::new(&k).is_file() => {
            let text = std::fs::read_to_string(&k).unwrap();
            text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("untrusted comment:")).expect("no key line in the .pub file").to_string()
        }
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => sentient_installer_core::signing::RELEASE_PUBLIC_KEY.to_string(),
    }
}

#[test]
fn the_pack_verifies_with_the_platform_managers_checks() {
    let Some(dir) = std::env::var_os("SPM_VERIFY_PACK").map(PathBuf::from) else {
        eprintln!("SPM_VERIFY_PACK is not set: nothing to verify");
        return;
    };
    let opened = pack::open_with_key(&key(), &dir).unwrap_or_else(|e| panic!("{}", e.message()));
    let p = &opened.pack;
    eprintln!("signed: {}", opened.signed);
    eprintln!("SPM {} / SENTIENT {} (licence build {}) / gateway {:?}", p.spm_version, p.sentient_version, p.licence_build(), p.gateway_version);
    eprintln!("components: {:?}", p.components);
    eprintln!("{}", p.check_title());

    // The computer it is meant for, and one it is not.
    let win10 = Host { os: "windows".into(), os_name: "Windows 10 (build 19045)".into(), arch: "x86_64".into(), windows_build: Some(19045) };
    pack::check_target(p, &win10).unwrap_or_else(|e| panic!("{}", e.message()));
    assert!(pack::check_target(p, &Host::this()).is_err() || cfg!(windows));

    // Every file, as Setup reads them.
    let last = std::cell::Cell::new(0usize);
    opened
        .verify_files(
            &|c| {
                if c.done_files != last.get() {
                    last.set(c.done_files);
                    eprintln!("  ok {:>3}/{} {}", c.done_files, c.files, c.file);
                }
            },
            &|| false,
        )
        .unwrap_or_else(|e| panic!("{}", e.message()));
    opened.check_present().unwrap();

    // Native: the payload folder holds what the native engine needs.
    let payload = opened.payload_dir().expect("no payload");
    for f in [sentient_installer_core::native::PG_ZIP, sentient_installer_core::native::TSDB_ZIP, "sentient-payload.tar", "vc_redist.x64.exe", "VERSION"] {
        assert!(payload.join(f).is_file(), "payload/{f} missing");
    }
    assert_eq!(std::fs::read_to_string(payload.join("VERSION")).unwrap().trim(), p.sentient_version);
    for kind in [Kind::Spm, Kind::Payload, Kind::Pg, Kind::Tsdb, Kind::Vcredist, Kind::Gateway, Kind::WslMsi, Kind::KernelMsi, Kind::Rootfs] {
        assert!(opened.file(kind).is_some(), "no {kind:?} file");
    }

    // The gateway is offered from the pack.
    let offer = sentient_installer_core::gateway::offer_from_pack(&opened).expect("no gateway offer");
    assert_eq!(Some(offer.version.clone()), p.gateway_version);

    // Docker in WSL: the rootfs, the packages, and every image of every flavour.
    assert!(opened.has_docker());
    pack_use::rootfs(&opened).unwrap();
    let cmd = pack_use::docker_install(&opened).unwrap();
    assert!(cmd.contains("dpkg -i"));
    let image = format!("quakestring/sentient:{}", p.sentient_version);
    for flavor in [Flavor::Stock, Flavor::Timescaledb, Flavor::Clickhouse] {
        let cfg = DeployConfig { flavor, image: image.clone(), ..DeployConfig::default() };
        match pack_use::image_step(&pack_use::stack_images(&cfg), Some(&opened), &|_| false) {
            Ok(pack_use::ImageStep::Load(files)) => eprintln!("  {flavor:?}: {}", files.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>().join(", ")),
            other => panic!("{flavor:?}: {other:?}"),
        }
    }
    pack_use::image_step(&pack_use::clickhouse_images(), Some(&opened), &|_| false).unwrap();
    for f in opened.files(Kind::Image) {
        assert!(f.image_digest.as_deref().is_some_and(|d| d.starts_with("sha256:")), "{} has no digest", f.path);
    }
    eprintln!("VERIFIED: {} files, {} bytes, {} Docker packages, {} images", p.files.len(), p.total_bytes(), opened.files(Kind::Deb).len(), opened.files(Kind::Image).len());
}
