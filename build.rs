#[path = "build-support/build-boot-logo.rs"]
mod boot_logo;

fn main() {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("missing manifest dir"),
    );
    boot_logo::generate(&manifest_dir.join("build-support/mochimochi-kun.png"));
    println!("cargo:rerun-if-changed=domain.ld");
    println!("cargo:rerun-if-changed=domain-aarch64.ld");
    if std::env::var_os("CARGO_FEATURE_ARM64_NATIVE").is_some()
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none")
    {
        println!("cargo:rustc-link-arg-bin=mochios-native=-zmax-page-size=4096");
        println!("cargo:rustc-link-arg-bin=mochios-native=--defsym=NATIVE_IMAGE=1");
        println!(
            "cargo:rustc-link-arg-bin=mochios-native=-T{}/domain-aarch64.ld",
            manifest_dir.display()
        );
    }
    if (std::env::var_os("CARGO_FEATURE_SYSTEM_DOMAIN").is_some()
        || std::env::var_os("CARGO_FEATURE_MDRIVER_PROBE").is_some()
        || std::env::var_os("CARGO_FEATURE_DOMAIN_PROBES").is_some())
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none")
    {
        let manifest =
            std::env::var("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR");
        let script = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
            && std::env::var_os("CARGO_FEATURE_SYSTEM_DOMAIN").is_some()
        {
            println!("cargo:rustc-link-arg=-zmax-page-size=4096");
            "domain-aarch64.ld"
        } else {
            "domain.ld"
        };
        for (feature, binaries) in [
            ("SYSTEM_DOMAIN", &["mochios-system"][..]),
            ("MDRIVER_PROBE", &["mdriver-probe"][..]),
            (
                "DOMAIN_PROBES",
                &[
                    "domain-bootstrap",
                    "hardware-bootstrap",
                    "event-bootstrap",
                    "grant-bootstrap",
                    "ring-bootstrap",
                ][..],
            ),
        ] {
            if std::env::var_os(format!("CARGO_FEATURE_{feature}")).is_some() {
                for binary in binaries {
                    println!("cargo:rustc-link-arg-bin={binary}=-T{manifest}/{script}");
                }
            }
        }
    }
}
