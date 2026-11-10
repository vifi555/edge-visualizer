fn main() {
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // Puts the icon into the .exe. If the Windows resource compiler isn't found,
    // the app still builds, it just uses a default icon.
    let _ = embed_resource::compile("app.rc", embed_resource::NONE);
}
