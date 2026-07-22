use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    emit_build_metadata();

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

fn emit_build_metadata() {
    println!("cargo:rerun-if-env-changed=HYPERX_BUILD_REVISION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = fs::read_to_string(".git/HEAD") {
        if let Some(ref_path) = head.strip_prefix("ref: ").map(str::trim) {
            println!("cargo:rerun-if-changed=.git/{ref_path}");
        }
    }

    let revision = env::var("HYPERX_BUILD_REVISION")
        .ok()
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
        .filter(|revision| !revision.is_empty());

    if let Some(revision) = revision {
        println!("cargo:rustc-env=HYPERX_BUILD_REVISION={revision}");
    }
}
