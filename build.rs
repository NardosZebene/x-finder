fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("cupertino".into());
    slint_build::compile_with_config("ui/main.slint", config).unwrap();
    // App icon and name ("X Finder") for Explorer, the taskbar and pins.
    println!("cargo:rerun-if-changed=assets/xfinder.rc");
    println!("cargo:rerun-if-changed=assets/xfinder.ico");
    embed_resource::compile("assets/xfinder.rc", embed_resource::NONE).manifest_optional().unwrap();
}
