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

    let windmc_status = match Command::new(&windmc)
        .arg("-h")
        .arg(&out_dir)
        .arg("-r")
        .arg(&out_dir)
        .arg(&message_file)
        .status()
    {
        Ok(status) => status,
        Err(error) => {
            println!(
                "cargo:warning=skipping Event Viewer message resource: failed to run {windmc}: {error}"
            );
            return;
        }
    };
    if !windmc_status.success() {
        println!(
            "cargo:warning=skipping Event Viewer message resource: {windmc} exited with {windmc_status}"
        );
        return;
    }

    let resource_script = out_dir.join("hyperx_messages.rc");
    let resource_object = out_dir.join("hyperx_messages.o");
    let windres_status = match Command::new(&windres)
        .arg("-i")
        .arg(&resource_script)
        .arg("-o")
        .arg(&resource_object)
        .status()
    {
        Ok(status) => status,
        Err(error) => {
            println!(
                "cargo:warning=skipping Event Viewer message resource: failed to run {windres}: {error}"
            );
            return;
        }
    };
    if !windres_status.success() {
        println!(
            "cargo:warning=skipping Event Viewer message resource: {windres} exited with {windres_status}"
        );
        return;
    }

    println!("cargo:rustc-link-arg={}", resource_object.display());
}
