# rgdrive (Rust GoogleDrive)

rgdrive is a command line tool useful for syncing files between the local computer and GoogleDrive.


Currently completed features:
- Push local files to GoogleDrive and track them
- Pull files from GoogleDrive (not tracked yet)
- Automatically update file on GoogleDrive if it's updated on the local computer





## Getting Started

To get started with rgdrive, just pull the repo and build it using cargo:

```
# Pull repo
> git clone git@github.com:cameron-williams/rgdrive.git

# Build with Cargo
> cd rgdrive
> cargo build --release && cd ./target/release

# CLI help menu
> ./rgdrive --help

# Start worker daemon
> ./rgdrive --start

# Push file from path to Drive, and keep it synced
> ./rgdrive --push /home/cam/testfile.txt

# Pull file from Drive and sync it to given path
> ./rgdrive --pull https://drive.google.com/open?id=1cJ1Iqdz9-mP43pJ_55z0xe-JliUsSzEk /home/cam/Downloads
```


### Prerequisites

To run rgdrive you will need the following:

```
rust >= 1.39.0
cargo >= 1.39.0

A unix-based operating system (haven't testing on OSX)
```

## Authors

* **Cameron Williams**  - [Github](https://github.com/cameron-williams)


## License

This project is licensed under the MIT License - see the [LICENSE.md](LICENSE.md) file for details


