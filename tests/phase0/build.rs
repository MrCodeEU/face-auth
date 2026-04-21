// Build script for phase0 test binaries
// Creates libpam.so symlink for linking if pam-devel isn't installed

use std::path::PathBuf;

fn main() {
    // Only needed for test-pam-stub, but build.rs runs for the whole crate
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // Check if libpam.so (dev symlink) exists
    if !std::path::Path::new("/usr/lib64/libpam.so").exists() {
        // Find the versioned lib
        let versioned = "/usr/lib64/libpam.so.0";
        if std::path::Path::new(versioned).exists() {
            // Create symlink in OUT_DIR
            let link = out_dir.join("libpam.so");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(versioned, &link)
                .expect("create libpam.so symlink in OUT_DIR");
            println!("cargo:rustc-link-search=native={}", out_dir.display());
        }
    }
}
