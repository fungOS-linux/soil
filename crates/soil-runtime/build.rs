fn main() {
    for name in [
        "phone-shell",
        "phone-lid",
        "canvas-client",
        "battery-canvas",
        "estimate-canvas",
        "animated-battery-canvas",
        "pixel-client",
    ] {
        let source = format!("fixtures/{name}.wat");
        println!("cargo:rerun-if-changed={source}");
        let bytes = wat::parse_file(&source).expect("guest fixture");
        std::fs::write(
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap())
                .join(format!("{name}.wasm")),
            bytes,
        )
        .unwrap();
    }
}
