extern crate log;

use std::env;
use std::path::PathBuf;

use std::fs::{File, OpenOptions};
use std::io::prelude::*;
use std::io::Error;

use std::os::unix::net::UnixStream;
use std::net::Shutdown;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use inotify::{Inotify, WatchDescriptor, WatchMask};


pub const SOCKET_PATH: &str = "/tmp/rgdrive.sock";
pub const CONFIG_PATH: &str = "/.config/cameron-williams/tracked_files";

fn config_dir() -> PathBuf {
    let mut dir = env::var("HOME").expect("$HOME not set");
    dir.push_str(CONFIG_PATH);
    PathBuf::from(dir)
}


#[derive(Deserialize, Serialize, Debug)]
pub enum DResult {
    Ok(String),
    Err(String),
}

impl DResult {
    
    // Send result on stream.
    pub fn send(&self, mut s: &UnixStream) -> Result<(), Error> {
        // Set write timeout just in case the client isn't listening/ready for a response for some reason.
        s.set_write_timeout(Some(Duration::from_secs(15)))?;
        s.write_all(
            &bincode::serialize(&self).unwrap()
        )?;
        Ok(())
    }

    pub fn error<M: Into<String>>(m: M) -> DResult {
        DResult::Err(m.into())
    }

    pub fn ok<M: Into<String>>(m: M) -> DResult {
        DResult::Ok(m.into())
    }

}

#[derive(Deserialize, Serialize, Debug)]
pub enum DCommand {
    // Args are as followed: drive_url, path_to_download_to, overwrite
    Pull(String, PathBuf, bool),
    
    // path_to_file_to_push
    Push(PathBuf),
    
    // path_to_local_file, drive_url
    FSync(PathBuf, String),
    
    // path_to_local_file
    FUnSync(PathBuf),

    None,
    Message(String),
    Ok,
    Quit
}


impl DCommand {

    // Initialize a DCommand from a &UnixStream. A reference since the stream will be used later to possibly send a response.
    pub fn from_stream(mut s: &UnixStream) -> DCommand {        
        let mut buf: Vec<u8> = Vec::new();
        s.read_to_end(&mut buf).unwrap();
        if buf.len() > 0 {
            bincode::deserialize(&buf).unwrap()
        } else {
            DCommand::None
        }
    }
}



pub struct DSocket {
    path: PathBuf,
}


impl DSocket {

    pub fn new<P: Into<PathBuf>>(p: P) -> DSocket {
        DSocket {
            path: p.into()
        }
    }

    pub fn is_active(&self) -> bool {
        if let Err(_) = UnixStream::connect(&self.path) {
            false
        } else {
            true
        }
    }

    // Send given command to the daemon. Expects and will wait timeout duration for a response.
    pub fn send_command(&self, cmd: DCommand) -> Result<DResult, Error> {

        // Connect to stream.
        let mut stream = UnixStream::connect(&self.path)?;

        // Write command to stream.
        stream.write_all(
            &bincode::serialize(&cmd).unwrap()
        )?;

        // Shutdown write half of stream and set read timeout for response.
        stream.shutdown(Shutdown::Write)?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;

        let mut buf: Vec<u8> = Vec::new();
        stream.read_to_end(&mut buf)?;
        let result: DResult = bincode::deserialize(&buf).unwrap();
        
        Ok(result)
    }

    // Send given command to the daemon. Does not expect a response.
    pub fn send_command_no_response(&self, cmd: DCommand) -> Result<(), Error> {
        let mut stream = UnixStream::connect(&self.path)?;
        stream.write_all(
            &bincode::serialize(&cmd).unwrap()
        )?;
        Ok(())
    }
}



pub struct Tracker {
    pub inotify: Inotify,
    pub tracked_files: Vec<TrackedFile>,
    tracked_files_path: PathBuf,
}


impl Tracker {

    // Initialize Tracker.
    pub fn init() -> Tracker {
        let mut tracker = Tracker {
            inotify: Inotify::init().unwrap(),
            tracked_files: Vec::new(),
            tracked_files_path: config_dir(),
        };

        // If we have an existing list of tracked files, open it and attempt to read it's contents.
        if tracker.tracked_files_path.exists() {

            // On a failed file read, just return tracker with empty tracked_files vec.
            let mut f = match File::open(&tracker.tracked_files_path) {
                Ok(f) => f,
                Err(e) => {
                    log::error!("Error opening tracked files config file: {:?}", e);
                    return tracker
                }
            };

            // Read existing files to buf, and overwrite empty vec with any existing files.
            let mut buf: Vec<u8> = Vec::new();
            f.read_to_end(&mut buf).unwrap();
            
            // Deserialize file to Vec<Trackedfile>
            let tracked_files: Vec<TrackedFile> = match bincode::deserialize(&buf) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("Error deserializing from file: {:?}.. Continuing anyways with a blank tracker.", e);
                    return tracker
                }
            };
            
            // Iterate any trackedfiles that were deseralized from file. Add watches for MODIFY, DELETE_SELF, and MOVE_SELF.
            // Update the TrackedFile resource to include the WatchDescriptor and add it back to the tracker tracked files list.
            for tf in tracked_files {
                let wd = match tracker.inotify.add_watch(&tf.path, WatchMask::MODIFY | WatchMask::DELETE_SELF | WatchMask::MOVE_SELF) {
                    Ok(wd) => wd,
                    Err(e) => {
                        log::error!("Failed to add {:?} to Inotify watch: {:?}", tf, e);
                        continue
                    }
                };
                log::info!("adding {:?} to watch", tf);
                tracker.tracked_files.push(TrackedFile {
                    wd: Some(wd),
                    ..tf
                });
            }
        }
        tracker
    }

    // Saves current Inotify config/tracked paths to file, as Inotify saved paths are not persistent between sessions.
    fn save(&self) -> Result<(), Error> {
        // Open tracked files path. Create new so that it erases any existing paths, since they could have been changed or removed since the last time we accessed the file.
        let mut f = OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(&self.tracked_files_path)?;    
        // Serialize the tracked files vec and write it to the file.
        f.write_all(
            &bincode::serialize(&self.tracked_files).unwrap()
        )?;
        Ok(())
    }

    // Adds given path to the inotify watchlist for MODIFY/DELETE_SELF/MOVE_SELF events.
    pub fn add_path<P: Into<PathBuf>, U: Into<String>>(&mut self, p: P, u: U) -> Result<(), Error> {
        let (url, path) = (u.into(), p.into());

        // Check if path is already added to the watchlist. Skip path if it is.
        for tf in &self.tracked_files {
            if *tf.path == path {
                return Ok(())
            }
        }

        // Add path to inotify watchlist for specific WatchMasks.
        let wd = match self.inotify.add_watch(&path, WatchMask::MODIFY | WatchMask::DELETE_SELF | WatchMask::MOVE_SELF) {
            Ok(wd) => wd,
            Err(e) => {
                log::error!("Failed to add {:?}{:?} to the inotify watchlist: {:?}", path, url, e);
                return Err(e);
            }
        };
        // Add a trackedfile entry with the newly created WatchDescriptor.
        self.tracked_files.push(
            TrackedFile {
                drive_url: url,
                path: path,
                wd: Some(wd),
            }
        );
        // Save and write to file so new config will persist through sessions.
        self.save()?;
        Ok(())
    }

    pub fn remove_path<P: Into<PathBuf>>(&mut self, p: P) -> Result<(), Error> {
        let path = p.into();
        
        // Temp vec to hold drained TrackedFiles.
        let mut _tf: Vec<TrackedFile> = Vec::new();
        // Iterate all tracked files, if their patch matches remove them from the Inotify watchlist.
        for tf in self.tracked_files.drain(..) {
            if tf.path == path {
                if let Some(wd) = tf.wd {
                    self.inotify.rm_watch(wd)?;
                }
            } else {
                _tf.push(tf);
            }
        }
        self.tracked_files = _tf;
        Ok(())
    }
}


#[derive(Deserialize, Serialize, Debug)]
pub struct TrackedFile {
    pub drive_url: String,
    pub path: PathBuf,

    #[serde(skip)]
    pub wd: Option<WatchDescriptor>,
}

impl TrackedFile {
    pub fn from_path<P: Into<PathBuf>>(p: P) -> Vec<TrackedFile> {
        // On a failed file read, just return an empty vec.
        let mut f = match File::open(p.into()) {
            Ok(f) => f,
            Err(e) => {
                log::error!("Error opening tracked files config file: {:?}", e);
                return Vec::new()
            }
        };

        // Read existing files to buf, and overwrite empty vec with any existing files.
        let mut buf: Vec<u8> = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        
        // Deserialize file to Vec<Trackedfile>
        let tracked_files: Vec<TrackedFile> = match bincode::deserialize(&buf) {
            Ok(v) => return v,
            Err(e) => {
                log::warn!("Error deserializing from file: {:?}.. Continuing anyways with a blank tracker.", e);
                return Vec::new()
            }
        };
    }
}


fn main() {}