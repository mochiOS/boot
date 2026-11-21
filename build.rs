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
        println!("cargo:rustc-check-cfg=cfg(arm64_boot_module)");
        println!("cargo:rustc-check-cfg=cfg(arm64_boot_disk)");
        println!("cargo:rerun-if-env-changed=MOCHIOS_ARM64_BOOT_MODULE");
        if let Some(path) = std::env::var_os("MOCHIOS_ARM64_BOOT_MODULE") {
            let path = std::fs::canonicalize(path).expect("ARM64 boot module is missing");
            println!("cargo:rerun-if-changed={}", path.display());
            println!(
                "cargo:rustc-env=MOCHIOS_ARM64_BOOT_MODULE={}",
                path.display()
            );
            println!("cargo:rustc-cfg=arm64_boot_module");
        }
        println!("cargo:rerun-if-env-changed=MOCHIOS_ARM64_BOOT_DISK");
        if let Some(path) = std::env::var_os("MOCHIOS_ARM64_BOOT_DISK") {
            embed_disk(path);
        }
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

fn embed_disk(path: std::ffi::OsString) {
    let path = std::fs::canonicalize(path).expect("ARM64 boot disk is missing");
    let filename = path
        .to_str()
        .filter(|name| !name.contains(['"', '\\', '\n', '\r']))
        .expect("ARM64 boot disk path cannot be represented in assembly");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("missing output dir"));
    let assembly = format!(
        ".section .data.arm64_boot_disk,\"aw\",%progbits\n\
         .balign 4096\n\
         .global __arm64_boot_disk_start, __arm64_boot_disk_end\n\
         __arm64_boot_disk_start:\n\
         .incbin \"{filename}\"\n\
         __arm64_boot_disk_end:\n"
    );
    std::fs::write(output.join("arm64-boot-disk.S"), assembly)
        .expect("write ARM64 boot disk input");
    println!("cargo:rerun-if-changed={filename}");
    println!("cargo:rustc-cfg=arm64_boot_disk");
    println!("cargo:rustc-link-arg-bin=mochios-native=--defsym=NATIVE_FREE_RAM=0xc000000");
}
