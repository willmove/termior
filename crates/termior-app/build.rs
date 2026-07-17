use std::path::PathBuf;

fn main() {
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/termior.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = icon
        .to_str()
        .expect("Termior Windows icon path must be valid UTF-8");
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon)
        .set("ProductName", "Termior")
        .set(
            "FileDescription",
            "Termior AI-native development environment",
        )
        .set("OriginalFilename", "termior-app.exe");
    resource
        .compile()
        .expect("failed to embed the Termior Windows icon");
}
