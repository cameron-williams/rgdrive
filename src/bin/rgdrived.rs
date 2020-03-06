// rgdrived is the daemon that powers the backend and syncing of rgdrive.
#[macro_use] extern crate log;


use std::path::{Path, PathBuf};

use std::os::unix::net::{UnixStream, UnixListener};
use std::fs;
use std::io::prelude::*;
use std::process;

use std::sync::{Arc, Mutex};

use inotify::{Inotify, WatchMask};
use google_api::Drive;



const SOCKET_PATH: &str = "/tmp/rgdrive.sock";


fn get_stream_text(s: &mut UnixStream) -> String {
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    resp
}


fn handle_stream(stream: &mut UnixStream, drive: &Drive, notify: &mut Inotify) {
    let text = get_stream_text(stream);
    if text.len() == 0 {
        return
    }
    let cmd: Vec<&str> = text.split(">").collect();
    debug!("Cmd: {:?}", cmd);

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
            match notify.add_watch(cmd[1], WatchMask::MODIFY | WatchMask::DELETE) {
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
        Ok(d) => d,
        Err(e) => {
            error!("Error initializing Drive API client: {:#?}. Unable to continue.", e);
            process::exit(1);
        }
    };

    // Initialize inotify wrapper for adding new watches.
    let mut inotify = match Inotify::init() {
        Ok(i) => i,
        Err(e) => {
            error!("Error initializing Inotify wrapped: {:#?}. Unable to continue", e);
            process::exit(1);
        }
    };


    // Listen for and handle incoming streams on the socket.
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                handle_stream(&mut s, &drive, &mut inotify);
            },
            Err(e) => {
                error!("stream err: {:?}", e);
                // maybe switch break to process::quit?
                break
            }
        }
    }
}