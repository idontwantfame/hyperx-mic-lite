use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom},
    process,
};

use crate::paths::log_file_path;

pub(crate) fn run_logs_command(args: &[String]) {
    if args.is_empty() {
        logs_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "path" => {
            println!("{}", log_file_path().display());
            Ok(())
        }
        "tail" => {
            let lines = args
                .get(1)
                .map(|value| value.parse::<usize>())
                .transpose()
                .unwrap_or_else(|_| {
                    eprintln!("Line count must be a whole number.");
                    process::exit(2);
                })
                .unwrap_or(80);
            tail_log(lines)
        }
        _ => {
            logs_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn logs_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite logs path\n\
  hyperx-mic-lite logs tail [lines]"
    );
}

fn tail_log(lines: usize) -> Result<(), String> {
    let path = log_file_path();
    let mut file = OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?
        .len();
    let start = len.saturating_sub(64 * 1024);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut output = text.lines().rev().take(lines).collect::<Vec<_>>();
    output.reverse();
    for line in output {
        println!("{line}");
    }
    Ok(())
}
