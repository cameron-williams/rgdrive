// rgdrived is the daemon that powers the backend and syncing of rgdrive.
#[macro_use]
extern crate log;

mod lib;
use lib::{DCommand, DResult, Tracker, SOCKET_PATH};


use std::env;
use std::path::{Path, PathBuf};

use std::fs;
use std::io::Error;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;


use google_api::Drive;
use inotify::EventMask;



// Returns a list of all subpaths in given path. Recursive.
fn get_subpaths(p: &PathBuf) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(p).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            paths.extend(get_subpaths(&path));
        } else if path.is_file() {
            paths.push(path);
        }
    }
    paths
}


fn pull(drive_url: String, path: PathBuf, overwrite: bool, tracker: Arc<Mutex<Tracker>>, drive: Arc<Mutex<Drive>>) -> Result<DResult, Error> {
    // Check if destination path exists, if it does check if we can overwrite it.
    if path.is_file() {
        if path.exists() && !overwrite {
            return Ok(
                DResult::error(
                    format!("Destination {:?} exists but no overwrite flag specified. Rerun with --overwrite to force destination path overwrite.", path)
                )
            );
        }
    } else {
        // Is a dir and doesn't exist, return err.
        if path.extension() == None && !path.is_dir() {
            return Ok(
                DResult::error(format!("Destiation {:?} doesn't exist.", path))
            )
        }
    }

    match drive.lock().unwrap().download_file(&drive_url, path) {
        Ok(path) => {
            info!("Downloaded {} successfully.", drive_url);
            // Add path to tracker.
            tracker.lock().unwrap().add_path(path, &drive_url)?;
            Ok(
                DResult::ok(format!("Pulled {} successfully.", drive_url))
            )
        },
        Err(e) => {
            error!("Error downloading {}: {:?}", drive_url, e);
            Ok(
                DResult::error(
                    format!("Error downloading {}: {:?}. See log for more information,", drive_url, e)
                )
            )
        }
    }

}


// Push given path to Google Drive, and add it to the Inotify watchlist.
fn push(path: PathBuf, tracker: Arc<Mutex<Tracker>>, drive: Arc<Mutex<Drive>>) -> Result<DResult, Error> {
    if !path.exists() {
        return Ok(
            DResult::error(format!("Cannot push path: {:?} does not exist.", path))
        )
    }

    // If given path is a dir, upload everything in it.
    if path.is_dir() {
        let (mut success, mut error): (u16, u16) = (0, 0);
        // Get all subpaths of given dir. Attempt to add them all and keep track of # fails/successes.
        for p in get_subpaths(&path) {
            match drive.lock().unwrap().upload_file(&p) {
                Ok(url) => {
                    info!("Uploaded {:?}: {:?}", p, url);
                    match tracker.lock().unwrap()
                            .add_path(&p, &url) {
                                Ok(_) => {
                                    info!("Added {:?} to tracker", p);
                                    success += 1;
                                },
                                Err(e) => {
                                    error!("Error adding {:?} to tracker: {:?}", p, e);
                                    error += 1;
                                }
                            }
                },
                Err(e) => {
                    error!("Error pushing {:?}: {:?}", p, e);
                    error += 1;
                    continue
                }
            }
        }
        let result_msg = format!("Directory upload status: {} successes, {} fails.", success, error);
        if error > 0 {
            return Ok(DResult::error(result_msg))
        }
        return Ok(DResult::ok(result_msg))

    // Single file path, upload it.
    } else {

        match drive.lock().unwrap().upload_file(&path) {
            Ok(url) => {
                info!("Uploaded {:?}: {:?}", path, url);
                match tracker.lock().unwrap().add_path(&path, &url) {
                    Ok(_) => {
                        info!("Added {:?} to tracked files.", path);
                        return Ok(DResult::ok(format!("Uploaded and synced {:?}.", path)));
                    },
                    Err(e) => {
                        error!("Error adding {:?} to tracked files: {:?}.", path, e);
                        return Ok(DResult::error(format!("Error uploading and syncing {:?}: {:?}", path, e)));
                    }
                }
            },
            Err(e) => {
                let emsg = format!("Failed to upload {:?}: {:?}", path, e);
                error!("{}", emsg);
                return Ok(DResult::error(emsg));
            }
        }
    }

}

// Handle each incoming stream. Deserialize command and perform it. 
fn handle_stream(mut stream: UnixStream, tracker: Arc<Mutex<Tracker>>, drive: Arc<Mutex<Drive>>) {
    // Deserialize command from stream.
    let command: DCommand = DCommand::from_stream(&mut stream);

    // Something with the udsockets causes empty bytes to be sent sometimes, dismiss any empty commands now.
    if let DCommand::None = command {
        return;
    }

    debug!("Got command: {:?}", command);

    // Match command to command handler.
    match command {

        // Handle message command.
        DCommand::Message(msg) => {
            info!("Message from client: {:?}", msg);
            if msg.contains("ping") {
                DResult::Ok(String::from("pong")).send(&mut stream).unwrap();
            }
        },

        // Handles the file pull command.
        DCommand::Pull(drive_url, path, overwrite) => {
            match pull(drive_url, path, overwrite, tracker, drive) {
                Ok(r) => r.send(&mut stream).unwrap(),
                Err(e) => {
                    error!("Unrecoverable pull error: {:?}", e);
                }
            }
        },

        DCommand::Push(path) => {
            match push(path, tracker, drive) {
                Ok(r) => r.send(&mut stream).unwrap(),
                Err(e) => {
                    error!("Unrecoverable push error: {:?}", e);
                }
            }
        },

        DCommand::FSync(path, drive_url) => {
            match tracker.lock().unwrap().add_path(&path, &drive_url) {
                Ok(_) => {
                    let msg = format!("Manual sync added for {:?} -> {:?}", &path, &drive_url);
                    info!("{}", msg);
                    DResult::ok(msg).send(&mut stream).unwrap();
                },
                Err(e) => {
                    let emsg = format!("Failed to add manual sync for {:?} -> {:?}: {:?}", &path, &drive_url, e);
                    error!("{}", emsg);
                    DResult::error(emsg).send(&mut stream).unwrap();
                }

            }
        },

        DCommand::FUnSync(path) => {
            match tracker.lock().unwrap().remove_path(&path) {
                Ok(_) => {
                    let msg = format!("Removed sync for {:?}", &path);
                    info!("{}", msg);
                    DResult::ok(msg).send(&mut stream).unwrap();
                },
                Err(e) => {
                    let emsg = format!("Error removing sync for {:?}: {:?}", &path, e);
                    error!("{}", emsg);
                    DResult::error(emsg).send(&mut stream).unwrap();
                }
            }
        },

        // Handle quit command.
        DCommand::Quit => {
            info!("Received quit command from client. Quitting..");
            DResult::Ok(
                String::from("Daemon stopped.")
            ).send(&mut stream).unwrap();
            process::exit(0);
        }
        _ => {},
    }    
}



/// Listens forever for inotify events.
fn inotify_listen(
    tracker: Arc<Mutex<Tracker>>,
    drive: Arc<Mutex<Drive>>,
) {
    let mut buffer = [0; 1024];
    debug!("waiting for events..");
    loop {
        let events = tracker
            .lock()
            .unwrap()
            .inotify
            .read_events(&mut buffer)
            .expect("Failed to read inotify events");

        for event in events {
            match event.mask {
                // Handle modify events. Find file associated with wd and update it on drive.
                EventMask::MODIFY => {
                    for tf in &tracker.lock().unwrap().tracked_files {
                        if let Some(wd) = &tf.wd {
                            if *wd == event.wd {
                                match drive.lock()
                                            .unwrap()
                                            .update_file(tf.path.clone(), &tf.drive_url) {
                                                Ok(_) => info!("Successfully updated file: {:?}", &tf.path),
                                                Err(e) => error!("Error updating file {:?} : {:?}", &tf.path, e),
                                            }
                            }
                        }
                    }
                }
                EventMask::DELETE => {}
                _ => {}
            }
        }
        // debug!("Checking for events...");
        thread::sleep(Duration::from_millis(500));
    }
}


fn main() {
    env_logger::init();
    // Check if socket exists already, if it does delete it.
    let socket = Path::new(SOCKET_PATH);
    if socket.exists() {
        fs::remove_file(&socket).unwrap()
    }

    // Create unix domain socket listener on SOCKET_PATH.
    let listener = match UnixListener::bind(&socket) {
        Ok(s) => s,
        Err(e) => {
            error!("Couldn't listen on socket: {:#?}", e);
            return;
        }
    };
    info!("Daemon initialized.");

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

    // Tracker hold inotify, and ensures that tracked files exist between sessions.
    let tracker = Arc::new(Mutex::new(Tracker::init()));

    // Spawn a new thread which listens for and handles Inotify events.
    let tracker_clone = Arc::clone(&tracker);
    let drive_clone = Arc::clone(&drive);
    thread::spawn(move || {
        inotify_listen(tracker_clone, drive_clone);
    });

    // Listen for and handle incoming streams on the socket.
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                handle_stream(
                    s,
                    Arc::clone(&tracker),
                    Arc::clone(&drive),
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
