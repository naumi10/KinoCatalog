use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::model::Collection;

const APP_DIR: &str = "KinoCatalog";
const LEGACY_APP_DIR: &str = "myMovies";
const DATA_FILE: &str = "movies.json";
const OMDB_KEY_FILE: &str = "omdb_api_key";
const TMDB_KEY_FILE: &str = "tmdb_api_key";

/// Reads a renamed environment variable, falling back to the name used before
/// the app was renamed so existing scripts keep working.
pub fn env_os(current: &str, legacy: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(current).or_else(|| std::env::var_os(legacy))
}

/// Location of the collection database.
///
/// `KINOCATALOG_DATA` overrides the path (used by the command line import and
/// by tests); the old `MYMOVIES_DATA` name still works. Otherwise the usual XDG
/// data directory is used. Data from the old `myMovies` directory is copied
/// once when the renamed app starts for the first time.
pub fn data_path() -> PathBuf {
    if let Some(path) = env_os("KINOCATALOG_DATA", "MYMOVIES_DATA") {
        return PathBuf::from(path);
    }

    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));

    let path = base.join(APP_DIR).join(DATA_FILE);
    migrate_legacy_data(&base, &path);
    path
}

/// Copies the old `myMovies` data directory to `KinoCatalog` the first time the
/// renamed app runs. Existing files in the new directory are never overwritten.
fn migrate_legacy_data(base: &Path, preferred: &Path) {
    if preferred.exists() {
        return;
    }
    let legacy_dir = base.join(LEGACY_APP_DIR);
    if !legacy_dir.join(DATA_FILE).exists() {
        return;
    }
    let Some(preferred_dir) = preferred.parent() else {
        return;
    };
    if fs::create_dir_all(preferred_dir).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(&legacy_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let source = entry.path();
        if !source.is_file() {
            continue;
        }
        let Some(name) = source.file_name() else {
            continue;
        };
        let target = preferred_dir.join(name);
        if !target.exists() {
            let _ = fs::copy(&source, &target);
        }
    }
}

/// Location of the stored OMDb API key, beside the collection file.
pub fn omdb_key_path(data_path: &Path) -> PathBuf {
    data_path.with_file_name(OMDB_KEY_FILE)
}

/// Location of the stored TMDB API key or read access token.
pub fn tmdb_key_path(data_path: &Path) -> PathBuf {
    data_path.with_file_name(TMDB_KEY_FILE)
}

/// Reads the saved OMDb API key. A missing or blank file means no key is known.
pub fn load_omdb_key(data_path: &Path) -> Option<String> {
    load_key(&omdb_key_path(data_path))
}

/// Reads the saved TMDB API key or read access token.
pub fn load_tmdb_key(data_path: &Path) -> Option<String> {
    load_key(&tmdb_key_path(data_path))
}

fn load_key(path: &Path) -> Option<String> {
    let key = fs::read_to_string(path).ok()?;
    let key = key.trim();
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

/// Stores the OMDb API key with permissions that keep it private.
pub fn save_omdb_key(data_path: &Path, key: &str) -> Result<(), String> {
    save_key(&omdb_key_path(data_path), key)
}

/// Stores the TMDB API key or read access token with private permissions.
pub fn save_tmdb_key(data_path: &Path, key: &str) -> Result<(), String> {
    save_key(&tmdb_key_path(data_path), key)
}

fn save_key(path: &Path, key: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }

    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options
        .open(path)
        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    file.write_all(key.trim().as_bytes())
        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;

    // `mode` only applies when the file is created, so tighten an existing one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Could not secure {}: {error}", path.display()))?;
    }

    Ok(())
}

pub fn load(path: &Path) -> Result<Collection, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Collection::default());
        }
        Err(error) => {
            return Err(format!("Could not read {}: {error}", path.display()));
        }
    };

    let mut collection: Collection = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if collection.normalize() {
        // Persist the migrated country names and digit-only UPCs. A failed
        // migration must not stop the collection from loading.
        let _ = save(path, &mut collection);
    }
    Ok(collection)
}

/// Saves through a temporary file so an interrupted write cannot leave a
/// truncated collection behind. Country names and UPCs are canonicalized first.
pub fn save(path: &Path, collection: &mut Collection) -> Result<(), String> {
    collection.normalize();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }

    let json = serde_json::to_vec_pretty(collection)
        .map_err(|error| format!("Could not serialize the collection: {error}"))?;

    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, json)
        .map_err(|error| format!("Could not write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("Could not replace {}: {error}", path.display()))?;
    Ok(())
}

/// A short, human friendly version of a path (uses `~` for the home folder).
pub fn display_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Movie;

    #[test]
    fn saves_and_loads_a_collection() {
        let directory =
            std::env::temp_dir().join(format!("kinocatalog-test-{}", std::process::id()));
        let path = directory.join("movies.json");

        let mut collection = Collection {
            version: 1,
            movies: vec![Movie {
                id: "test:1".to_string(),
                title: "Alien".to_string(),
                ..Default::default()
            }],
        };

        save(&path, &mut collection).expect("save");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded.movies.len(), 1);
        assert_eq!(loaded.movies[0].title, "Alien");

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn saves_country_names_in_their_canonical_form() {
        let directory =
            std::env::temp_dir().join(format!("kinocatalog-country-test-{}", std::process::id()));
        let path = directory.join("movies.json");

        let mut collection = Collection {
            version: 1,
            movies: vec![Movie {
                id: "test:1".to_string(),
                title: "Alien".to_string(),
                country: "United States of America".to_string(),
                ..Default::default()
            }],
        };

        save(&path, &mut collection).expect("save");
        assert_eq!(collection.movies[0].country, "United States");
        let loaded = load(&path).expect("load");
        assert_eq!(loaded.movies[0].country, "United States");

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn loading_migrates_upcs_to_digits_and_rewrites_the_file() {
        let directory =
            std::env::temp_dir().join(format!("kinocatalog-upc-test-{}", std::process::id()));
        let path = directory.join("movies.json");
        fs::create_dir_all(&directory).expect("create directory");
        fs::write(
            &path,
            r#"{
                "version": 1,
                "movies": [
                    {
                        "id": "upc:012236-047605",
                        "title": "Bad Lieutenant",
                        "upc": "012236-047605"
                    }
                ]
            }"#,
        )
        .expect("write collection");

        let loaded = load(&path).expect("load");
        assert_eq!(loaded.movies[0].upc, "012236047605");
        assert_eq!(loaded.movies[0].id, "upc:012236047605");

        let saved = fs::read_to_string(&path).expect("read migrated collection");
        assert!(!saved.contains("012236-047605"));
        assert!(saved.contains("\"upc\": \"012236047605\""));
        assert!(saved.contains("\"id\": \"upc:012236047605\""));

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_file_is_an_empty_collection() {
        let path = std::env::temp_dir().join("kinocatalog-does-not-exist.json");
        let collection = load(&path).expect("load");
        assert!(collection.movies.is_empty());
    }

    #[test]
    fn saves_and_loads_the_omdb_key_beside_the_collection() {
        let directory =
            std::env::temp_dir().join(format!("kinocatalog-key-test-{}", std::process::id()));
        let path = directory.join("movies.json");

        save_omdb_key(&path, "  abc123  ").expect("save key");
        assert_eq!(omdb_key_path(&path), directory.join(OMDB_KEY_FILE));
        assert_eq!(load_omdb_key(&path).as_deref(), Some("abc123"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(omdb_key_path(&path))
                .expect("key metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn saves_and_loads_the_tmdb_key_beside_the_collection() {
        let directory =
            std::env::temp_dir().join(format!("kinocatalog-tmdb-key-test-{}", std::process::id()));
        let path = directory.join("movies.json");

        save_tmdb_key(&path, "  token123  ").expect("save key");
        assert_eq!(tmdb_key_path(&path), directory.join(TMDB_KEY_FILE));
        assert_eq!(load_tmdb_key(&path).as_deref(), Some("token123"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(tmdb_key_path(&path))
                .expect("key metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn copies_legacy_data_on_first_start() {
        let base =
            std::env::temp_dir().join(format!("kinocatalog-migrate-test-{}", std::process::id()));
        let legacy = base.join(LEGACY_APP_DIR);
        fs::create_dir_all(&legacy).expect("create legacy directory");
        fs::write(legacy.join(DATA_FILE), r#"{"version":1,"movies":[]}"#).expect("write data");
        fs::write(legacy.join(OMDB_KEY_FILE), "old-key").expect("write key");

        let preferred = base.join(APP_DIR).join(DATA_FILE);
        migrate_legacy_data(&base, &preferred);
        assert!(preferred.exists());
        assert_eq!(
            fs::read_to_string(base.join(APP_DIR).join(OMDB_KEY_FILE)).unwrap(),
            "old-key"
        );

        fs::write(&preferred, "new data").expect("write current data");
        migrate_legacy_data(&base, &preferred);
        assert_eq!(fs::read_to_string(&preferred).unwrap(), "new data");

        let _ = fs::remove_dir_all(base);
    }
}
