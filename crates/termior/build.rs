#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/termior.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    let icon = icon
        .to_str()
        .ok_or("Termior Windows icon path must be valid UTF-8")?;
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon)
        .set("ProductName", "Termior")
        .set(
            "FileDescription",
            "Termior AI-native development environment",
        )
        .set("OriginalFilename", "termior.exe");
    resource.compile()?;
    Ok(())
}
