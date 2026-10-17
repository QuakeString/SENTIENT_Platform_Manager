// Copyright © 2016-2026 The SENTIENT Authors

//! Check the download agent against URLs: `cargo run --example tls_check -- <url>…`.

fn main() {
    for url in std::env::args().skip(1) {
        match sentient_installer_core::net::agent().head(&url).call() {
            Ok(r) => println!("OK      {} {url}", r.status()),
            Err(ureq::Error::Status(code, _)) => println!("OK      {code} {url}"),
            Err(e) => println!("REFUSED {url}: {e}"),
        }
    }
}
