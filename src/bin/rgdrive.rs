extern crate clap;
use clap::{App, Arg};

use std::env;

use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use std::fs::File;
use std::io::prelude::*;
use std::io::{stdout, Error, ErrorKind};

use std::path::Path;
use url::Url;

const SOCKET_PATH: &str = "/tmp/rgdrive.sock";

const ANSI_GREEN: &str = "\x1B[32m";
const ANSI_RED: &str = "\x1B[31m";
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

// Write given str or String to daemon socket.
fn write_to_daemon<M: Into<String>>(msg: M) -> Result<(), Error> {
    let mut s = UnixStream::connect(SOCKET_PATH)?;
    s.write_all(msg.into().as_bytes())
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
fn fmt_err<I: Into<String>, M: Into<String>>(identifier: I, message: M) {
    eprintln!(
        "{}",
        format!(
            "{}rgdrive {}{} {}",
            ANSI_RED,
            identifier.into(),
            ANSI_RESET,
            message.into()
        )
        .as_str()
    );
}

/// Starts the daemon process with proper settings.
fn handle_start() {
    println!("Starting daemon.");
    if !daemon_is_active() {
        unsafe {
            Command::new(get_bin_path())
                .env_clear()
                .env("RUST_LOG", "debug")
                .env("HOME", env::var("HOME").unwrap())
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

/// Stops the active daemon.
fn handle_stop() {
    print!("Stopping daemon...");
    stdout().flush().unwrap();
    match write_to_daemon("quit") {
        Err(e) => match e.kind() {
            ErrorKind::ConnectionRefused => print!(" Already stopped.\n"),
            _ => {
                print!(" Error\n");
                eprintln!("Error stopping daemon: {}", e)
            }
        },
        Ok(_) => print!(" Stopped.\n"),
    }
    stdout().flush().unwrap()
}

/// Handler for the file pull command.
/// Expects vals to be Vec<url, local_path>
fn handle_pull(vals: Vec<&str>, overwrite: bool) {
    // Ensure given url is valid.
    if let Err(_) = Url::parse(vals[0]) {
        fmt_err("pull_error", format!("Invalid pull url: {}", vals[0]));
        return;
    };

    let p = Path::new(vals[1]);
    // If destination is a file and exists, and we don't have an overwrite flag, warn user and break.
    if p.is_file() {
        if p.exists() && !overwrite {
            fmt_err(
                "pull_error",
                format!(
                    "Destination {} exists, but no overwrite flag specified. Please rerun with the --overwrite flag to run anyways.",
                    vals[1]
                )
            );
            return;
        }
    } else {
        // If is a dir and doesn't exist warn user and break.
        if p.extension() == None && !p.is_dir() {
            fmt_err(
                "pull_error",
                format!("Destination {} doesn't exist.", vals[1]),
            );
            return;
        }
    }

    write_to_daemon(format!("pull>{}>{}", vals[0], vals[1])).unwrap();
}

/// Handler for the file push command.
/// Expects p to be a path to a file on the localsystem.
/// Will check to ensure it exists.
fn handle_push(p: &str) {
    // Ensure path is valid and that a file exists there.
    let p = Path::new(p);
    if !p.exists() {
        fmt_err(
            "push_error",
            format!(
                "{} doesn't exist. Please check you rpath and try again.",
                p.display()
            ),
        );
        return;
    }
    // Send push command to daemon.
    write_to_daemon(format!("push>{}", p.display())).unwrap()
}

/// Handler for the file status command.
/// Notifies if the daemon is running, as well as prints any logs that it has accumulated.
fn handle_status() {
    // Read rgdrived.err to stdout. Todo:// cut it to only be the last 5-10 lines of logs?
    let mut f = File::open(STDERR_PATH).unwrap();
    let mut log_lines = String::new();
    f.read_to_string(&mut log_lines)
        .expect("failed to read rgdrived.err to string");

    // Add header with daemon status.
    if daemon_is_active() {
        println!("Daemon status: {}Running{}", ANSI_GREEN, ANSI_RESET);
    } else {
        println!("Daemon status: {}Not Running{}", ANSI_RED, ANSI_RESET);
    }

    // Print daemon log lines.
    print!("{}", log_lines);
    stdout().flush().unwrap();
}

fn main() {
    let matches = App::new("rgdrive")
        .version("1.0")
        .author("Cameron W. <cam@camwilliams.ca>")
        .arg(
            Arg::with_name("start") // done (maybe add different print fmt)
                .long("start")
                .help("Start the background daemon")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("stop") // done
                .long("stop")
                .help("Stop the background daemon")
                .takes_value(false),
        )
        .arg(
            Arg::with_name("status") // done
                .long("status")
                .help("check the current status of the background daemon"),
        )
        .arg(
            Arg::with_name("pull")
                .long("pull")
                .value_names(&["gdrive_url", "/local/path/to/put/to"])
                .number_of_values(2),
        )
        .arg(
            Arg::with_name("push")
                .long("push")
                .takes_value(true)
                .value_name("/path/of/file/to/push"),
        )
        .arg(Arg::with_name("msg").long("msg").takes_value(true))
        .arg(
            Arg::with_name("overwrite")
                .long("overwrite")
                .takes_value(false),
        )
        .get_matches();

    // Starts the daemon. Put all fds to null except stderr which gets written to STDERR_PATH.
    // Todo:// maybe add a 2nd fork so the forked process isn't it's sesssion leader?
    if matches.occurrences_of("start") > 0 {
        handle_start();
        return;
    }

    // Print current daemon status and daemon logs to stdout.
    if matches.occurrences_of("status") > 0 {
        handle_status();
        return;
    }

    // Stops the daemon process.
    if matches.occurrences_of("stop") > 0 {
        handle_stop();
        return;
    }

    // Any further functions require an active daemon. Check here and error out if not active.
    if !daemon_is_active() {
        fmt_err(
            "error",
            "Daemon is not active, Please start it with `rgdrive --start`",
        );
        return;
    }

    // Testing function, write a msg to the daemon.
    if let Some(m) = matches.value_of("msg") {
        write_to_daemon(m).unwrap();
    }

    // Handles push command.
    if let Some(p) = matches.value_of("push") {
        handle_push(p);
    }

    // Handles pull command.
    if let Some(v) = matches.values_of("pull") {
        let vals: Vec<&str> = v.collect();
        let overwrite = matches.occurrences_of("overwrite") == 1;
        handle_pull(vals, overwrite);
    }
}
