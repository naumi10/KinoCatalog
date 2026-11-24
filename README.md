# KinoCatalog

A small desktop app for keeping track of a DVD / Blu-ray / 4K Ultra HD collection.
See the [User Guide](USER_GUIDE.md) for instructions on adding movies, importing
collections and ratings, updating details, and managing cover images and data.

## Build and run

Rust 1.85 or newer is required (the project uses edition 2024). With the
[rustup](https://rustup.rs/) toolchain installed:

```sh
cargo run
```

To run without a terminal attached:

```sh
cargo build --release
./target/release/KinoCatalog
```

The GUI uses `eframe`/`egui` with the OpenGL (glow) renderer, so no GTK
development packages are needed.

## Install on Windows

[Download the Windows x64 installer](https://github.com/naumi10/KinoCatalog/releases/latest/download/KinoCatalog-windows-x64-setup.exe)
and run it. It installs KinoCatalog for the current user without requiring
administrator rights, adds a Start Menu shortcut, and can offer a desktop
shortcut. A portable
[ZIP package](https://github.com/naumi10/KinoCatalog/releases/latest/download/KinoCatalog-windows-x64.zip)
is published alongside the installer.

## Install on Linux

[Download the Linux x64 package](https://github.com/naumi10/KinoCatalog/releases/latest/download/KinoCatalog-linux-x64.tar.gz),
then extract it and run the installer:

```sh
tar -xzvf KinoCatalog-linux-x64.tar.gz
cd KinoCatalog
./install.sh
```

The installer installs the app for the current user, adds a `KinoCatalog` command
under `~/.local/bin`, and creates an application-menu shortcut.

## Project layout

| File | Purpose |
| --- | --- |
| `src/main.rs` | Entry point, CLI arguments, startup and corrupt-store recovery |
| `src/app.rs` | egui interface, dialogs and app state |
| `src/model.rs` | `Movie`, `Collection`, filtering, sorting, import merge |
| `src/covers.rs` | Cover-image directory index, naming and UPC/locality matching |
| `src/scanner.rs` | SANE `scanimage` scanning, cover splitting/rotation, selection cropping and 200 kB JPEG output |
| `src/dvdcompare.rs` | Native DVDCompare search and release lookup |
| `src/import.rs` | DVD Profiler XML decoding and parsing |
| `src/letterboxd.rs` | Letterboxd ZIP/CSV import and rating matching |
| `src/lookup.rs` | Native Invelos barcode lookup and profile comparison |
| `src/omdb.rs` | OMDb lookup and response-to-movie field mapping |
| `src/tmdb.rs` | TMDB search, detail lookup and response-to-movie field mapping |
| `src/storage.rs` | JSON load/save, data path and API key storage |
| `src/update.rs` | GitHub release check, download and in-place installation |

## Tests

```sh
cargo test
cargo clippy --all-targets
```

Two ignored tests exercise the live Invelos service (network required):

```sh
cargo test -- --ignored
```

The OMDb and TMDB tests also need keys:

```sh
OMDB_API_KEY=your-key cargo test -- --ignored omdb
TMDB_API_KEY=your-key cargo test -- --ignored tmdb
```

The DVDCompare tests contact dvdcompare.net:

```sh
cargo test -- --ignored dvdcompare
```
