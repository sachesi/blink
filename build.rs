use std::{env, fs, path::Path, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let resource_dir = out.join("resources");
    let ui_out = resource_dir.join("ui");
    fs::create_dir_all(&ui_out).unwrap();

    let mut blps: Vec<PathBuf> = fs::read_dir("data/ui")
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "blp"))
        .collect();
    blps.sort();
    let status = Command::new("blueprint-compiler")
        .arg("batch-compile")
        .arg(&ui_out)
        .arg("data/ui")
        .args(&blps)
        .status()
        .expect("blueprint-compiler not found; install it (dnf install blueprint-compiler)");
    assert!(status.success(), "blueprint-compiler failed");

    // The gresource names the compiled ui/*.ui, the stylesheet and the icons relative to
    // one directory, so everything is gathered here first.
    copy_dir(Path::new("data/icons"), &resource_dir.join("icons"));
    for file in ["blink.gresource.xml", "style.css"] {
        fs::copy(Path::new("data").join(file), resource_dir.join(file)).unwrap();
    }
    glib_build_tools::compile_resources(
        &[resource_dir.to_str().unwrap()],
        resource_dir.join("blink.gresource.xml").to_str().unwrap(),
        "blink.gresource",
    );

    // The settings schema, compiled for the tests, which read it from here with a memory
    // backend whatever is installed on the machine.
    let schemas = out.join("schemas");
    fs::create_dir_all(&schemas).unwrap();
    fs::copy(
        "data/io.github.sachesi.blink.gschema.xml",
        schemas.join("io.github.sachesi.blink.gschema.xml"),
    )
    .unwrap();
    let status = Command::new("glib-compile-schemas")
        .arg(&schemas)
        .status()
        .expect("glib-compile-schemas not found; it comes with GLib");
    assert!(status.success(), "glib-compile-schemas failed");

    println!("cargo:rerun-if-changed=data/ui");
    println!("cargo:rerun-if-changed=data/icons");
    println!("cargo:rerun-if-changed=data/blink.gresource.xml");
    println!("cargo:rerun-if-changed=data/style.css");
    println!("cargo:rerun-if-changed=data/io.github.sachesi.blink.gschema.xml");
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            fs::copy(e.path(), to).unwrap();
        }
    }
}
