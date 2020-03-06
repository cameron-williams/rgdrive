// rgdrived is the daemon that powers the backend and syncing of rgdrive.
#[macro_use] extern crate log;

use std::env;
use std::path::{Path, PathBuf};

use std::os::unix::net::{UnixStream, UnixListener};
use std::fs;
use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::io::prelude::*;
use std::process;

use std::collections::HashSet;

use std::sync::{Arc, Mutex};
use std::thread;

use std::time::Duration;

use inotify::{Inotify, WatchMask, EventMask};
use google_api::Drive;

use serde_json::Value;



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


fn handle_stream(stream: &mut UnixStream, drive: Arc<Mutex<Drive>>, notify: Arc<Mutex<Inotify>>) {
    let text = get_stream_text(stream);
    if text.len() == 0 {
        return
    }
    let cmd: Vec<&str> = text.split(">").collect();
    debug!("Cmd: {:?}", cmd);

    let drive = drive.lock().unwrap();

    // Match command to requested action.
    match cmd[0] {

        "quit" => {
            info!("Quit command received. Quitting dameon.");
            process::exit(0);
        },

        "push" => {
            debug!("Push command received: {:?}", cmd);
            // Upload file to drive.
            match drive.upload_file(
                PathBuf::from(cmd[1])
            ) {
                Ok(_) => info!("uploaded {} successfully", cmd[1]),
                Err(e) => {
                    error!("failed to upload {}: {:?}", cmd[1], e);
                    return
                },
            }
            // After a successful upload, add path to Inotify watch list for modify/delete masks.
            match notify.lock().unwrap().add_watch(cmd[1], WatchMask::MODIFY | WatchMask::DELETE) {
                Ok(_) => debug!("{} added to Inotify watchlist successfully", cmd[1]),
                Err(e) => error!("failed to add {} to the Inotify watchlist: {:?}", cmd[1], e),
            };
        },

        "pull" => {
            debug!("Pull command received: {:?}", cmd);
            match drive.download_file(
                cmd[1],
                PathBuf::from(cmd[2])
            ) {
                Ok(_) => info!("downloaded {} successfully", cmd[1]),
                Err(e) => {
                    error!("failed to download {}: {:?}", cmd[1], e);
                    return
                },
            }
        }

        _ => ()
    }
}



struct InotifyTracker {
    notify: Arc<Mutex<Inotify>>,
    tracked_files: HashSet<String>,
}

impl InotifyTracker {
    fn new(n: Arc<Mutex<Inotify>>) -> Self {
        // On init, load any tracked files from config.
        let mut t = Self {
            notify: n,
            tracked_files: HashSet::new()
        };
        t.tracked_files = InotifyTracker::read_from_config().unwrap();
        for p in &t.tracked_files {
            match t.notify.lock().unwrap().add_watch(p, WatchMask::MODIFY | WatchMask::DELETE) {
                Ok(_) => debug!("{} added to Inotify watchlist successfully", p),
                Err(e) => error!("failed to add {} to the Inotify watchlist: {:?}", p, e),
            };
            info!("tracked path: {}", p);
        }
        t
    }

    fn read_from_config() -> Result<HashSet<String>, String> {
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
            return Ok(HashSet::new());
        }
        // Open file as readonly, and read vec of pathnames from file.
        match OpenOptions::new()
            .read(true)
            .write(false)
            .open(config_dir())
        {
            Ok(f) => {
                let reader = BufReader::new(f);
                match serde_json::from_reader(reader) {
                    Ok(d) => Ok(d),
                    Err(_) => Ok(HashSet::new()),
                }
            }
            Err(e) => panic!(format!("error reading from config file: {:#?}", e)),
        }
    }

    fn write_path_to_config(&self) -> Result<(), String> {
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
        // Write current state of self GoogleOAuthToken to file as json.
        match OpenOptions::new()
            .read(true)
            .write(true)
            .truncate(true)
            .open(path)
        {
            Ok(f) => {
                let writer = BufWriter::new(f);
                if let Err(e) = serde_json::to_writer_pretty(writer, &self.tracked_files) {
                    panic!(format!(
                        "error writing/serializing config to file: {:#?}",
                        e
                    ));
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(format!("error opening config file in write mode: {:#?}", e)),
        }
    }

    fn listen(&mut self) {
        let mut buffer = [0; 1024];
        loop {
            let events = self.notify.lock().unwrap().read_events(&mut buffer).expect("Failed to read inotify events");
            debug!("Checking for events...");

            for event in events {
                match event.mask {
                    EventMask::MODIFY => {
                        info!("modify event for {:?}", event.name);
                        info!("modify event: {:#?}", event);
                    },
                    EventMask::DELETE => {},
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
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
            return
        }
    };
    info!("Daemon initialized");

    // Initialize gdrive api client.
    let drive = match Drive::new(
        String::from("1979642470-56gn3t87ibds6kllqp6rqu09im00qj5i.apps.googleusercontent.com"),
        String::from("PekwZar1ZaqSjWRz7A2PMkaF"),
        None
    ) {
        Ok(d) => Arc::new(Mutex::new(d)),
        Err(e) => {
            error!("Error initializing Drive API client: {:#?}. Unable to continue.", e);
            process::exit(1);
        }
    };

    // Initialize inotify wrapper for adding new watches.
    let inotify: Arc<Mutex<Inotify>> = match Inotify::init() {
        Ok(i) => Arc::new(Mutex::new(i)),
        Err(e) => {
            error!("Error initializing Inotify wrapped: {:#?}. Unable to continue", e);
            process::exit(1);
        }
    };

    // Spawn a new thread which listens for and handles Inotify events.
    let inotify_clone = Arc::clone(&inotify);
    let drive_clone = Arc::clone(&drive);
    thread::spawn(move || {
        // inotify_listener(inotify_clone, drive_clone)
        let mut t = InotifyTracker::new(inotify_clone);
        t.listen();
        // t.tracked_files.remove(&String::from("/home/cam/testfile2.txt"));
        // t.write_path_to_config().unwrap();

    });

    // Listen for and handle incoming streams on the socket.
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                handle_stream(&mut s, Arc::clone(&drive), Arc::clone(&inotify));
            },
            Err(e) => {
                error!("stream err: {:?}", e);
                // maybe switch break to process::quit?
                break
            }
        }
    }
}