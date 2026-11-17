// Generates the Windows executable icon from the logo code in src/icon.rs.

#[allow(dead_code)]
mod icon {
    include!("src/icon.rs");
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("spoon.ico");
        std::fs::write(&path, icon::ico(&[16, 24, 32, 48, 64, 256])).expect("write icon");
        winresource::WindowsResource::new()
            .set_icon(path.to_str().expect("utf-8 OUT_DIR"))
            // Shown by Task Manager and read by the installer script.
            .set("ProductName", "Spoon")
            .set("FileDescription", "Spoon")
            .compile()
            .expect("embed icon resource");
    }
}
