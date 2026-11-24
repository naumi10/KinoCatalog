use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbImage};

use crate::covers::{self, CoverSide};

const DEVICE_ENV: &str = "KINOCATALOG_SANE_DEVICE";
const RESOLUTION: &str = "300";
// The opened cover is placed with the front above the back, so the scan area
// must be taller than it is wide to capture both panels.
const WIDTH_MM: &str = "184";
const HEIGHT_MM: &str = "274";
// The spine shows on the left of the rotated front and the right of the
// rotated back. Trim that strip so both saved covers start with the artwork.
const SPINE_CROP_MM: f32 = 10.0;
// A Blu-ray cover insert is 150 mm high. The scan is wider so the case can be
// placed with margin; the extra scanner space below the cover is trimmed off.
const COVER_HEIGHT_MM: f32 = 150.0;
// Existing covers are about 500 px wide and under 200 kB. Scanning at 300 dpi
// produces much larger files, so saved covers are scaled and quality-adjusted
// to match the collection instead of storing every scanner pixel.
const MAX_COVER_WIDTH: u32 = 500;
const MAX_COVER_HEIGHT: u32 = 700;
const MAX_COVER_BYTES: usize = 200_000;
const JPEG_QUALITIES: [u8; 6] = [90, 85, 80, 75, 70, 65];
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const SCAN_TIMEOUT: Duration = Duration::from_secs(180);

/// The two untrimmed halves of one scan: the front is above the back.
/// Both are lossless PNGs so the selection tool can crop without further loss.
#[derive(Debug, Clone)]
pub struct ScannedSpread {
    pub front: Vec<u8>,
    pub back: Vec<u8>,
}

impl ScannedSpread {
    pub fn bytes(&self, side: CoverSide) -> &[u8] {
        match side {
            CoverSide::Front => &self.front,
            CoverSide::Back => &self.back,
        }
    }
}

/// A rectangular crop in image pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CropRect {
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    fn clamp(mut self, image_width: u32, image_height: u32) -> Self {
        self.x = self.x.min(image_width.saturating_sub(1));
        self.y = self.y.min(image_height.saturating_sub(1));
        self.width = self.width.min(image_width - self.x).max(1);
        self.height = self.height.min(image_height - self.y).max(1);
        self
    }
}

/// Scans the whole cover once, splits it horizontally and rotates both halves
/// upright into lossless PNGs.
pub fn scan_spread() -> Result<ScannedSpread, String> {
    let png = run_scanimage()?;
    split_image(&png)
}

/// Starting selection matching the previous automatic trims: the spine and
/// scanner margin are excluded, leaving the printable Blu-ray cover area.
pub fn default_selection(png: &[u8], side: CoverSide) -> Result<CropRect, String> {
    let (width, height) = image_dimensions(png)?;
    if width == 0 || height == 0 {
        return Err("The scanned cover has no pixels to select.".to_string());
    }

    let spine = spine_crop_pixels(width).min(width.saturating_sub(1));
    let crop_height = cover_height_pixels(height).min(height).max(1);
    let x = match side {
        CoverSide::Front => spine,
        CoverSide::Back => 0,
    };
    Ok(CropRect {
        x,
        y: 0,
        width: width.saturating_sub(spine).max(1),
        height: crop_height,
    })
}

/// Crops the selected area and encodes it as the JPEG saved for a cover.
pub fn crop_selection(png: &[u8], selection: CropRect) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(png)
        .map_err(|error| format!("Could not read the scanned cover: {error}"))?;
    let mut rgb = image.to_rgb8();
    let selection = selection.clamp(rgb.width(), rgb.height());
    let cropped = image::imageops::crop(
        &mut rgb,
        selection.x,
        selection.y,
        selection.width,
        selection.height,
    )
    .to_image();
    encode_cover(&cropped)
}

/// Saves one selected cover using the conventional UPC-based file name.
pub fn save_cover(
    upc: &str,
    locality: Option<&str>,
    png: &[u8],
    selection: CropRect,
    side: CoverSide,
) -> Result<PathBuf, String> {
    save_cover_to(&covers::directory(), upc, locality, png, selection, side)
}

fn save_cover_to(
    dir: &Path,
    upc: &str,
    locality: Option<&str>,
    png: &[u8],
    selection: CropRect,
    side: CoverSide,
) -> Result<PathBuf, String> {
    let jpeg = crop_selection(png, selection)?;
    let path = dir.join(covers::file_name_for(upc, locality, side)?);
    write_atomic(&path, &jpeg)?;
    Ok(path)
}

fn split_image(png: &[u8]) -> Result<ScannedSpread, String> {
    let image = image::load_from_memory(png)
        .map_err(|error| format!("scanimage returned an unreadable image: {error}"))?;
    let mut rgb = image.to_rgb8();
    let (width, height) = (rgb.width(), rgb.height());
    if height < 2 {
        return Err("The scanned image is too short to split into two covers.".to_string());
    }

    let middle = height / 2;
    let front_half = image::imageops::crop(&mut rgb, 0, 0, width, middle).to_image();
    let back_half = image::imageops::crop(&mut rgb, 0, middle, width, height - middle).to_image();
    // The opened cover is scanned sideways, so each panel needs a quarter turn
    // to appear upright in the cover viewer.
    let front = image::imageops::rotate90(&front_half);
    let back = image::imageops::rotate90(&back_half);
    Ok(ScannedSpread {
        front: encode_png(&front)?,
        back: encode_png(&back)?,
    })
}

/// The rotated image's width represents half the scanned height, so convert
/// millimetres using that panel depth rather than the full scan width.
fn spine_crop_pixels(rotated_width: u32) -> u32 {
    let panel_depth_mm = HEIGHT_MM.parse::<f32>().unwrap_or(274.0) / 2.0;
    let crop = (SPINE_CROP_MM / panel_depth_mm * rotated_width as f32).round() as u32;
    crop.min(rotated_width.saturating_sub(1))
}

/// The rotated image's height represents the full scan width, so convert
/// millimetres using that dimension.
fn cover_height_pixels(rotated_height: u32) -> u32 {
    let scan_width_mm = WIDTH_MM.parse::<f32>().unwrap_or(184.0);
    (COVER_HEIGHT_MM / scan_width_mm * rotated_height as f32).round() as u32
}

fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| format!("Could not read the scanned cover: {error}"))?;
    Ok((image.width(), image.height()))
}

fn encode_png(image: &RgbImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| format!("Could not encode the scanned cover: {error}"))?;
    Ok(png)
}

/// Scales a crop to the dimensions used by the rest of the collection, then
/// lowers JPEG quality until the file is at most `MAX_COVER_BYTES`.
fn encode_cover(image: &RgbImage) -> Result<Vec<u8>, String> {
    let scale = (MAX_COVER_WIDTH as f32 / image.width() as f32)
        .min(MAX_COVER_HEIGHT as f32 / image.height() as f32)
        .min(1.0);
    let mut resized = if scale < 1.0 {
        let width = (image.width() as f32 * scale).round().max(1.0) as u32;
        let height = (image.height() as f32 * scale).round().max(1.0) as u32;
        image::imageops::resize(image, width, height, image::imageops::FilterType::Lanczos3)
    } else {
        image.clone()
    };

    for &quality in &JPEG_QUALITIES {
        let jpeg = encode_jpeg(&resized, quality)?;
        if jpeg.len() <= MAX_COVER_BYTES {
            return Ok(jpeg);
        }
    }

    // Very detailed scans can still exceed the cap at the lowest quality. Keep
    // shrinking them rather than writing a much larger file than the collection
    // uses.
    let lowest_quality = *JPEG_QUALITIES
        .last()
        .expect("JPEG_QUALITIES must not be empty");
    loop {
        let width = (resized.width() as f32 * 0.9).round().max(1.0) as u32;
        let height = (resized.height() as f32 * 0.9).round().max(1.0) as u32;
        if width == resized.width() && height == resized.height() {
            break;
        }
        resized = image::imageops::resize(
            &resized,
            width,
            height,
            image::imageops::FilterType::Lanczos3,
        );
        let jpeg = encode_jpeg(&resized, lowest_quality)?;
        if jpeg.len() <= MAX_COVER_BYTES {
            return Ok(jpeg);
        }
    }

    Err(format!(
        "Could not reduce the selected cover below {} kB.",
        MAX_COVER_BYTES / 1000
    ))
}

fn encode_jpeg(image: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, quality)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| format!("Could not convert the selected cover to JPEG: {error}"))?;
    Ok(jpeg)
}

/// Resolves the SANE device name. `snapscan` by itself is a backend name, not
/// a device, so `scanimage -L` is used to find the full name such as
/// `snapscan:libusb:007:003`.
fn device() -> Result<String, String> {
    let devices = discover_devices()?;

    if let Some(configured) = std::env::var(DEVICE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        if configured.contains(':') {
            return Ok(configured);
        }
        // Accept a bare backend name as a preference and resolve it.
        let prefix = format!("{configured}:");
        if let Some(found) = devices.iter().find(|name| name.starts_with(&prefix)) {
            return Ok(found.clone());
        }
        return Err(format!(
            "SANE device '{configured}' is a backend name, not a full device name. \
             Available devices: {}. Set KINOCATALOG_SANE_DEVICE to one of these.",
            devices.join(", ")
        ));
    }

    if let Some(found) = devices.iter().find(|name| name.starts_with("snapscan:")) {
        return Ok(found.clone());
    }
    Ok(devices[0].clone())
}

fn discover_devices() -> Result<Vec<String>, String> {
    let output = run_scanimage_command(&["-L".to_string()], DISCOVERY_TIMEOUT, "-L")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            output.status.to_string()
        } else {
            stderr
        };
        return Err(format!("Could not list SANE scanners: {detail}"));
    }

    let devices: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_device_line)
        .collect();

    if devices.is_empty() {
        return Err(
            "No SANE scanner was found. Connect and power on the scanner, then check \
             'scanimage -L'. If it is listed, set KINOCATALOG_SANE_DEVICE to the full \
             device name it prints, for example 'snapscan:libusb:007:003'."
                .to_string(),
        );
    }
    Ok(devices)
}

/// Extracts the device name from a `scanimage -L` line such as:
/// ``device `snapscan:libusb:007:003' is a AGFA-Gevaert SnapScan e20 scanner``.
fn parse_device_line(line: &str) -> Option<String> {
    let start = line.find('`')? + 1;
    let rest = &line[start..];
    let end = rest.find('\'')?;
    let name = rest[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn scan_args(device: &str) -> Vec<String> {
    vec![
        "-d".to_string(),
        device.to_string(),
        "--format=png".to_string(),
        "--mode".to_string(),
        "Color".to_string(),
        "--resolution".to_string(),
        RESOLUTION.to_string(),
        "-x".to_string(),
        WIDTH_MM.to_string(),
        "-y".to_string(),
        HEIGHT_MM.to_string(),
    ]
}

fn run_scanimage() -> Result<Vec<u8>, String> {
    let device = device()?;
    let output =
        run_scanimage_command(&scan_args(&device), SCAN_TIMEOUT, &format!("-d '{device}'"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            output.status.to_string()
        } else {
            stderr
        };
        return Err(format!(
            "scanimage failed for SANE device '{device}': {detail}"
        ));
    }

    if output.stdout.is_empty() {
        return Err(format!(
            "scanimage returned an empty image from SANE device '{device}'."
        ));
    }

    Ok(output.stdout)
}

/// Runs `scanimage` with a deadline. SANE backends can hang when a scanner is
/// powered off or busy, so the UI must not wait on them indefinitely.
fn run_scanimage_command(args: &[String], timeout: Duration, what: &str) -> Result<Output, String> {
    let mut child = Command::new("scanimage")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!("Could not run scanimage {what}: {error}. Install SANE and make sure scanimage is on PATH.")
        })?;

    let pid = child.id();
    let mut stdout = child
        .stdout
        .take()
        .ok_or("Could not read scanimage output.")?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or("Could not read scanimage errors.")?;

    let (out_tx, out_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = out_tx.send(bytes);
    });
    let (err_tx, err_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        let _ = err_tx.send(bytes);
    });

    let (wait_tx, wait_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = wait_tx.send(child.wait());
    });

    match wait_rx.recv_timeout(timeout) {
        Ok(Ok(status)) => Ok(Output {
            status,
            stdout: out_rx.recv().unwrap_or_default(),
            stderr: err_rx.recv().unwrap_or_default(),
        }),
        Ok(Err(error)) => Err(format!("scanimage {what} failed: {error}")),
        Err(RecvTimeoutError::Timeout) => {
            kill_process(pid);
            Err(format!(
                "scanimage {what} timed out after {} seconds. Check that the scanner is powered on and not busy.",
                timeout.as_secs()
            ))
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(format!("scanimage {what} stopped unexpectedly."))
        }
    }
}

fn kill_process(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cover");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));

    fs::write(&temp, bytes)
        .map_err(|error| format!("Could not write {}: {error}", temp.display()))?;

    // Unix rename replaces an existing file; Windows needs it removed first.
    let renamed = if cfg!(windows) && path.exists() {
        fs::remove_file(path).and_then(|()| fs::rename(&temp, path))
    } else {
        fs::rename(&temp, path)
    };

    if let Err(error) = renamed {
        let _ = fs::remove_file(&temp);
        return Err(format!("Could not save {}: {error}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    use image::codecs::png::PngEncoder;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kinocatalog-scanner-{name}-{}", std::process::id()))
    }

    fn assert_rgb_close(actual: [u8; 3], expected: [u8; 3]) {
        for (actual, expected) in actual.iter().zip(expected.iter()) {
            assert!(
                actual.abs_diff(*expected) <= 24,
                "expected {expected:?} ±24, got {actual:?}"
            );
        }
    }

    fn png_bytes(image: &RgbImage) -> Vec<u8> {
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgb8,
            )
            .unwrap();
        png
    }

    #[test]
    fn uses_the_documented_scanimage_arguments() {
        assert_eq!(
            scan_args("snapscan"),
            vec![
                "-d",
                "snapscan",
                "--format=png",
                "--mode",
                "Color",
                "--resolution",
                "300",
                "-x",
                "184",
                "-y",
                "274",
            ]
        );
    }

    #[test]
    fn parses_scanimage_device_listings() {
        let line =
            "device `snapscan:libusb:007:003' is a AGFA-Gevaert SnapScan e20 flatbed scanner";
        assert_eq!(
            parse_device_line(line),
            Some("snapscan:libusb:007:003".to_string())
        );
        assert_eq!(parse_device_line("No scanners were identified."), None);
    }

    #[test]
    fn splits_and_rotates_both_halves_without_cropping() {
        let mut image = RgbImage::new(184, 4);
        for y in 0..4 {
            for x in 0..184 {
                let color = if y < 2 {
                    image::Rgb([255, 0, 0])
                } else {
                    image::Rgb([0, 0, 255])
                };
                image.put_pixel(x, y, color);
            }
        }

        // Larger x values become lower rows after a clockwise turn.
        image.put_pixel(149, 0, image::Rgb([0, 255, 0]));
        image.put_pixel(149, 2, image::Rgb([255, 255, 0]));

        let spread = split_image(&png_bytes(&image)).unwrap();
        let front = image::load_from_memory(&spread.front).unwrap().to_rgb8();
        let back = image::load_from_memory(&spread.back).unwrap().to_rgb8();

        assert_eq!(front.dimensions(), (2, 184));
        assert_eq!(back.dimensions(), (2, 184));
        assert_rgb_close(front.get_pixel(1, 0).0, [255, 0, 0]);
        assert_rgb_close(front.get_pixel(1, 149).0, [0, 255, 0]);
        assert_rgb_close(back.get_pixel(1, 0).0, [0, 0, 255]);
        assert_rgb_close(back.get_pixel(1, 149).0, [255, 255, 0]);
    }

    #[test]
    fn default_selection_excludes_spine_and_scanner_margin() {
        let mut image = RgbImage::new(184, 274);
        for y in 0..274 {
            for x in 0..184 {
                let color = if y < 137 {
                    image::Rgb([255, 0, 0])
                } else {
                    image::Rgb([0, 0, 255])
                };
                image.put_pixel(x, y, color);
            }
        }
        // Bottom of the front half and top of the back half are the spine.
        image.put_pixel(0, 136, image::Rgb([0, 255, 0]));
        image.put_pixel(0, 137, image::Rgb([255, 0, 255]));
        // The opposite edges must remain.
        image.put_pixel(0, 0, image::Rgb([255, 255, 0]));
        image.put_pixel(0, 273, image::Rgb([0, 255, 255]));

        let spread = split_image(&png_bytes(&image)).unwrap();
        let front_selection = default_selection(&spread.front, CoverSide::Front).unwrap();
        let back_selection = default_selection(&spread.back, CoverSide::Back).unwrap();
        assert_eq!(
            front_selection,
            CropRect {
                x: 10,
                y: 0,
                width: 127,
                height: 150
            }
        );
        assert_eq!(
            back_selection,
            CropRect {
                x: 0,
                y: 0,
                width: 127,
                height: 150
            }
        );

        let front =
            image::load_from_memory(&crop_selection(&spread.front, front_selection).unwrap())
                .unwrap()
                .to_rgb8();
        let back = image::load_from_memory(&crop_selection(&spread.back, back_selection).unwrap())
            .unwrap()
            .to_rgb8();

        assert_eq!(front.dimensions(), (127, 150));
        assert_eq!(back.dimensions(), (127, 150));
        assert_rgb_close(front.get_pixel(126, 0).0, [255, 255, 0]);
        assert_rgb_close(front.get_pixel(0, 0).0, [255, 0, 0]);
        assert_rgb_close(back.get_pixel(0, 0).0, [0, 255, 255]);
        assert_rgb_close(back.get_pixel(126, 0).0, [0, 0, 255]);
    }

    #[test]
    fn saves_selected_cover_using_the_locality_convention() {
        let directory = temp_dir("save-selected-cover");
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let image = RgbImage::from_pixel(184, 274, image::Rgb([12, 34, 56]));
        let spread = split_image(&png_bytes(&image)).unwrap();
        let front_selection = default_selection(&spread.front, CoverSide::Front).unwrap();
        let back_selection = default_selection(&spread.back, CoverSide::Back).unwrap();

        let front = save_cover_to(
            &directory,
            "044007-867020",
            Some("16"),
            &spread.front,
            front_selection,
            CoverSide::Front,
        )
        .unwrap();
        let back = save_cover_to(
            &directory,
            "044007-867020",
            Some("16"),
            &spread.back,
            back_selection,
            CoverSide::Back,
        )
        .unwrap();

        assert_eq!(front.file_name().unwrap(), "044007867020.16f.jpg");
        assert_eq!(back.file_name().unwrap(), "044007867020.16b.jpg");
        assert_eq!(
            image::load_from_memory(&fs::read(&front).unwrap())
                .unwrap()
                .dimensions(),
            (127, 150)
        );
        assert_eq!(
            image::load_from_memory(&fs::read(&back).unwrap())
                .unwrap()
                .dimensions(),
            (127, 150)
        );

        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn caps_saved_cover_size_like_existing_covers() {
        let mut image = RgbImage::new(1500, 1770);
        for y in 0..image.height() {
            for x in 0..image.width() {
                image.put_pixel(
                    x,
                    y,
                    image::Rgb([
                        (x ^ y) as u8,
                        (x.wrapping_mul(3).wrapping_add(y)) as u8,
                        (x / 3).wrapping_add(y.wrapping_mul(7)) as u8,
                    ]),
                );
            }
        }

        let jpeg = crop_selection(
            &png_bytes(&image),
            CropRect {
                x: 0,
                y: 0,
                width: image.width(),
                height: image.height(),
            },
        )
        .unwrap();

        assert!(jpeg.len() <= MAX_COVER_BYTES, "{} bytes", jpeg.len());
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert!(decoded.width() <= MAX_COVER_WIDTH);
        assert!(decoded.height() <= MAX_COVER_HEIGHT);
    }
}
