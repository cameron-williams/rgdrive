extern crate clap;
use clap::{App, Arg};

mod lib;
use lib::{DCommand, DResult, DSocket, TrackedFile, config_dir, SOCKET_PATH};

use std::env;

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use std::fs::File;
use std::io::prelude::*;
use std::io::Error;

const ANSI_GREEN: &str = "\x1B[32m";
const ANSI_RED: &str = "\x1B[31m";
const ANSI_BLUE: &str = "\x1B[34m";
const ANSI_RESET: &str = "\x1B[0m";
const STDERR_PATH: &str = "/tmp/rgdrived.err";

// Gets the bin path of the daemon binary. (assumes it's in the same path as this bin).
fn get_bin_path() -> String {
    let bin_dir = env::current_exe().unwrap();
    let parent = bin_dir.parent().unwrap();
    let mut pb = parent.to_path_buf();
    pb.push("rgdrived");
    String::from(pb.to_str().unwrap())
}

// Check if the daemon is active and listening. (any unixstream err is assumed not active)
fn daemon_is_active() -> bool {
    if let Err(_) = UnixStream::connect(SOCKET_PATH) {
        false
    } else {
        true
    }
}

// Quick fmt function for errors. Pass an identifier (e.g "push_err" for push function) and the err msg and it will auto color and format.
fn fmt_err<I: AsRef<str>, M: AsRef<str>>(identifier: I, message: M) {
    eprintln!(
        "{}",
        format!(
            "{}rgdrive {}{} {}",
            ANSI_RED,
            identifier.as_ref(),
            ANSI_RESET,
            message.as_ref()
        )
        .as_str()
    );
}

// Maybe add as a method to DResult instead of a separate function? dresult.format()
fn fmt_result(r: DResult) {
    match r {
        DResult::Ok(s) => {
            println!("{}OK:{} {}", ANSI_GREEN, ANSI_RESET, s);
        }
        DResult::Err(e) => {
            eprintln!("{}ERR:{} {}", ANSI_RED, ANSI_RESET, e);
        }
    }
}

/// Starts the daemon process with proper settings.
fn start_daemon() {
    println!("Starting daemon.");
    // Ensure client id and secret are set in $ENV.
    let (client_id, secret) = match (
        env::var("GOOGLE_CLIENT_ID"),
        env::var("GOOGLE_CLIENT_SECRET"),
    ) {
        (Ok(id), Ok(secret)) => (id, secret),
        (Ok(_), _) => {
            fmt_err("start_error", "$GOOGLE_CLIENT_SECRET is not set");
            return;
        }
        (_, Ok(_)) => {
            fmt_err("start_error", "$GOOGLE_CLIENT_ID is not set");
            return;
        }
        (_, _) => {
            fmt_err(
                "start_error",
                "$GOOGLE_CLIENT_ID and $GOOGLE_CLIENT_SECRET are not set",
            );
            return;
        }
    };

    if !daemon_is_active() {
        unsafe {
            Command::new(get_bin_path())
                .env_clear()
                .env("RUST_LOG", "debug")
                .env("HOME", env::var("HOME").unwrap())
                .env("GOOGLE_CLIENT_ID", client_id)
                .env("GOOGLE_CLIENT_SECRET", secret)
                .pre_exec(|| {
                    let pid_t = libc::setsid();
                    if pid_t < 0 {
                        return Err(Error::from_raw_os_error(pid_t));
                    }
                    libc::umask(0);
                    Ok(())
                })
                .current_dir("/")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(File::create(STDERR_PATH).unwrap())
                .spawn()
                .expect("failed to init command");
        }
    } else {
        println!("daemon already running");
    }
}

fn main() {
    // Todo maybe move app config to yaml file or something?
    let matches = App::new("rgdrive")
        .version("1.0")
        .author("Cameron W. <cam@camwilliams.ca>")
        .arg(
            Arg::with_name("start")
                .long("start")
                .help("Start the background daemon.")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("stop")
                .long("stop")
                .help("Stop the background daemon.")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("status")
                .long("status")
                .help("Check the current status of the background daemon.")
        )
        .arg(
            Arg::with_name("pull")
                .long("pull")
                .value_names(&["gdrive_url", "/path/to/file"])
                .number_of_values(2)
                .help("Pull specified drive_url to given path, and sync it's contents.")
        )
        .arg(
            Arg::with_name("push")
                .long("push")
                .takes_value(true)
                .value_name("/path/to/file")
                .help("Push given file to drive, and sync it's contents.")
        )
        .arg(Arg::with_name("msg").long("msg").takes_value(true))
        .arg(
            Arg::with_name("overwrite")
                .long("overwrite")
                .takes_value(false)
                .help("Optional flag to overwrite file contents when pulling a file if it already exists.")
        )
        .arg(
            Arg::with_name("log")
                .long("log")
                .takes_value(false) // maybe change to take a value to limit log lines? --log 5 -> last 5 log lines
                .help("Optional flag to display daemon log.")
        )
        .arg(
            Arg::with_name("list")
                .long("list")
                .takes_value(false)
                .help("List all currently synced paths.")
        )
        .arg(
            Arg::with_name("sync")
                .long("sync")
                .value_names(&["/path/to/file", "drive_url"])
                .number_of_values(2)
                .help("Manually add a sync between given path and drive url.")
        )
        .arg(
            Arg::with_name("unsync")
                .long("unsync")
                .value_name("/path/to/file")
                .takes_value(true)
                .help("Manually remove any syncs for given path.")
        )
        .get_matches();

    let socket = DSocket::new(SOCKET_PATH);

    // Starts the daemon. Put all fds to null except stderr which gets written to STDERR_PATH.
    // Todo:// maybe add a 2nd fork so the forked process isn't it's sesssion leader?
    if matches.occurrences_of("start") > 0 {
        start_daemon();
        return;
    }

    // Print current daemon status and daemon logs to stdout.
    if matches.occurrences_of("status") > 0 {
        let status = match socket.is_active() {
            true => format!("{}running{}", ANSI_GREEN, ANSI_RESET),
            false => format!("{}stopped{}", ANSI_RED, ANSI_RESET),
        };
        println!("Daemon status: {}", status);
        return;
    }

    if matches.occurrences_of("log") > 0 {
        let mut f: File = File::open(STDERR_PATH).unwrap();
        let mut lines: String = String::new();
        f.read_to_string(&mut lines).unwrap();
        println!("{}", lines);
        return;
    }

    // Any further functions require an active daemon. Check here and error out if not active.
    if !socket.is_active() {
        fmt_err(
            "error",
            "Daemon is not active, Please start it with `rgdrive --start`",
        );
        return;
    }

    // Stops the daemon process.
    if matches.occurrences_of("stop") > 0 {
        let result = socket.send_command(DCommand::Quit).unwrap();
        fmt_result(result);
        return;
    }

    // Testing function, write a msg to the daemon.
    if let Some(m) = matches.value_of("msg") {
        let msg = m.to_string();
        if m.contains("ping") {
            fmt_result(socket.send_command(DCommand::Message(msg)).unwrap());
        } else {
            socket
                .send_command_no_response(DCommand::Message(msg))
                .unwrap();
        }
    }

    // Handles push command.
    if let Some(p) = matches.value_of("push") {
        let path = PathBuf::from(p);
        fmt_result(socket.send_command(DCommand::Push(path)).unwrap());
    }

    // Handles pull command.
    if let Some(v) = matches.values_of("pull") {
        let vals: Vec<&str> = v.collect();
        let overwrite = matches.occurrences_of("overwrite") == 1;
        fmt_result(
            socket
                .send_command(DCommand::Pull(
                    vals[0].to_string(),
                    PathBuf::from(vals[1]),
                    overwrite,
                ))
                .unwrap(),
        );
    }

    // Handles list command.
    if matches.occurrences_of("list") > 0 {
        // Iterate all Trackedfiles and prettyprint them.
        let files = TrackedFile::from_path(config_dir());
        println!("Synced files:");
        for tf in &files {
            println!(
                "{green}{:?}{end} {blue}->{end} {green}{:?}{end}",
                tf.path,
                tf.drive_url,
                green = ANSI_GREEN,
                blue = ANSI_BLUE,
                end = ANSI_RESET
            );
        }
    }

    // Handle sync command.
    if let Some(v) = matches.values_of("sync") {
        let vals: Vec<&str> = v.collect();
        fmt_result(
            socket
                .send_command(DCommand::FSync(PathBuf::from(vals[0]), vals[1].to_string()))
                .unwrap(),
        )
    }

    // Handle unsync command.
    if let Some(p) = matches.value_of("unsync") {
        fmt_result(
            socket
                .send_command(DCommand::FUnSync(PathBuf::from(p)))
                .unwrap(),
        );
    }
}
