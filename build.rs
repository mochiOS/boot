#[path = "build-support/build-boot-logo.rs"]
mod boot_logo;

fn main() {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("missing manifest dir"),
    );
    boot_logo::generate(&manifest_dir.join("build-support/mochimochi-kun.png"));
    println!("cargo:rerun-if-changed=domain.ld");
    if (std::env::var_os("CARGO_FEATURE_SYSTEM_DOMAIN").is_some()
        || std::env::var_os("CARGO_FEATURE_MDRIVER_PROBE").is_some()
        || std::env::var_os("CARGO_FEATURE_DOMAIN_PROBES").is_some())
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none")
    {
        let manifest =
            std::env::var("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR");
        println!("cargo:rustc-link-arg=-T{manifest}/domain.ld");
    }
}
