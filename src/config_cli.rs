use std::{
    path::{Path, PathBuf},
    process,
};

use crate::{
    config::{
        AppConfig, export_config, import_config, load_or_create_config, reset_config,
        validate_config_file,
    },
    paths::config_path,
};

pub(crate) fn run_config_command(args: &[String]) {
    if args.is_empty() {
        config_usage();
        process::exit(2);
    }

    let result = match args[0].as_str() {
        "path" => {
            println!("{}", config_path().display());
            Ok(())
        }
        "dump" => match load_or_create_config() {
            Ok(config) => print_config_json(&config),
            Err(error) => Err(error.to_string()),
        },
        "export" => {
            if args.len() != 2 {
                Err("Usage: hyperx-mic-lite config export <file>".to_string())
            } else {
                export_config(Path::new(&args[1])).map_err(|error| error.to_string())
            }
        }
        "import" => {
            if args.len() != 2 {
                Err("Usage: hyperx-mic-lite config import <file>".to_string())
            } else {
                import_config(Path::new(&args[1])).map_err(|error| error.to_string())
            }
        }
        "validate" => {
            let path = if args.len() == 2 {
                PathBuf::from(&args[1])
            } else {
                config_path()
            };
            validate_config_file(&path).map_err(|error| error.to_string())
        }
        "reset" => reset_config().map_err(|error| error.to_string()),
        _ => {
            config_usage();
            process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn config_usage() {
    eprintln!(
        "Usage:\n\
  hyperx-mic-lite config path\n\
  hyperx-mic-lite config dump\n\
  hyperx-mic-lite config export <file>\n\
  hyperx-mic-lite config import <file>\n\
  hyperx-mic-lite config validate [file]\n\
  hyperx-mic-lite config reset"
    );
}

fn print_config_json(config: &AppConfig) -> Result<(), String> {
    let text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}
