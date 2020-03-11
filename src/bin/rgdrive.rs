extern crate clap;
use clap::{App, Arg};

use std::collections::HashSet;
use std::env;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use std::fs::{File, OpenOptions};
use std::io::prelude::*;
use std::io::{stdout, BufReader, Error, ErrorKind};

use std::path::Path;
use url::Url;

const SOCKET_PATH: &str = "/tmp/rgdrive.sock";
const CONFIG_PATH: &str = "/.config/cameron-williams/tracked_files";

const ANSI_GREEN: &str = "\x1B[32m";
const ANSI_RED: &str = "\x1B[31m";
const ANSI_BLUE: &str = "\x1B[34m";
const ANSI_RESET: &str = "\x1B[0m";
const STDERR_PATH: &str = "/tmp/rgdrived.err";

fn config_dir() -> PathBuf {
    let mut dir = env::var("HOME").expect("$HOME not set");
    dir.push_str(CONFIG_PATH);
    PathBuf::from(dir)
}

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

fn is_valid_path<P: Into<PathBuf>>(p: P) -> bool {
    // Ensure path is valid and that a file exists there.
    p.into().exists()
}

/// Starts the daemon process with proper settings.
fn handle_start() {
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
                .env("RUST_LOG", "info")
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
    if !is_valid_path(p) {
        fmt_err(
            "push_error",
            format!("{} doesn't exist. Please check your path and try again.", p),
        );
        return;
    }
    // Send push command to daemon.
    write_to_daemon(format!("push>{}", p)).unwrap()
}

/// Handler for the file status command.
/// Notifies if the daemon is running, as well as prints any logs that it has accumulated.
fn handle_status() {
    // Read rgdrived.err to stdout. Todo:// cut it to only be the last 5-10 lines of logs?
    let mut log_lines = String::new();
    match File::open(STDERR_PATH) {
        Ok(mut f) => {
            f.read_to_string(&mut log_lines)
                .expect("failed to read rgdrive.err to string");
        }
        Err(e) => {
            // Check error, NotFound is fine because that means the daemon just hasn't been run yet. Panic on anything else.
            match e.kind() {
                ErrorKind::NotFound => {}
                _ => {
                    panic!("failed to open stderr path, unknown error: {:?}", e);
                }
            }
        }
    }

    // Add header with daemon status.
    if daemon_is_active() {
        println!("Daemon status: {}Running{}", ANSI_GREEN, ANSI_RESET);
    } else {
        println!("Daemon status: {}Not Running{}", ANSI_RED, ANSI_RESET);
    }

    // Print daemon log lines (if any).
    print!("{}", log_lines);
    stdout().flush().unwrap();
}

/// Handler for list command. This command lists the currently synced files/folders. in
/// the format <path> - <drive url>.
fn handle_list() {
    // Open file as readonly, and read vec of pathnames from file.
    let paths: HashSet<String> = match OpenOptions::new()
        .read(true)
        .write(false)
        .open(config_dir())
    {
        Ok(f) => {
            let reader = BufReader::new(f);
            match serde_json::from_reader(reader) {
                Ok(d) => d,
                Err(_) => HashSet::new(),
            }
        }
        Err(e) => panic!(format!("error reading from config file: {:#?}", e)),
    };
    println!("Synced files:");
    for p in paths {
        // p[0] = path, p[1] = url.
        let p: Vec<&str> = p.split(",").collect();
        println!(
            "{green}{}{end} {blue}->{end} {green}{}{end}",
            p[0],
            p[1],
            green = ANSI_GREEN,
            end = ANSI_RESET,
            blue = ANSI_BLUE
        );
    }
}

/// Handler for manual sync command.
/// Vals is a vec which holds:
/// vals[0] = /path/local/to/sync
/// vals[1] = drive_url to sync to
fn handle_sync(vals: Vec<&str>) {
    // Ensure path is valid and that a file exists there.
    if !is_valid_path(vals[0]) {
        fmt_err(
            "sync_error",
            format!(
                "{} doesn't exist. Please check your path and try again.",
                vals[0]
            ),
        );
        return;
    }

    // Ensure given url is valid.
    if let Err(_) = Url::parse(vals[1]) {
        fmt_err("sync_error", format!("Invalid pull url: {}", vals[1]));
        return;
    };

    write_to_daemon(format!("sync>{}>{}", vals[0], vals[1])).unwrap();
}

/// Handler for manual unsync command.
/// Any current syncs that are synced to given path will be removed from the watcher.
fn handle_unsync(p: &str) {
    if !is_valid_path(p) {
        fmt_err(
            "unsync_error",
            format!("{} doesn't exist. Please check your path and try again.", p),
        );
        return;
    }
    write_to_daemon(format!("unsync>{}", p)).unwrap();
}

fn main() {
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

    // Handles list command.
    if matches.occurrences_of("list") > 0 {
        //
        handle_list();
    }

    // Handle sync command.
    if let Some(v) = matches.values_of("sync") {
        let vals: Vec<&str> = v.collect();
        handle_sync(vals);
    }

    // Handle unsync command.
    if let Some(p) = matches.value_of("unsync") {
        handle_unsync(p)
    }
}
