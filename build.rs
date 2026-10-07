fn main() {
    // Releases are numbered a.b.c.d; Cargo only takes three numbers, so the
    // fourth rides along as build metadata (version = "a.b.c+d").
    let cargo = std::env::var("CARGO_PKG_VERSION").unwrap();
    let version = match cargo.split_once('+') {
        Some((base, build)) => format!("{base}.{build}"),
        None => format!("{cargo}.0"),
    };
    println!("cargo:rustc-env=ONIFY_VERSION={version}");

    glib_build_tools::compile_resources(
        &["data"],
        "data/resources.gresource.xml",
        "onify.gresource",
    );
}
