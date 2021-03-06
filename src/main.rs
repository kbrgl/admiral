extern crate toml;
extern crate clap;

use std::process::{Command, exit, Stdio};
use std::io::{stderr, Write, Read, BufRead, BufReader};
use std::sync::mpsc::{channel, Sender};
use std::fs::File;
use std::path::PathBuf;
use std::thread::{self, sleep};
use std::time::Duration;
use std::env;
use std::ffi::OsStr;

use toml::Value;
use clap::{App, Arg};

#[derive(Debug)]
struct Update {
    position: usize,
    message: String,
}

fn if_readable(path: PathBuf) -> Option<PathBuf> { if path.exists() { Some(path) } else { None } }

fn get_config_file() -> Option<PathBuf> {
    let xdg_path = env::var("XDG_CONFIG_HOME").ok()
        .map(|v| PathBuf::from(v).join("admiral.d").join("admiral.toml"))
        .and_then(if_readable);

    let dot_home = env::var("HOME").ok()
        .map(|v| PathBuf::from(v).join(".config").join("admiral.d").join("admiral.toml"))
        .and_then(if_readable);

    xdg_path.or(dot_home)
}

fn execute_script(section_name: &str, config_root: PathBuf, configuration: Option<&toml::Table>, position: usize, sender: Sender<Update>,) {
    let _ = env::set_current_dir(&config_root);
    let configuration = configuration.expect(&format!("Failed to find valid section for {}", section_name));
    let command = match configuration.get("path") {
        Some(value) => {
            let value = value.to_owned();
            match value {
                toml::Value::Array(_) => {
                    let _ = stderr().write(format!("Invalid path found for {}: arrays are deprecated - use a string instead\n", section_name).as_bytes());

                    panic!();
                },

                toml::Value::String(string) => {
                    string
                },

                _ => {
                    let _ = stderr().write(format!("Invalid path found for {}\n", section_name).as_bytes());
                    panic!();
                },
            }
        },
        None => {
            let _ = stderr().write(format!("No path found for {}\n", section_name).as_bytes());
            panic!();
        },
    };

    let is_static: bool = match configuration.get("static").and_then(Value::as_bool) {
        Some(value) => value,
        None => false,
    };

    let duration: Option<u64> = match configuration.get("reload") {
        Some(value) => {
            let value = value.to_owned();
            match value {
                toml::Value::Float(float) => {
                    Some((float * 1000f64) as u64)
                }
                toml::Value::Integer(int) => {
                    Some((int as f64 * 1000f64) as u64)
                },
                _ => None,
            }
        },
        None => None
    };

    let shell = match configuration.get("shell") {
        Some(value) => {
            let value = value.to_owned();
            match value {
                toml::Value::String(string) => {
                    string
                },
                _ => {
                    let _ = stderr().write(format!("Invalid shell found for {}\n", section_name).as_bytes());
                    panic!()
                }
            }
        },
        None => {
            match env::var("SHELL").ok() {
                Some(sh) => {
                    sh
                },
                None => {
                    let _ = stderr().write("Could not find your system's shell. Make sure the $SHELL variable is set.\n".as_bytes());
                    panic!()
                }
            }
        }
    };

    let shell = OsStr::new(&shell);

    let arguments = &["-c", &command];

    if is_static {
        let output = Command::new(&shell).args(arguments).output().expect(&format!("Failed to run {}", &command));
        let _ = sender.send(Update { position: position, message: String::from_utf8_lossy(&output.stdout).trim_matches(&['\r', '\n'] as &[_]).to_owned(), });
    } else {
        match duration {
            Some(time) => {
                loop {
                    let output = Command::new(&shell).args(arguments).output().expect(&format!("Failed to run {}", &command));
                    let _ = sender.send(Update { position: position, message: String::from_utf8_lossy(&output.stdout).trim_matches(&['\r', '\n'] as &[_]).to_owned(), });
                    sleep(Duration::from_millis(time));
                }
            },
            None => {
                loop {
                    let output = Command::new(&shell).args(arguments).stdout(Stdio::piped()).spawn().expect(&format!("Failed to run {}", &command));
                    let reader = BufReader::new(output.stdout.unwrap());
                    for line in reader.lines().flat_map(Result::ok) {
                        let _ = sender.send(Update { position: position, message: line.trim_matches(&['\r', '\n'] as &[_]).to_owned(), });
                    }
                    sleep(Duration::from_millis(10));
                }
            },
        }
    }
}

fn main() {
    let matches = App::new("admiral")
        .arg(Arg::with_name("config")
             .help("Specify alternate config file")
             .short("c")
             .long("config-file")
             .takes_value(true))
        .get_matches();

    let config_file = match matches.value_of("config") {
        Some(file) => PathBuf::from(file),
        None => {
            match get_config_file() {
                Some(file) => file,
                None => {
                    let _ = stderr().write("Configuration file not found\n".as_bytes());
                    exit(1);
                },
            }
        }
    };

    if ! config_file.is_file() {
        let _ = stderr().write("Invalid configuration file specified\n".as_bytes());
        exit(1);
    }

    let config_root = PathBuf::from(&config_file.parent().unwrap());

    let mut buffer = String::new();
    if let Ok(mut file) = File::open(&config_file) {
        file.read_to_string(&mut buffer).expect("Could not read configuration file");
    }

    let config_toml = match toml::Parser::new(&buffer).parse() {
        Some(val) => val,
        None => {
            let _ = stderr().write("Syntax error in configuration file.\n".as_bytes());
            panic!();
        }
    };

    let admiral_config = config_toml.get("admiral").unwrap();
    let items = admiral_config.as_table().unwrap().get("items").unwrap().as_slice().unwrap().iter().map(|x| x.as_str().unwrap()).collect::<Vec<_>>();

    let (sender, receiver) = channel::<Update>();

    let mut message_vec: Vec<String> = Vec::new();
    let mut print_message = String::new();

    let mut position: usize = 0;
    for value in items {
        match config_toml.get(value) {
            Some(script) => {
                // Annoying stuff because of how ownership works with closures
                let script = script.to_owned();
                let value = value.to_owned();
                let config_root = config_root.clone();
                let clone = sender.clone();

                let _ = thread::spawn(move || {
                    execute_script(&value, config_root, script.as_table(), position, clone);
                });

                position += 1;
                message_vec.push(String::new());
            },
            None => {
                let _ = stderr().write(format!("No {} found\n", value).as_bytes());
                continue;
            },
        }
    }

    for line in receiver.iter() {
        let position = line.position;
        message_vec[position] = line.message;
        if print_message != message_vec.iter().cloned().collect::<String>() {
            print_message = message_vec.iter().cloned().collect::<String>();
            sleep(Duration::from_millis(5));
            println!("{}", print_message);
        }
    }
}
