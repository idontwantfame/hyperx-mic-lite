use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=resources/hyperx_messages.mc");
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let message_file = PathBuf::from("resources/hyperx_messages.mc");
    let windmc = env::var("WINDMC").unwrap_or_else(|_| "windmc".to_string());
    let windres = env::var("WINDRES").unwrap_or_else(|_| "windres".to_string());

    let windmc_status = Command::new(&windmc)
        .arg("-h")
        .arg(&out_dir)
        .arg("-r")
        .arg(&out_dir)
        .arg(&message_file)
        .status()
        .expect("failed to run windmc; set WINDMC to the full windmc.exe path");
    assert!(windmc_status.success(), "windmc failed");

    let resource_script = out_dir.join("hyperx_messages.rc");
    let resource_object = out_dir.join("hyperx_messages.o");
    let windres_status = Command::new(&windres)
        .arg("-i")
        .arg(&resource_script)
        .arg("-o")
        .arg(&resource_object)
        .status()
        .expect("failed to run windres; set WINDRES to the full windres.exe path");
    assert!(windres_status.success(), "windres failed");

    println!("cargo:rustc-link-arg={}", resource_object.display());
}
