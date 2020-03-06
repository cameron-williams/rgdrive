// rgdrived is the daemon that powers the backend and syncing of rgdrive.
#[macro_use]
extern crate log;

use std::env;
use std::path::{Path, PathBuf};

use std::fs;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::prelude::*;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process;

use std::collections::{HashMap, HashSet};

use std::sync::{Arc, Mutex};
use std::thread;

use std::time::Duration;

use google_api::Drive;
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

const SOCKET_PATH: &str = "/tmp/rgdrive.sock";
const CONFIG_PATH: &str = "/.config/cameron-williams/tracked_files";

fn config_dir() -> PathBuf {
    let mut dir = env::var("HOME").expect("$HOME not set");
    dir.push_str(CONFIG_PATH);
    PathBuf::from(dir)
}

fn get_stream_text(s: &mut UnixStream) -> String {
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    resp
}

fn handle_stream(
    stream: &mut UnixStream,
    drive: Arc<Mutex<Drive>>,
    notify: Arc<Mutex<Inotify>>,
    tracked_files: Arc<Mutex<TrackedFiles>>,
) {
    let text = get_stream_text(stream);
    if text.len() == 0 {
        return;
    }
    let cmd: Vec<&str> = text.split(">").collect();
    debug!("Cmd: {:?}", cmd);

    let drive = drive.lock().unwrap();

    // Match command to requested action.
    match cmd[0] {
        "quit" => {
            info!("Quit command received. Quitting dameon.");
            process::exit(0);
        }

        "push" => {
            debug!("Push command received: {:?}", cmd);
            // Upload file to drive.
            let url = match drive.upload_file(PathBuf::from(cmd[1])) {
                Ok(url) => {
                    info!("uploaded {} successfully", cmd[1]);
                    url
                }
                Err(e) => {
                    error!("failed to upload {}: {:?}", cmd[1], e);
                    return;
                }
            };
            // After a successful upload, add path to TrackedFiles for modify/delete masks.
            tracked_files.lock().unwrap().add_path(notify, cmd[1], url);
        }

        "pull" => {
            debug!("Pull command received: {:?}", cmd);
            match drive.download_file(cmd[1], PathBuf::from(cmd[2])) {
                Ok(_) => info!("downloaded {} successfully", cmd[1]),
                Err(e) => {
                    error!("failed to download {}: {:?}", cmd[1], e);
                    return;
                }
            }
        }

        _ => (),
    }
}

/// Write paths that need to be tracked to config file from TrackedFiles.map (HashMap<wd, String>)
fn write_saved_inotify_events(e: &HashMap<WatchDescriptor, String>) {
    // Ensure config path exists. If it doesn't create it.
    let path = config_dir();
    if !path.exists() {
        match create_dir_all(path.parent().unwrap()) {
            Ok(_) => {
                if let Err(e) = File::create(&path) {
                    panic!(format!("failed to create new config file: {:#?}", e));
                }
            }
            Err(e) => panic!(format!("failed to create config dir: {:#?}", e)),
        }
    }
    let mut vals = Vec::new();
    e.values().for_each(|i| vals.push(i));
    match OpenOptions::new()
        .read(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        Ok(f) => {
            let writer = BufWriter::new(f);
            if let Err(e) = serde_json::to_writer_pretty(writer, &vals) {
                panic!(format!(
                    "error writing/serializing config to file: {:#?}",
                    e
                ));
            }
        }
        Err(e) => panic!(format!("error opening config file in write mode: {:#?}", e)),
    }
}

fn inotify_listen(
    notify: Arc<Mutex<Inotify>>,
    drive: Arc<Mutex<Drive>>,
    tracked_files: Arc<Mutex<TrackedFiles>>,
) {
    let mut buffer = [0; 1024];
    loop {
        let events = notify
            .lock()
            .unwrap()
            .read_events(&mut buffer)
            .expect("Failed to read inotify events");

        for event in events {
            match event.mask {
                EventMask::MODIFY => {
                    let entry = tracked_files.lock().unwrap();
                    let path: Vec<&str> = match entry.map.get(&event.wd) {
                        Some(p) => p.split(",").collect(),
                        None => {
                            error!("no matching entry in saved wds for {:?}", event.wd);
                            continue;
                        }
                    };
                    debug!("modify event: {:#?}\nEntry: {:#?}", event.wd, path);
                    match drive
                        .lock()
                        .unwrap()
                        .update_file(PathBuf::from(path[0]), path[1])
                    {
                        Ok(_) => info!("Successfully updated file: {:?}", path),
                        Err(e) => error!("Error updating file {:?}: {:?}", path, e),
                    }
                }
                EventMask::DELETE => {}
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}

struct TrackedFiles {
    // Holds {WD: String("path,drive_url")
    map: HashMap<WatchDescriptor, String>,
}

impl TrackedFiles {
    // Init tracked files, loading any from saved config.
    fn from_config(notify: Arc<Mutex<Inotify>>) -> Self {
        // Ensure config path exists. If it doesn't create it and return a blank value
        let path = config_dir();
        if !path.exists() {
            match create_dir_all(path.parent().unwrap()) {
                Ok(_) => {
                    if let Err(e) = File::create(&path) {
                        panic!(format!("failed to create new config file: {:#?}", e));
                    }
                }
                Err(e) => panic!(format!("failed to create config dir: {:#?}", e)),
            }
        }
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

        let mut map: HashMap<WatchDescriptor, String> = HashMap::new();

        for p in &paths {
            let vals: Vec<&str> = p.split(",").collect();
            match notify
                .lock()
                .unwrap()
                .add_watch(vals[0], WatchMask::MODIFY | WatchMask::DELETE)
            {
                Ok(wd) => {
                    debug!("{} added to Inotify watchlist successfully", p);
                    map.insert(wd, p.to_string())
                }
                Err(e) => {
                    error!("failed to add {} to the Inotify watchlist: {:?}", p, e);
                    continue;
                }
            };
        }
        info!("{:#?}", map);

        Self { map }
    }

    /// Adds given path to Inotify watch, as well as tracked files map.
    fn add_path<P: Into<String>, U: Into<String>>(
        &mut self,
        notify: Arc<Mutex<Inotify>>,
        path: P,
        url: U,
    ) {
        let path = path.into();
        let url = url.into();
        match notify
            .lock()
            .unwrap()
            .add_watch(path.clone(), WatchMask::MODIFY | WatchMask::DELETE)
        {
            Ok(wd) => {
                debug!("{} added to Inotify watchlist successfully", path);
                self.map.insert(wd, format!("{},{}", path, url));
            }
            Err(e) => {
                error!("failed to add {} to the Inotify watchlist: {:?}", path, e);
                return;
            }
        }
        write_saved_inotify_events(&self.map)
    }
}

fn main() {
    env_logger::init();
    // Check if socket exists already, if it does delete it.
    let socket = Path::new(SOCKET_PATH);
    if socket.exists() {
        fs::remove_file(&socket).unwrap()
    }

    // Create unix domain socket on SOCKET_PATH.
    let listener = match UnixListener::bind(&socket) {
        Ok(s) => s,
        Err(e) => {
            error!("Couldn't listen on socket: {:#?}", e);
            return;
        }
    };
    info!("Daemon initialized");

    // Initialize gdrive api client.
    let drive = match Drive::new(
        String::from(env::var("GOOGLE_CLIENT_ID").unwrap()),
        String::from(env::var("GOOGLE_CLIENT_SECRET").unwrap()),
        None,
    ) {
        Ok(d) => Arc::new(Mutex::new(d)),
        Err(e) => {
            error!(
                "Error initializing Drive API client: {:#?}. Unable to continue.",
                e
            );
            process::exit(1);
        }
    };

    // Initialize inotify wrapper for adding new watches.
    let inotify: Arc<Mutex<Inotify>> = match Inotify::init() {
        Ok(i) => Arc::new(Mutex::new(i)),
        Err(e) => {
            error!(
                "Error initializing Inotify wrapped: {:#?}. Unable to continue",
                e
            );
            process::exit(1);
        }
    };

    // Holds all tracked paths.
    let tracked_files = Arc::new(Mutex::new(TrackedFiles::from_config(Arc::clone(&inotify))));

    // Spawn a new thread which listens for and handles Inotify events.
    let inotify_clone = Arc::clone(&inotify);
    let drive_clone = Arc::clone(&drive);
    let tracked_files_c = Arc::clone(&tracked_files);

    thread::spawn(move || {
        inotify_listen(inotify_clone, drive_clone, tracked_files_c);
    });

    // Listen for and handle incoming streams on the socket.
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                handle_stream(
                    &mut s,
                    Arc::clone(&drive),
                    Arc::clone(&inotify),
                    Arc::clone(&tracked_files),
                );
            }
            Err(e) => {
                error!("stream err: {:?}", e);
                // maybe switch break to process::quit?
                break;
            }
        }
    }
}
