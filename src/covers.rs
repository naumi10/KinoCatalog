use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

/// Folder chosen in Settings. Environment variables and defaults are only used
/// while this is unset, so a saved choice also affects scanning and downloads.
static DIRECTORY_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn override_lock() -> &'static RwLock<Option<PathBuf>> {
    DIRECTORY_OVERRIDE.get_or_init(|| RwLock::new(None))
}

/// Sets (or clears) the cover folder for the running app.
pub fn set_directory_override(dir: Option<PathBuf>) {
    let mut current = override_lock()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    *current = dir;
}

/// Front and back cover images for one movie.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Covers {
    pub front: Option<PathBuf>,
    pub back: Option<PathBuf>,
}

impl Covers {
    pub fn is_empty(&self) -> bool {
        self.front.is_none() && self.back.is_none()
    }
}

/// Identifies a cover by UPC and, when present, its Invelos locality ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CoverKey {
    upc: String,
    locality: Option<String>,
}

/// Maps UPCs and localities to cover images by scanning a directory.
///
/// Files are named after the disc's UPC and suffixed with `f` or `b`, for
/// example `012236047605f.jpg`. Locality-specific profiles include the Invelos
/// locality ID before the side, for example `044007867020.16f.jpg`.
#[derive(Debug, Default)]
pub struct CoverIndex {
    by_key: HashMap<CoverKey, Covers>,
}

impl CoverIndex {
    /// Scans `dir` for cover images. A missing or unreadable directory simply
    /// produces an empty index.
    pub fn scan(dir: &Path) -> Self {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Self::default();
        };

        let names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_file())
                    .map(|_| entry.file_name().to_string_lossy().into_owned())
            })
            .collect();

        Self::from_file_names(dir, names)
    }

    /// Builds an index from file names located in `dir`. Kept separate from
    /// [`Self::scan`] so the matching rules can be tested without a filesystem.
    pub fn from_file_names(dir: &Path, names: impl IntoIterator<Item = String>) -> Self {
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();

        let mut by_key: HashMap<CoverKey, Covers> = HashMap::new();
        for name in names {
            let Some((key, side)) = parse_cover_name(&name) else {
                continue;
            };
            let covers = by_key.entry(key).or_default();
            let slot = match side {
                CoverSide::Front => &mut covers.front,
                CoverSide::Back => &mut covers.back,
            };
            if slot.is_none() {
                *slot = Some(dir.join(&name));
            }
        }

        Self { by_key }
    }

    /// Looks up covers for a movie's UPC and Invelos locality. An exact locality
    /// match wins; a locality-free file, then any other locality, are fallbacks.
    pub fn for_upc_and_locality(&self, upc: &str, locality: Option<&str>) -> Option<&Covers> {
        let upcs = upc_candidates(upc);
        let localities = locality_candidates(locality);

        for upc in &upcs {
            for candidate in &localities {
                let key = CoverKey {
                    upc: upc.clone(),
                    locality: candidate.clone(),
                };
                if let Some(covers) = self.by_key.get(&key)
                    && !covers.is_empty()
                {
                    return Some(covers);
                }
            }
        }

        // Preserve the old behaviour of showing some cover even when the saved
        // locality does not match a file on disk.
        for upc in &upcs {
            let mut matches: Vec<(&CoverKey, &Covers)> = self
                .by_key
                .iter()
                .filter(|(key, covers)| key.upc == *upc && !covers.is_empty())
                .collect();
            matches.sort_by(|(left, _), (right, _)| left.locality.cmp(&right.locality));
            if let Some((_, covers)) = matches.first() {
                return Some(*covers);
            }
        }

        None
    }
}

fn upc_candidates(upc: &str) -> Vec<String> {
    let digits: String = upc.chars().filter(char::is_ascii_digit).collect();
    let mut keys = Vec::with_capacity(4);
    if !digits.is_empty() {
        keys.push(digits.clone());
        let trimmed = digits.trim_start_matches('0');
        if !trimmed.is_empty() && trimmed != digits {
            keys.push(trimmed.to_string());
        }
        for width in [12, 13] {
            if digits.len() < width {
                keys.push(format!("{digits:0>width$}"));
            }
        }
    }
    keys
}

fn locality_candidates(locality: Option<&str>) -> Vec<Option<String>> {
    match locality {
        Some(value) => vec![Some(value.to_string()), None],
        None => vec![None],
    }
}

/// Which side of a cover disc is being stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverSide {
    Front,
    Back,
}

impl CoverSide {
    /// The single-letter suffix used in conventional cover file names.
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Front => "f",
            Self::Back => "b",
        }
    }
}

/// Name for a scanned cover. A known locality is included, for example
/// `044007867020.16f.jpg`; without one the conventional `012236047605f.jpg`
/// is used.
pub fn file_name_for(upc: &str, locality: Option<&str>, side: CoverSide) -> Result<String, String> {
    let digits: String = upc.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return Err("This movie needs a UPC before covers can be scanned.".to_string());
    }
    let base = match locality {
        Some(value) if valid_locality(value) => format!("{digits}.{value}"),
        _ => digits,
    };
    Ok(format!("{base}{}.jpg", side.suffix()))
}

/// Directory that holds the cover images. A folder saved in Settings wins;
/// otherwise `KINOCATALOG_COVERS` (or the old `MYMOVIES_COVERS`) is used, and
/// finally the `covers` folder in the user's home directory.
pub fn directory() -> PathBuf {
    if let Some(dir) = override_lock()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
    {
        return dir;
    }
    if let Some(dir) = crate::storage::env_os("KINOCATALOG_COVERS", "MYMOVIES_COVERS") {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join("covers"))
        .unwrap_or_else(|| PathBuf::from("covers"))
}

/// Parses `012236047605f.jpg` or `044007867020.16_front.jpg` into its key and
/// side. Returns `None` for files that are not JPEG covers.
fn parse_cover_name(name: &str) -> Option<(CoverKey, CoverSide)> {
    let lower = name.to_ascii_lowercase();
    let stem = lower
        .strip_suffix(".jpeg")
        .or_else(|| lower.strip_suffix(".jpg"))?;

    let (stem, side) = if let Some(base) = stem.strip_suffix("_front") {
        (base, CoverSide::Front)
    } else if let Some(base) = stem.strip_suffix("_back") {
        (base, CoverSide::Back)
    } else {
        match stem.as_bytes().last()? {
            b'f' => (&stem[..stem.len() - 1], CoverSide::Front),
            b'b' => (&stem[..stem.len() - 1], CoverSide::Back),
            _ => return None,
        }
    };

    // The dotted part is the Invelos locality ID, for example `.16` in
    // `044007867020.16f.jpg`. Files without it use locality-free matching.
    let (base, locality) = match stem.split_once('.') {
        Some((base, suffix)) if valid_locality(suffix) => (base, Some(suffix.to_string())),
        Some((base, _)) => (base, None),
        None => (stem, None),
    };
    let digits: String = base.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }

    Some((
        CoverKey {
            upc: digits,
            locality,
        },
        side,
    ))
}

fn valid_locality(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value
            .parse::<u16>()
            .is_ok_and(|number| (1..=999).contains(&number))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(names: &[&str]) -> CoverIndex {
        CoverIndex::from_file_names(
            Path::new("/covers"),
            names.iter().map(|name| name.to_string()),
        )
    }

    fn covers_for<'a>(index: &'a CoverIndex, upc: &str) -> Option<&'a Covers> {
        index.for_upc_and_locality(upc, None)
    }

    #[test]
    fn scanned_cover_names_include_the_locality_when_known() {
        assert_eq!(
            file_name_for("044007-867020", Some("16"), CoverSide::Front).unwrap(),
            "044007867020.16f.jpg"
        );
        assert_eq!(
            file_name_for("044007 867020", Some("11"), CoverSide::Back).unwrap(),
            "044007867020.11b.jpg"
        );
        assert_eq!(
            file_name_for("012236-047605", None, CoverSide::Front).unwrap(),
            "012236047605f.jpg"
        );
        assert_eq!(
            file_name_for("012236 047605", Some("0"), CoverSide::Back).unwrap(),
            "012236047605b.jpg"
        );
        assert!(file_name_for("---", None, CoverSide::Front).is_err());
    }

    #[test]
    fn matches_hyphenated_and_plain_upcs() {
        let index = index(&["012236047605f.jpg", "012236047605b.jpg"]);
        let covers = covers_for(&index, "012236-047605").unwrap();
        assert_eq!(
            covers.front.as_deref(),
            Some(Path::new("/covers/012236047605f.jpg"))
        );
        assert_eq!(
            covers.back.as_deref(),
            Some(Path::new("/covers/012236047605b.jpg"))
        );
    }

    #[test]
    fn matches_locality_specific_cover_files() {
        let index = index(&[
            "044007867020.11f.jpg",
            "044007867020.11b.jpg",
            "044007867020.16f.jpg",
            "044007867020.16b.jpg",
        ]);

        let swedish = index
            .for_upc_and_locality("044007-867020", Some("11"))
            .unwrap();
        assert_eq!(
            swedish.front.as_deref(),
            Some(Path::new("/covers/044007867020.11f.jpg"))
        );
        assert_eq!(
            swedish.back.as_deref(),
            Some(Path::new("/covers/044007867020.11b.jpg"))
        );

        let finnish = index
            .for_upc_and_locality("044007-867020", Some("16"))
            .unwrap();
        assert_eq!(
            finnish.front.as_deref(),
            Some(Path::new("/covers/044007867020.16f.jpg"))
        );
        assert_eq!(
            finnish.back.as_deref(),
            Some(Path::new("/covers/044007867020.16b.jpg"))
        );
    }

    #[test]
    fn falls_back_when_locality_has_no_matching_file() {
        let index = index(&["3259190289994.4f.jpg", "3259190289994.4b.jpg"]);
        let covers = index
            .for_upc_and_locality("325919-0289994", Some("16"))
            .unwrap();
        assert_eq!(
            covers.front.as_deref(),
            Some(Path::new("/covers/3259190289994.4f.jpg"))
        );
    }

    #[test]
    fn tolerates_leading_zeroes_and_case() {
        let index = index(&["012569505322F.JPG", "012569505322B.JPEG"]);
        let covers = covers_for(&index, "12569505322").unwrap();
        assert!(covers.front.is_some());
        assert!(covers.back.is_some());
    }

    #[test]
    fn matches_lookup_script_cover_names() {
        let index = index(&["044007867020.16_front.jpg", "044007867020.16_back.jpg"]);
        let covers = covers_for(&index, "044007-867020").unwrap();
        assert_eq!(
            covers.front.as_deref(),
            Some(Path::new("/covers/044007867020.16_front.jpg"))
        );
        assert_eq!(
            covers.back.as_deref(),
            Some(Path::new("/covers/044007867020.16_back.jpg"))
        );
    }

    #[test]
    fn ignores_unrelated_files_and_unknown_upcs() {
        let index = index(&["readme.txt", "cover.jpg", "012236047605.jpg", "abc.jpg"]);
        assert!(covers_for(&index, "012236-047605").is_none());
        assert!(covers_for(&index, "999999999999").is_none());
        assert!(covers_for(&index, "").is_none());
    }

    #[test]
    fn prefers_the_first_locality_when_none_is_given() {
        let index = index(&["044007867020.11f.jpg", "044007867020.16f.jpg"]);
        let covers = covers_for(&index, "044007867020").unwrap();
        assert_eq!(
            covers.front.as_deref(),
            Some(Path::new("/covers/044007867020.11f.jpg"))
        );
    }
}
