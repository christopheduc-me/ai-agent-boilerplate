//! Thin exit-code shell around the tested probe in `backend::healthcheck`.

fn main() {
    if let Err(reason) = backend::healthcheck::check(&backend::healthcheck::addr_from_env()) {
        eprintln!("healthcheck failed: {reason}");
        std::process::exit(1);
    }
}
