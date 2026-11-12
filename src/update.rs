//! Checks GitHub for a newer release and installs the Linux x64 archive.
//!
//! The update runs on a worker thread, so the caller only needs to poll a
//! channel. Installation replaces the running executable by renaming a fully
//! written staging file over it; on Linux the old executable stays mapped until
//! the process exits, so this is safe while the app is running.

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;

const RELEASES_API: &str = "https://api.github.com/repos/naumi10/KinoCatalog/releases/latest";
const LINUX_ARCHIVE_URL: &str =
    "https://github.com/naumi10/KinoCatalog/releases/latest/download/KinoCatalog-linux-x64.tar.gz";
const USER_AGENT: &str = concat!("KinoCatalog/", env!("CARGO_PKG_VERSION"));
const TIMEOUT_SECONDS: u64 = 60;
const MAX_API_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;

/// Result of an update check that may also have installed a newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate { current: String, latest: String },
    Installed { previous: String, installed: String },
}

/// Checks the latest GitHub release and installs it when it is newer than the
/// running build. This function blocks, so call it from a worker thread.
pub fn check_and_install() -> Result<Outcome, String> {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return Err("Automatic updates are only available for the Linux x64 build.".to_string());
    }

    let current = current_version()?;
    let latest = latest_release()?;
    if latest <= current {
        return Ok(Outcome::UpToDate {
            current: current.to_string(),
            latest: latest.to_string(),
        });
    }

    let archive = download_archive()?;
    install_archive(&archive, &install_dir()?)?;
    Ok(Outcome::Installed {
        previous: current.to_string(),
        installed: latest.to_string(),
    })
}

fn current_version() -> Result<Version, String> {
    parse_version(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("The app's own version is invalid: {error}"))
}

fn latest_release() -> Result<Version, String> {
    let agent = http_agent();
    let response = agent
        .get(RELEASES_API)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(404, _) => {
                "GitHub has no published release for KinoCatalog.".to_string()
            }
            ureq::Error::Status(403, _) => {
                "GitHub refused the update check. Try again later.".to_string()
            }
            ureq::Error::Status(code, _) => {
                format!("GitHub returned HTTP {code} while checking for updates.")
            }
            ureq::Error::Transport(error) => format!("Could not reach GitHub: {error}"),
        })?;

    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_API_BYTES + 1)
        .read_to_string(&mut body)
        .map_err(|error| format!("Could not read the GitHub response: {error}"))?;
    if body.len() as u64 > MAX_API_BYTES {
        return Err("GitHub returned an unexpectedly large response.".to_string());
    }

    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| format!("Could not parse the GitHub response: {error}"))?;
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "The GitHub release has no version tag.".to_string())?;
    parse_version(tag)
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(TIMEOUT_SECONDS))
        .redirects(10)
        .user_agent(USER_AGENT)
        .build()
}

fn parse_version(tag: &str) -> Result<Version, String> {
    let tag = tag.trim();
    let version = tag
        .strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag);
    Version::parse(version)
        .map_err(|error| format!("“{tag}” is not a valid version number: {error}"))
}

fn download_archive() -> Result<Vec<u8>, String> {
    let agent = http_agent();
    let response = agent
        .get(LINUX_ARCHIVE_URL)
        .set("Accept", "application/octet-stream")
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(404, _) => {
                "The GitHub release does not contain KinoCatalog-linux-x64.tar.gz.".to_string()
            }
            ureq::Error::Status(code, _) => {
                format!("GitHub returned HTTP {code} while downloading the update.")
            }
            ureq::Error::Transport(error) => format!("Could not download the update: {error}"),
        })?;

    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_ARCHIVE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the update archive: {error}"))?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err("The update archive is unexpectedly large.".to_string());
    }
    if bytes.is_empty() {
        return Err("The update archive is empty.".to_string());
    }
    Ok(bytes)
}

fn install_dir() -> Result<PathBuf, String> {
    if let Some(dir) = crate::storage::env_os("KINOCATALOG_INSTALL_DIR", "MYMOVIES_INSTALL_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Ok(dir);
        }
    }

    let executable = std::env::current_exe()
        .map_err(|error| format!("Could not locate the running executable: {error}"))?;
    executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "The running executable has no parent directory.".to_string())
}

fn install_archive(bytes: &[u8], target: &Path) -> Result<(), String> {
    let temp = temp_dir()?;
    fs::create_dir_all(&temp)
        .map_err(|error| format!("Could not create a temporary folder: {error}"))?;

    let result = (|| {
        extract_archive(bytes, &temp)?;
        let binary = archive_file(&temp, "KinoCatalog")
            .or_else(|| archive_file(&temp, "myMovies"))
            .ok_or_else(|| {
                "The update archive did not contain the KinoCatalog executable.".to_string()
            })?;
        fs::create_dir_all(target)
            .map_err(|error| format!("Could not create {}: {error}", target.display()))?;
        install_file(&binary, &target.join("KinoCatalog"), true)?;

        if let Some(icon) =
            archive_file(&temp, "KinoCatalog.png").or_else(|| archive_file(&temp, "myMovies.png"))
        {
            install_file(&icon, &target.join("KinoCatalog.png"), false)?;
        }
        if let Some(script) = archive_file(&temp, "install.sh") {
            install_file(&script, &target.join("install.sh"), true)?;
        }
        Ok(())
    })();

    let _ = fs::remove_dir_all(&temp);
    result
}

fn temp_dir() -> Result<PathBuf, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(std::env::temp_dir().join(format!("KinoCatalog-update-{}-{stamp}", std::process::id())))
}

fn extract_archive(bytes: &[u8], destination: &Path) -> Result<(), String> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("Could not read the update archive: {error}"))?;

    for entry in entries {
        let mut entry =
            entry.map_err(|error| format!("Could not read an update archive entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("Could not read an update archive path: {error}"))?
            .into_owned();
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err("The update archive contained an unsafe path.".to_string());
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err("The update archive contained an unsupported entry.".to_string());
        }
        entry
            .unpack_in(destination)
            .map_err(|error| format!("Could not extract the update: {error}"))?;
    }
    Ok(())
}

/// Finds a file both in the flat archive layout and in a `KinoCatalog/` folder.
/// The old `myMovies/` folder and file names are still accepted so an update
/// from a release published before the rename keeps working.
fn archive_file(root: &Path, name: &str) -> Option<PathBuf> {
    let direct = root.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    for folder in ["KinoCatalog", "myMovies"] {
        let nested = root.join(folder).join(name);
        if nested.is_file() {
            return Some(nested);
        }
    }
    None
}

fn install_file(source: &Path, target: &Path, executable: bool) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent directory.", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;

    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("KinoCatalog");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = parent.join(format!(".{name}.{stamp}.update"));

    fs::copy(source, &staging)
        .map_err(|error| format!("Could not stage {}: {error}", target.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        if let Err(error) = fs::set_permissions(&staging, fs::Permissions::from_mode(mode)) {
            let _ = fs::remove_file(&staging);
            return Err(format!(
                "Could not set permissions on {}: {error}",
                target.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = executable;

    if let Err(error) = fs::rename(&staging, target) {
        let _ = fs::remove_file(&staging);
        return Err(format!("Could not install {}: {error}", target.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    fn archive_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, path, *contents)
                .expect("test archive entry should be writable");
        }
        let encoder = builder.into_inner().expect("tar builder should finish");
        encoder.finish().expect("gzip stream should finish")
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("KinoCatalog-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn compares_release_tags_by_semantic_version() {
        assert_eq!(parse_version("v1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(parse_version("V2.0.0").unwrap(), Version::new(2, 0, 0));
        assert!(parse_version("v1.2.3").unwrap() > parse_version("1.2.2").unwrap());
        assert!(parse_version("1.2.3").unwrap() < parse_version("1.3.0").unwrap());
        assert!(parse_version("not-a-version").is_err());
    }

    #[test]
    fn installs_a_flat_archive() {
        let archive = archive_with(&[
            ("KinoCatalog", b"new executable"),
            ("KinoCatalog.png", b"new icon"),
        ]);
        let target = test_dir("flat");
        fs::write(target.join("KinoCatalog"), b"old executable").unwrap();
        fs::write(target.join("KinoCatalog.png"), b"old icon").unwrap();

        install_archive(&archive, &target).unwrap();

        assert_eq!(
            fs::read(target.join("KinoCatalog")).unwrap(),
            b"new executable"
        );
        assert_eq!(
            fs::read(target.join("KinoCatalog.png")).unwrap(),
            b"new icon"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(target.join("KinoCatalog"))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0);
        }

        fs::remove_dir_all(&target).unwrap();
    }

    #[test]
    fn installs_a_nested_archive() {
        let archive = archive_with(&[
            ("KinoCatalog/KinoCatalog", b"nested executable"),
            ("KinoCatalog/KinoCatalog.png", b"nested icon"),
        ]);
        let target = test_dir("nested");

        install_archive(&archive, &target).unwrap();

        assert_eq!(
            fs::read(target.join("KinoCatalog")).unwrap(),
            b"nested executable"
        );
        assert_eq!(
            fs::read(target.join("KinoCatalog.png")).unwrap(),
            b"nested icon"
        );

        fs::remove_dir_all(&target).unwrap();
    }

    #[test]
    fn installs_a_legacy_named_archive() {
        let archive = archive_with(&[
            ("myMovies/myMovies", b"legacy executable"),
            ("myMovies/myMovies.png", b"legacy icon"),
        ]);
        let target = test_dir("legacy");

        install_archive(&archive, &target).unwrap();

        assert_eq!(
            fs::read(target.join("KinoCatalog")).unwrap(),
            b"legacy executable"
        );
        assert_eq!(
            fs::read(target.join("KinoCatalog.png")).unwrap(),
            b"legacy icon"
        );

        fs::remove_dir_all(&target).unwrap();
    }

    #[test]
    fn rejects_an_archive_without_the_executable() {
        let archive = archive_with(&[("KinoCatalog.png", b"icon")]);
        let target = test_dir("missing-binary");

        let error = install_archive(&archive, &target).unwrap_err();
        assert!(error.contains("did not contain"));
        assert!(!target.join("KinoCatalog").exists());

        fs::remove_dir_all(&target).unwrap();
    }
}
