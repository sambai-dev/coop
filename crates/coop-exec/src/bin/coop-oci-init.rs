#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(coop_exec::oci_init::main());
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("coop-oci-init is available only on Linux");
    std::process::exit(64);
}
