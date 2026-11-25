use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::covers::CoverSide;
use crate::model::{Collection, DiscFormat, ListTitle, TitleListRule, split_list};
use crate::scanner::CropRect;

const APP_DIR: &str = "KinoCatalog";
const LEGACY_APP_DIR: &str = "myMovies";
const DATA_FILE: &str = "movies.json";
const SETTINGS_FILE: &str = "settings.json";
const LOCATIONS_FILE: &str = "locations.json";
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

/// Location of the app's small preferences file, beside the collection.
pub fn settings_path(data_path: &Path) -> PathBuf {
    data_path.with_file_name(SETTINGS_FILE)
}

/// Collection file inside a folder chosen in Settings.
pub fn movies_path_in(dir: &Path) -> PathBuf {
    dir.join(DATA_FILE)
}

/// Folder-based locations configured from the Settings dialog.
///
/// These live in the user config directory instead of beside `movies.json`, so
/// moving the collection does not make the app forget where it went.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Locations {
    pub movies_dir: Option<PathBuf>,
    pub covers_dir: Option<PathBuf>,
}

impl Locations {
    pub fn movies_path(&self) -> Option<PathBuf> {
        self.movies_dir.as_ref().map(|dir| movies_path_in(dir))
    }
}

/// Location of the small file that remembers the configured folders.
pub fn config_path() -> PathBuf {
    if let Some(path) = env_os("KINOCATALOG_CONFIG", "MYMOVIES_CONFIG") {
        return PathBuf::from(path);
    }

    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(APP_DIR).join(LOCATIONS_FILE)
}

pub fn load_locations() -> Locations {
    fs::read(config_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_locations(locations: &Locations) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }

    let json = serde_json::to_vec_pretty(locations)
        .map_err(|error| format!("Could not serialize the settings: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, json)
        .map_err(|error| format!("Could not write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("Could not replace {}: {error}", path.display()))?;
    Ok(())
}

/// Chooses the collection file: command line first, then Settings, then the
/// usual environment variable or XDG default.
pub fn resolve_data_path(command_line: Option<&Path>, locations: &Locations) -> PathBuf {
    if let Some(path) = command_line {
        return path.to_path_buf();
    }
    if let Some(path) = locations.movies_path() {
        return path;
    }
    data_path()
}

/// Copies per-collection preferences and API keys when Settings points at a
/// different collection folder. Existing files are never overwritten.
pub fn copy_sidecars(from_data_path: &Path, to_data_path: &Path) -> Result<(), String> {
    if from_data_path == to_data_path {
        return Ok(());
    }

    for path in [
        settings_path(from_data_path),
        omdb_key_path(from_data_path),
        tmdb_key_path(from_data_path),
    ] {
        let Some(name) = path.file_name() else {
            continue;
        };
        let target = to_data_path.with_file_name(name);
        if !path.exists() || target.exists() {
            continue;
        }
        if let Some(parent) = target.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        fs::copy(&path, &target).map_err(|error| {
            format!(
                "Could not copy {} to {}: {error}",
                path.display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

/// Expands a leading `~` from a path typed into the Settings dialog.
pub fn expand_user_path(value: &str) -> PathBuf {
    let value = value.trim();
    let home = || {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    };

    if value == "~" {
        return home();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home().join(rest);
    }
    PathBuf::from(value)
}

/// A crop stored as fractions of the preview so it survives small changes in
/// `scanimage`'s output dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StoredCrop {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl StoredCrop {
    pub fn from_pixels(crop: CropRect, image_width: u32, image_height: u32) -> Self {
        let width = image_width.max(1) as f32;
        let height = image_height.max(1) as f32;
        Self {
            x: crop.x as f32 / width,
            y: crop.y as f32 / height,
            width: crop.width as f32 / width,
            height: crop.height as f32 / height,
        }
    }

    pub fn to_pixels(self, image_width: u32, image_height: u32) -> CropRect {
        let image_width = image_width.max(1);
        let image_height = image_height.max(1);
        let x = (self.x * image_width as f32)
            .round()
            .max(0.0)
            .min((image_width - 1) as f32) as u32;
        let y = (self.y * image_height as f32)
            .round()
            .max(0.0)
            .min((image_height - 1) as f32) as u32;
        CropRect {
            x,
            y,
            width: (self.width * image_width as f32)
                .round()
                .max(1.0)
                .min((image_width - x) as f32) as u32,
            height: (self.height * image_height as f32)
                .round()
                .max(1.0)
                .min((image_height - y) as f32) as u32,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct CoverCropDefaults {
    front: Option<StoredCrop>,
    back: Option<StoredCrop>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct CoverCropSettings {
    dvd: CoverCropDefaults,
    bluray: CoverCropDefaults,
    ultrahd: CoverCropDefaults,
}

impl CoverCropSettings {
    fn for_format(&self, format: DiscFormat) -> &CoverCropDefaults {
        match format {
            DiscFormat::Dvd => &self.dvd,
            DiscFormat::BluRay => &self.bluray,
            DiscFormat::UltraHd => &self.ultrahd,
        }
    }

    fn for_format_mut(&mut self, format: DiscFormat) -> &mut CoverCropDefaults {
        match format {
            DiscFormat::Dvd => &mut self.dvd,
            DiscFormat::BluRay => &mut self.bluray,
            DiscFormat::UltraHd => &mut self.ultrahd,
        }
    }
}

/// Preferences that affect cover scanning and the movie list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    cover_crops: CoverCropSettings,
    #[serde(default = "default_title_rules")]
    pub title_rules: Vec<TitleListRule>,
}

/// The first rule matches the common practice of listing the original title
/// for English-speaking or Australian/South African releases.
fn default_title_rules() -> Vec<TitleListRule> {
    vec![TitleListRule {
        countries: split_list("United States, United Kingdom, Australia, South Africa"),
        title: ListTitle::OriginalTitle,
    }]
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            cover_crops: CoverCropSettings::default(),
            title_rules: default_title_rules(),
        }
    }
}

impl AppSettings {
    /// Saved starting crop for one side of a given disc format.
    pub fn cover_crop(&self, format: DiscFormat, side: CoverSide) -> Option<StoredCrop> {
        let defaults = self.cover_crops.for_format(format);
        match side {
            CoverSide::Front => defaults.front,
            CoverSide::Back => defaults.back,
        }
    }

    pub fn set_cover_crop(&mut self, format: DiscFormat, side: CoverSide, crop: StoredCrop) {
        let defaults = self.cover_crops.for_format_mut(format);
        match side {
            CoverSide::Front => defaults.front = Some(crop),
            CoverSide::Back => defaults.back = Some(crop),
        }
    }

    /// Whether either side already has a saved default for this format.
    pub fn has_cover_crop(&self, format: DiscFormat) -> bool {
        let defaults = self.cover_crops.for_format(format);
        defaults.front.is_some() || defaults.back.is_some()
    }
}

/// Missing or damaged preferences fall back to built-in defaults rather than
/// stopping the app from starting.
pub fn load_settings(data_path: &Path) -> AppSettings {
    match fs::read(settings_path(data_path)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => AppSettings::default(),
    }
}

/// Writes preferences through a temporary file so an interrupted save cannot
/// leave truncated settings behind.
pub fn save_settings(data_path: &Path, settings: &AppSettings) -> Result<(), String> {
    let path = settings_path(data_path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }

    let json = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("Could not serialize the settings: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, json)
        .map_err(|error| format!("Could not write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("Could not replace {}: {error}", path.display()))?;
    Ok(())
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
    fn saves_and_loads_per_format_cover_crops() {
        let directory =
            std::env::temp_dir().join(format!("kinocatalog-settings-test-{}", std::process::id()));
        let path = directory.join("movies.json");
        let crop = StoredCrop::from_pixels(
            CropRect {
                x: 118,
                y: 0,
                width: 1500,
                height: 1770,
            },
            1618,
            2173,
        );

        let mut settings = AppSettings::default();
        assert!(!settings.has_cover_crop(DiscFormat::BluRay));
        settings.set_cover_crop(DiscFormat::BluRay, CoverSide::Front, crop);
        settings.set_cover_crop(DiscFormat::Dvd, CoverSide::Back, crop);
        save_settings(&path, &settings).expect("save settings");

        let loaded = load_settings(&path);
        assert_eq!(settings_path(&path), directory.join(SETTINGS_FILE));
        assert_eq!(
            loaded.cover_crop(DiscFormat::BluRay, CoverSide::Front),
            Some(crop)
        );
        assert_eq!(
            loaded.cover_crop(DiscFormat::Dvd, CoverSide::Back),
            Some(crop)
        );
        assert_eq!(loaded.cover_crop(DiscFormat::BluRay, CoverSide::Back), None);
        assert_eq!(
            loaded
                .cover_crop(DiscFormat::BluRay, CoverSide::Front)
                .unwrap()
                .to_pixels(1618, 2173),
            CropRect {
                x: 118,
                y: 0,
                width: 1500,
                height: 1770
            }
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn invalid_settings_fall_back_to_defaults() {
        let directory = std::env::temp_dir().join(format!(
            "kinocatalog-settings-invalid-test-{}",
            std::process::id()
        ));
        let path = directory.join("movies.json");
        fs::create_dir_all(&directory).expect("create directory");
        fs::write(settings_path(&path), b"not json").expect("write settings");

        let settings = load_settings(&path);
        assert!(!settings.has_cover_crop(DiscFormat::Dvd));

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn default_title_rules_match_the_documented_example() {
        let settings = AppSettings::default();
        assert_eq!(settings.title_rules.len(), 1);
        assert_eq!(settings.title_rules[0].title, ListTitle::OriginalTitle);
        assert_eq!(
            settings.title_rules[0].countries,
            vec![
                "United States".to_string(),
                "United Kingdom".to_string(),
                "Australia".to_string(),
                "South Africa".to_string(),
            ]
        );
    }

    #[test]
    fn title_rules_are_persisted_and_can_be_cleared() {
        let directory = std::env::temp_dir().join(format!(
            "kinocatalog-title-rules-test-{}",
            std::process::id()
        ));
        let path = directory.join(DATA_FILE);

        let mut settings = AppSettings::default();
        settings.title_rules.clear();
        settings.title_rules.push(TitleListRule {
            countries: split_list("Finland, Sweden"),
            title: ListTitle::InternationalTitle,
        });
        save_settings(&path, &settings).expect("save settings");
        assert_eq!(load_settings(&path).title_rules, settings.title_rules);

        settings.title_rules.clear();
        save_settings(&path, &settings).expect("save cleared settings");
        assert!(load_settings(&path).title_rules.is_empty());

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

    #[test]
    fn locations_resolve_collection_paths_and_expand_tilde() {
        let dir = PathBuf::from("/tmp/kinocatalog-locations-test");
        let locations = Locations {
            movies_dir: Some(dir.clone()),
            covers_dir: Some(dir.join("covers")),
        };

        assert_eq!(locations.movies_path(), Some(dir.join(DATA_FILE)));
        assert_eq!(movies_path_in(&dir), dir.join(DATA_FILE));
        assert_eq!(
            resolve_data_path(Some(Path::new("/tmp/cli.json")), &locations),
            PathBuf::from("/tmp/cli.json")
        );
        assert_eq!(resolve_data_path(None, &locations), dir.join(DATA_FILE));

        let expanded = expand_user_path("~/films");
        assert!(expanded.ends_with("films"));
        assert_ne!(expanded, PathBuf::from("~/films"));
        assert_eq!(
            expand_user_path("  /tmp/plain  "),
            PathBuf::from("/tmp/plain")
        );
    }

    #[test]
    fn copying_sidecars_does_not_overwrite_existing_files() {
        let base =
            std::env::temp_dir().join(format!("kinocatalog-sidecars-test-{}", std::process::id()));
        let old = base.join("old");
        let new = base.join("new");
        let old_data = old.join(DATA_FILE);
        let new_data = new.join(DATA_FILE);
        save_settings(&old_data, &AppSettings::default()).expect("old settings");
        save_omdb_key(&old_data, "old-key").expect("old key");
        save_tmdb_key(&old_data, "old-token").expect("old token");
        fs::create_dir_all(&new).expect("create new directory");
        fs::write(new.join(OMDB_KEY_FILE), "new-key").expect("existing key");

        copy_sidecars(&old_data, &new_data).expect("copy sidecars");

        assert!(settings_path(&new_data).exists());
        assert!(tmdb_key_path(&new_data).exists());
        assert_eq!(
            fs::read_to_string(new.join(OMDB_KEY_FILE)).unwrap(),
            "new-key"
        );

        let _ = fs::remove_dir_all(base);
    }
}
