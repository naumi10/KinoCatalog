//! Native DVDCompare lookup.
//!
//! Search requests and film pages are fetched directly from dvdcompare.net and
//! parsed from their HTML, so the app does not depend on an external scraper.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;

use crate::model::{DiscFormat, Movie, join_list, normalize_upc};

const BASE_URL: &str = "https://www.dvdcompare.net";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:124.0) Gecko/20100101 Firefox/124.0";
const ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

/// Known DVDCompare soundtrack spellings, longest first where one phrase
/// contains another (Dolby Digital Plus versus Dolby Digital).
const AUDIO_FORMATS: &[(&str, &str)] = &[
    ("DTS-HD Master Audio", "DTS-HD Master Audio"),
    (
        "DTS-HD High Resolution Audio",
        "DTS-HD High Resolution Audio",
    ),
    ("DTS-HD", "DTS-HD"),
    ("Dolby Digital Plus", "Dolby Digital Plus"),
    ("Dolby Digital", "Dolby Digital"),
    ("Dolby TrueHD", "Dolby TrueHD"),
    ("Dolby Atmos", "Dolby Atmos"),
    ("DTS-ES", "DTS-ES"),
    ("DTS", "DTS"),
    ("LPCM", "PCM"),
    ("PCM", "PCM"),
    ("MPEG-2", "MPEG-2"),
    ("Stereo", "Stereo"),
];

/// Disc-format names DVDCompare puts in parentheses.
const KNOWN_FORMATS: &[&str] = &[
    "blu-ray",
    "blu-ray 4k",
    "blu-ray 3d",
    "dvd",
    "hd dvd",
    "hd-dvd",
];

/// One search hit. The title has the search page's year and disc-format suffix
/// removed, because both are exposed separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub year: Option<u16>,
    pub film_id: u32,
    pub disc_format: Option<String>,
}

/// A film page with all of its regional releases.
#[derive(Debug, Clone, Deserialize)]
pub struct Film {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub film_id: Option<u32>,
    #[serde(default)]
    pub releases: Vec<Release>,
    /// Per-release cut information listed at the bottom of a film page.
    #[serde(default)]
    pub cuts: Vec<Cut>,
}

/// One entry in the film page's cut comparison table. The entry names the
/// release and region it belongs to, so it can be matched with `Release`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Cut {
    #[serde(default)]
    pub text: String,
    /// DVDCompare leaves this null for many releases. The release label already
    /// contains the region, so matching still works without it.
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub release: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub status_detail: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// One regional release listed on a film page.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub discs: Vec<Disc>,
    #[serde(default)]
    pub specs: BTreeMap<String, String>,
}

/// One disc inside a release. Only the parts used by the release summary and
/// the extras count are kept.
#[derive(Debug, Clone, Deserialize)]
pub struct Disc {
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub is_film: bool,
    #[serde(default)]
    pub features: Vec<Feature>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Feature {
    #[serde(default)]
    pub children: Vec<Feature>,
}

/// The release values that can update a movie.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    pub audio: Vec<String>,
    pub subtitles: Vec<String>,
    pub case_type: String,
    pub upc: String,
    pub cuts: String,
}

/// One DVDCompare field that an update can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Audio,
    Subtitles,
    CaseType,
    Upc,
    Cuts,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Self::Audio => "Audio",
            Self::Subtitles => "Subtitles",
            Self::CaseType => "Case type",
            Self::Upc => "UPC",
            Self::Cuts => "Cuts",
        }
    }
}

/// One field in the DVDCompare comparison dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub field: Field,
    pub label: &'static str,
    pub current: String,
    pub incoming: String,
    pub selected: bool,
}

impl Details {
    /// Maps the release specs and its cut entry to movie fields. DVDCompare
    /// writes `None` for absent values; those become empty lists and are never
    /// selected by default.
    pub fn from_release(release: &Release, cuts: &[Cut]) -> Self {
        let audio = release
            .specs
            .get("Soundtrack(s)")
            .map(|value| {
                let mut audio: Vec<String> = Vec::new();
                for line in spec_lines(value) {
                    let line = parse_audio_line(&line);
                    if !audio.iter().any(|seen| seen.eq_ignore_ascii_case(&line)) {
                        audio.push(line);
                    }
                }
                audio
            })
            .unwrap_or_default();

        let subtitles = release
            .specs
            .get("Subtitles")
            .map(|value| split_list_value(value))
            .unwrap_or_default();

        let case_type = release
            .specs
            .get("Case type")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.eq_ignore_ascii_case("none"))
            .unwrap_or_default();

        let upc = release
            .specs
            .get("UPC Number")
            .map(|value| normalize_upc(value))
            .unwrap_or_default();

        let cuts = cut_for_release(release, cuts)
            .map(format_cut)
            .unwrap_or_default();

        Self {
            audio,
            subtitles,
            case_type,
            upc,
            cuts,
        }
    }

    /// Builds the comparison rows between a movie and a DVDCompare release.
    /// Changed values are selected by default; empty provider values are not.
    pub fn changes(&self, movie: &Movie) -> Vec<Change> {
        let rows = [
            (
                Field::Audio,
                join_list(&movie.audio),
                join_list(&self.audio),
            ),
            (
                Field::Subtitles,
                join_list(&movie.subtitles),
                join_list(&self.subtitles),
            ),
            (
                Field::CaseType,
                movie.case_type.clone(),
                self.case_type.clone(),
            ),
            (Field::Upc, movie.upc.clone(), normalize_upc(&self.upc)),
            (Field::Cuts, movie.cuts.clone(), self.cuts.clone()),
        ];

        rows.into_iter()
            .map(|(field, current, incoming)| {
                let current_empty = current.trim().is_empty();
                Change {
                    field,
                    label: field.label(),
                    selected: !incoming.trim().is_empty() && (current_empty || incoming != current),
                    current,
                    incoming,
                }
            })
            .collect()
    }

    /// Applies the selected fields to a movie.
    pub fn apply(&self, movie: &mut Movie, fields: impl Iterator<Item = Field>) {
        for field in fields {
            match field {
                Field::Audio => movie.audio = self.audio.clone(),
                Field::Subtitles => movie.subtitles = self.subtitles.clone(),
                Field::CaseType => movie.case_type = self.case_type.clone(),
                Field::Upc => movie.upc = normalize_upc(&self.upc),
                Field::Cuts => movie.cuts = self.cuts.clone(),
            }
        }
    }
}

/// Finds the cut entry for a release. DVDCompare lists cut information as a
/// separate table whose release labels contain the same distributor and edition
/// words in a different order (and sometimes the region prefix attached with no
/// space), so the best match is chosen by word overlap rather than by string
/// equality.
fn cut_for_release<'a>(release: &Release, cuts: &'a [Cut]) -> Option<&'a Cut> {
    let release_words = words(&release.name);
    let mut best: Option<(&Cut, f32)> = None;

    for cut in cuts {
        let mut cut_words = words(&cut.release);
        cut_words.extend(words(cut.region.as_deref().unwrap_or_default()));
        let score = overlap(&release_words, &cut_words);
        if score > 0.0 && best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((cut, score));
        }
    }

    best.filter(|(_, score)| *score >= 0.5).map(|(cut, _)| cut)
}

/// Splits text into lowercase alphanumeric words, ignoring punctuation and
/// repeated whitespace.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// Jaccard similarity of two word lists, with duplicates removed.
fn overlap(left: &[String], right: &[String]) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let matched = left.iter().filter(|word| right.contains(word)).count();
    let total = left.len() + right.iter().filter(|word| !left.contains(word)).count();
    matched as f32 / total as f32
}

/// Renders a cut entry as the multi-line value stored on a movie, for example
/// `No cuts (91:12 NTSC)` or `Yes - Missing a few seconds (91:00 NTSC)` with the
/// detailed cut notes below.
fn format_cut(cut: &Cut) -> String {
    let status = cut.status.trim();
    let detail = cut
        .status_detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| {
            detail.chars().any(char::is_alphanumeric) && !detail.eq_ignore_ascii_case("none")
        });
    let runtime = cut
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|runtime| !runtime.is_empty());

    let mut lines = Vec::new();
    let mut headline = match (status.is_empty(), detail) {
        (false, Some(detail)) => format!("{status} - {detail}"),
        (false, None) => status.to_string(),
        (true, Some(detail)) => detail.to_string(),
        (true, None) => String::new(),
    };
    if let Some(runtime) = runtime {
        if headline.is_empty() {
            headline = format!("({runtime})");
        } else {
            headline.push_str(&format!(" ({runtime})"));
        }
    }

    if headline.is_empty() {
        let text = cut.text.trim();
        if !text.is_empty() {
            lines.push(text.to_string());
        }
    } else {
        lines.push(headline);
    }
    for note in &cut.notes {
        let note = note.trim();
        if !note.is_empty() {
            lines.push(note.to_string());
        }
    }
    lines.join("\n")
}

/// Searches DVDCompare by title and returns the parsed result list.
pub fn search(query: &str) -> Result<Vec<SearchResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Enter a film title to search DVDCompare.".to_string());
    }

    let url = format!("{BASE_URL}/comparisons/search.php");
    let form = [("param", query), ("searchtype", "text")];
    let referer = format!("{BASE_URL}/");
    let html = fetch_html(&url, Some(&form), Some(&referer))
        .map_err(|error| format!("DVDCompare search failed: {error}"))?;
    parse_search_html(&html)
}

/// Fetches a film page and all of its releases by DVDCompare film id.
pub fn film(film_id: u32) -> Result<Film, String> {
    let url = format!("{BASE_URL}/comparisons/film.php?fid={film_id}");
    let html = fetch_html(&url, None, Some(&format!("{BASE_URL}/")))
        .map_err(|error| format!("DVDCompare film lookup failed: {error}"))?;
    parse_film_html(&html)
}

/// Sends a request to DVDCompare. Official-host HTTPS failures are retried
/// over plain HTTP for resilience.
fn fetch_html(
    url: &str,
    form: Option<&[(&str, &str)]>,
    referer: Option<&str>,
) -> Result<String, String> {
    match send_request(url, form, referer) {
        Ok(html) => Ok(html),
        Err(FetchError::Status(code)) if code >= 500 => {
            let fallback = http_fallback(url);
            send_request(&fallback, form, referer)
                .map_err(|error| format!("HTTPS returned HTTP {code}, and {error}"))
        }
        Err(FetchError::Transport(message)) => {
            let fallback = http_fallback(url);
            send_request(&fallback, form, referer)
                .map_err(|error| format!("{message}, and {error}"))
        }
        Err(FetchError::Status(code)) => Err(format!("HTTP {code}")),
    }
}

enum FetchError {
    Status(u16),
    Transport(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(code) => write!(formatter, "HTTP retry returned HTTP {code}"),
            Self::Transport(message) => write!(formatter, "HTTP retry failed: {message}"),
        }
    }
}

fn send_request(
    url: &str,
    form: Option<&[(&str, &str)]>,
    referer: Option<&str>,
) -> Result<String, FetchError> {
    let agent = ureq::AgentBuilder::new()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build();

    let mut request = if form.is_some() {
        agent.post(url)
    } else {
        agent.get(url)
    };
    request = request
        .set("Accept", ACCEPT)
        .set("Accept-Language", "en-US,en;q=0.9")
        .set("DNT", "1")
        .set("Upgrade-Insecure-Requests", "1");
    if let Some(referer) = referer {
        request = request.set("Referer", referer);
    }

    let response = match form {
        Some(form) => request.send_form(form),
        None => request.call(),
    };

    match response {
        Ok(response) => response
            .into_string()
            .map_err(|error| FetchError::Transport(format!("could not read the page: {error}"))),
        Err(ureq::Error::Status(code, _)) => Err(FetchError::Status(code)),
        Err(ureq::Error::Transport(error)) => Err(FetchError::Transport(error.to_string())),
    }
}

fn http_fallback(url: &str) -> String {
    url.strip_prefix("https://")
        .map(|rest| format!("http://{rest}"))
        .unwrap_or_else(|| url.to_string())
}

/// Parses a DVDCompare search results page. Single-result searches use a
/// JavaScript redirect instead of a clickable link, so both forms are handled.
fn parse_search_html(html: &str) -> Result<Vec<SearchResult>, String> {
    let document = Html::parse_document(html);
    let link_selector = css("a[href]");
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for link in document.select(&link_selector) {
        let Some(href) = link.attr("href") else {
            continue;
        };
        if !href.contains("film.php?fid=") {
            continue;
        }
        let Some(film_id) = film_id_from(href) else {
            continue;
        };
        if !seen.insert(film_id) {
            continue;
        }
        let raw_title = element_text(link);
        if raw_title.is_empty() {
            continue;
        }

        results.push(search_result(raw_title, film_id));
    }

    if results.is_empty() {
        let script_selector = css("script");
        let redirect =
            Regex::new(r#"location\.href\s*=\s*["']([^"']*film\.php\?fid=\d+[^"']*)["']"#)
                .expect("redirect pattern");
        for script in document.select(&script_selector) {
            let content: String = script.text().collect();
            let Some(captures) = redirect.captures(&content) else {
                continue;
            };
            let Some(film_id) = film_id_from(&captures[1]) else {
                continue;
            };
            let title = document
                .select(&css("h2"))
                .next()
                .map(|heading| {
                    heading
                        .select(&css("i"))
                        .next()
                        .map(element_text)
                        .unwrap_or_else(|| element_text(heading))
                })
                .unwrap_or_default();
            results.push(search_result(title, film_id));
            break;
        }
    }

    if results.is_empty() && html.trim().is_empty() {
        return Err("DVDCompare returned an empty search page.".to_string());
    }
    Ok(results)
}

fn search_result(raw_title: String, film_id: u32) -> SearchResult {
    let year = extract_year(&raw_title).and_then(|year| u16::try_from(year).ok());
    let disc_format = extract_format(&raw_title);
    let title = clean_search_title(&raw_title, disc_format.as_deref());
    SearchResult {
        title,
        year,
        film_id,
        disc_format,
    }
}

/// Parses a film comparison page into the release list and cut table.
fn parse_film_html(html: &str) -> Result<Film, String> {
    let document = Html::parse_document(html);

    let mut title = String::new();
    let mut year = None;
    let mut format = None;
    if let Some(heading) = document.select(&css("h2")).next() {
        let text = element_text(heading);
        let (rest, extracted_year) = split_year_suffix(&text);
        year = extracted_year;
        let (extracted_title, extracted_format) = split_format_suffix(&rest);
        title = extracted_title;
        format = extracted_format;
    }

    let film_id = document
        .select(&css("form[action]"))
        .filter_map(|form| form.attr("action"))
        .find_map(film_id_from);

    let release_index = parse_release_index(&document);
    let releases = parse_releases(&document, &release_index);
    let cuts = parse_cuts(&document);

    if title.trim().is_empty() && releases.is_empty() {
        return Err("DVDCompare returned an empty film page.".to_string());
    }

    Ok(Film {
        title,
        year,
        format,
        film_id,
        releases,
        cuts,
    })
}

/// Maps release numbers to the canonical names shown in the page index.
fn parse_release_index(document: &Html) -> BTreeMap<u32, (String, Option<i32>)> {
    let anchor_selector = css("a[href]");
    let year_selector = css("span.disc-release-year");
    let year_pattern = Regex::new(r"\[(\d{4})").expect("release year pattern");
    let mut index = BTreeMap::new();

    for anchor in document.select(&anchor_selector) {
        let Some(href) = anchor.attr("href") else {
            continue;
        };
        let Some(number) = href.strip_prefix('#').and_then(|value| {
            value
                .chars()
                .all(|character| character.is_ascii_digit())
                .then(|| value.parse::<u32>().ok())
                .flatten()
        }) else {
            continue;
        };

        let mut name = element_text(anchor);
        let mut release_year = None;
        if let Some(year_span) = anchor.select(&year_selector).next() {
            let span_text = element_text(year_span);
            if let Some(captures) = year_pattern.captures(&span_text) {
                release_year = captures[1].parse::<i32>().ok();
            }
            name = name.replace(&span_text, " ");
        }
        let name = clean_inline(&name);
        if !name.is_empty() {
            index.insert(number, (name, release_year));
        }
    }

    index
}

fn parse_releases(document: &Html, index: &BTreeMap<u32, (String, Option<i32>)>) -> Vec<Release> {
    let list_selector = css("ul.dvd");
    let heading_selector = css("h3");
    let anchor_selector = css("a[name]");
    let description_selector = css("div.description");
    let label_selector = css("div.label");
    let item_selector = css("li");
    let year_selector = css("span.disc-release-year");
    let year_pattern = Regex::new(r"\[(\d{4})").expect("release year pattern");
    let mut releases = Vec::new();

    for list in document.select(&list_selector) {
        let Some(heading) = list.select(&heading_selector).next() else {
            continue;
        };
        let Some(anchor) = heading.select(&anchor_selector).next() else {
            continue;
        };
        let Some(number) = anchor
            .attr("name")
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };

        let (name, release_year) = match index.get(&number) {
            Some((name, year)) => (name.clone(), *year),
            None => {
                let mut name = element_text(heading);
                let mut year = None;
                if let Some(year_span) = heading.select(&year_selector).next() {
                    let span_text = element_text(year_span);
                    if let Some(captures) = year_pattern.captures(&span_text) {
                        year = captures[1].parse::<i32>().ok();
                    }
                    name = name.replace(&span_text, " ");
                }
                (clean_inline(&name), year)
            }
        };

        let mut discs = Vec::new();
        let mut specs: BTreeMap<String, String> = BTreeMap::new();
        for item in list.select(&item_selector) {
            if item.select(&heading_selector).next().is_some() {
                continue;
            }
            let Some(description) = item.select(&description_selector).next() else {
                continue;
            };
            let label = item
                .select(&label_selector)
                .next()
                .map(element_text)
                .unwrap_or_default();
            let label = label.trim_end_matches(':').trim().to_string();

            if label == "Extras" {
                discs = parse_extras(&description.inner_html());
            } else if !label.is_empty() {
                let value = text_lines(description);
                if !value.is_empty() {
                    specs
                        .entry(label)
                        .and_modify(|existing| {
                            existing.push('\n');
                            existing.push_str(&value);
                        })
                        .or_insert(value);
                }
            }
        }

        releases.push(Release {
            name,
            year: release_year,
            discs,
            specs,
        });
    }

    releases
}

fn parse_cuts(document: &Html) -> Vec<Cut> {
    let heading_selector = css("h3");
    let list_selector = css("ul");
    let mut cuts = Vec::new();

    for heading in document.select(&heading_selector) {
        let text = element_text(heading);
        if !text
            .trim()
            .trim_end_matches(':')
            .eq_ignore_ascii_case("CUTS")
        {
            continue;
        }

        let list = heading
            .next_siblings()
            .filter_map(ElementRef::wrap)
            .find(|element| element.value().name() == "ul")
            .or_else(|| {
                heading
                    .parent()
                    .and_then(ElementRef::wrap)
                    .and_then(|parent| parent.select(&list_selector).next())
            });
        let Some(list) = list else {
            continue;
        };

        for item in list
            .child_elements()
            .filter(|element| element.value().name() == "li")
        {
            if let Some(cut) = parse_cut(&item.inner_html()) {
                cuts.push(cut);
            }
        }
    }

    cuts
}

/// Parses one entry from the page-level CUTS list.
fn parse_cut(fragment: &str) -> Option<Cut> {
    let raw = html_to_text(fragment);
    if raw.is_empty() {
        return None;
    }

    let mut lines = raw.lines();
    let main = lines.next().unwrap_or_default();
    let notes = lines
        .map(|line| {
            line.strip_prefix('-')
                .map(str::trim_start)
                .unwrap_or(line)
                .to_string()
        })
        .collect::<Vec<_>>();

    let region_pattern = Regex::new(r"^(R\d+)\s+").expect("region pattern");
    let (region, rest) = match region_pattern.captures(main) {
        Some(captures) => (
            Some(captures[1].to_string()),
            &main[captures.get(0).expect("whole region match").end()..],
        ),
        None => (None, main),
    };

    let (release, tail) = match rest.split_once(" - ") {
        Some((release, tail)) => (release, tail),
        None => (rest, ""),
    };

    let mut status = String::new();
    let mut remainder = tail;
    let lower = tail.to_ascii_lowercase();
    if lower.starts_with("no cuts") {
        status = "No cuts".to_string();
        remainder = &tail["No cuts".len()..];
    } else if lower.starts_with("yes") {
        status = "Yes".to_string();
        remainder = tail[3..].trim_start();
        if let Some(stripped) = remainder.strip_prefix('-') {
            remainder = stripped.trim_start();
        }
    }

    let (runtime, remainder) = split_cut_runtime(remainder);
    let punctuation = Regex::new(r"\s+([.,;:])").expect("punctuation pattern");
    let remainder = punctuation.replace_all(&remainder, "$1");
    let remainder = remainder.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, '-' | ':' | ';' | ',')
    });
    let status_detail = (!remainder.is_empty()).then(|| remainder.to_string());

    Some(Cut {
        text: main.to_string(),
        region,
        release: release.trim().to_string(),
        status,
        status_detail,
        runtime,
        notes,
    })
}

/// Splits the last runtime-looking parenthetical off text. Returns the runtime
/// and the remaining text.
fn split_cut_runtime(text: &str) -> (Option<String>, String) {
    let runtime_pattern =
        Regex::new(r"\(([^()]*\d{1,3}:\d{2}[^()]*)\)").expect("cut runtime pattern");
    let Some(last) = runtime_pattern.captures_iter(text).last() else {
        return (None, text.to_string());
    };
    let whole = last.get(0).expect("whole runtime match");
    let runtime = clean_inline(&last[1]);
    let remaining = format!("{}{}", &text[..whole.start()], &text[whole.end()..]);
    ((!runtime.is_empty()).then_some(runtime), remaining)
}

/// Parses the inner HTML of an extras description into discs.
fn parse_extras(html: &str) -> Vec<Disc> {
    let text = html_to_text(html);
    let range_pattern =
        Regex::new(r"(?i)^DISCS?\s+([A-Z0-9-]+)\s*[-–—]\s*([A-Z0-9-]+)(?:\s*:\s*(.+))?$")
            .expect("disc range pattern");
    let disc_pattern = Regex::new(r#"(?i)^DISC\s+([A-Z0-9-]+)(?:\s+"([^"]+)")?(?:\s+\((.+)\))?$"#)
        .expect("disc header pattern");

    let mut discs: Vec<Disc> = Vec::new();
    let mut current_disc: Option<usize> = None;
    let mut current_group: Option<usize> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(captures) = range_pattern.captures(line) {
            let start = disc_number(&captures[1]);
            let end = disc_number(&captures[2]);
            if start > 0 && end >= start {
                for number in start..=end {
                    discs.push(Disc {
                        number,
                        format: String::new(),
                        is_film: false,
                        features: Vec::new(),
                    });
                }
                current_disc = discs.len().checked_sub(1);
                current_group = None;
                continue;
            }
        }

        if let Some(captures) = disc_pattern.captures(line) {
            let number = disc_number(&captures[1]);
            if number > 0 {
                let format = captures
                    .get(3)
                    .map(|value| value.as_str().trim().to_string())
                    .unwrap_or_default();
                let disc = Disc {
                    number,
                    format,
                    is_film: false,
                    features: Vec::new(),
                };
                if let Some(index) = discs.iter().rposition(|candidate| {
                    candidate.number == number && candidate.features.is_empty()
                }) {
                    discs[index] = disc;
                    current_disc = Some(index);
                } else {
                    discs.push(disc);
                    current_disc = Some(discs.len() - 1);
                }
                current_group = None;
                continue;
            }
        }

        let mut line = line;
        if line.starts_with('*') {
            let stripped = line.trim_start_matches('*').trim();
            let is_film_marker =
                stripped.is_empty() || stripped.to_ascii_lowercase().starts_with("the film");
            if is_film_marker {
                if current_disc.is_none() {
                    discs.push(Disc {
                        number: 1,
                        format: String::new(),
                        is_film: false,
                        features: Vec::new(),
                    });
                    current_disc = Some(discs.len() - 1);
                }
                if let Some(index) = current_disc {
                    discs[index].is_film = true;
                }
            }
            if stripped.is_empty() || stripped.eq_ignore_ascii_case("the film") {
                continue;
            }
            line = stripped;
        }

        if current_disc.is_none() {
            discs.push(Disc {
                number: 1,
                format: String::new(),
                is_film: false,
                features: Vec::new(),
            });
            current_disc = Some(discs.len() - 1);
        }
        let disc_index = current_disc.expect("current disc");

        if let Some(feature_line) = strip_feature_dash(line) {
            let feature = feature(feature_line);
            if let Some(group_index) = current_group
                && let Some(group) = discs[disc_index].features.get_mut(group_index)
            {
                group.children.push(feature);
                continue;
            }
            discs[disc_index].features.push(feature);
            continue;
        }

        let is_group =
            line.trim_end().ends_with(':') || line.to_ascii_lowercase().contains("play all");
        discs[disc_index].features.push(feature(line));
        current_group = is_group.then(|| discs[disc_index].features.len() - 1);
    }

    discs
}

fn feature(_line: &str) -> Feature {
    Feature {
        children: Vec::new(),
    }
}

fn strip_feature_dash(line: &str) -> Option<&str> {
    let dash_count = line
        .chars()
        .take_while(|character| *character == '-')
        .count();
    if !(1..=3).contains(&dash_count) {
        return None;
    }
    let rest = &line[dash_count..];
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then(|| rest.trim_start())
}

fn disc_number(word: &str) -> u32 {
    let word = word.trim().to_ascii_uppercase();
    if word.is_empty() {
        return 0;
    }
    if let Ok(number) = word.parse::<u32>() {
        return number;
    }
    if word.contains('-') {
        let mut total = 0;
        for part in word.split('-') {
            let value = disc_number_word(part.trim());
            if value == 0 {
                return 0;
            }
            total += value;
        }
        return total;
    }
    disc_number_word(&word)
}

fn disc_number_word(word: &str) -> u32 {
    match word {
        "ONE" => 1,
        "TWO" => 2,
        "THREE" => 3,
        "FOUR" => 4,
        "FIVE" => 5,
        "SIX" => 6,
        "SEVEN" => 7,
        "EIGHT" => 8,
        "NINE" => 9,
        "TEN" => 10,
        "ELEVEN" => 11,
        "TWELVE" => 12,
        "THIRTEEN" => 13,
        "FOURTEEN" => 14,
        "FIFTEEN" => 15,
        "SIXTEEN" => 16,
        "SEVENTEEN" => 17,
        "EIGHTEEN" => 18,
        "NINETEEN" => 19,
        "TWENTY" => 20,
        "THIRTY" => 30,
        "FORTY" => 40,
        "FIFTY" => 50,
        "SIXTY" => 60,
        "SEVENTY" => 70,
        "EIGHTY" => 80,
        "NINETY" => 90,
        _ => 0,
    }
}

/// Removes the trailing `(year)` and `(disc format)` that DVDCompare adds to
/// search result titles.
fn clean_search_title(title: &str, disc_format: Option<&str>) -> String {
    let mut title = title.trim().to_string();
    title = strip_year_suffix(&title).0;

    if let Some(format) = disc_format {
        let suffix = format!(" ({})", format.trim());
        if title
            .to_ascii_lowercase()
            .ends_with(&suffix.to_ascii_lowercase())
        {
            title.truncate(title.len() - suffix.len());
        }
    }

    title.trim_end().to_string()
}

fn strip_year_suffix(title: &str) -> (String, Option<i32>) {
    let year_pattern = Regex::new(r"\((\d{4})\)\s*$").expect("year pattern");
    let Some(captures) = year_pattern.captures(title) else {
        return (title.trim_end().to_string(), None);
    };
    let whole = captures.get(0).expect("whole year match");
    (
        title[..whole.start()].trim_end().to_string(),
        captures[1].parse::<i32>().ok(),
    )
}

fn split_year_suffix(text: &str) -> (String, Option<i32>) {
    let pattern =
        Regex::new(r"\((\d{4})(?:\s*[-–—]\s*(\d{4}))?\)\s*$").expect("header year pattern");
    let Some(captures) = pattern.captures(text) else {
        return (text.trim().to_string(), None);
    };
    let whole = captures.get(0).expect("whole header year match");
    (
        text[..whole.start()].trim().to_string(),
        captures[1].parse::<i32>().ok(),
    )
}

fn split_format_suffix(text: &str) -> (String, Option<String>) {
    let pattern = Regex::new(r"\(([^()]+)\)\s*$").expect("format pattern");
    let Some(captures) = pattern.captures(text) else {
        return (text.trim().to_string(), None);
    };
    let candidate = captures[1].trim();
    if !is_known_format(candidate) {
        return (text.trim().to_string(), None);
    }
    let whole = captures.get(0).expect("whole format match");
    (
        text[..whole.start()].trim().to_string(),
        Some(candidate.to_string()),
    )
}

fn extract_year(title: &str) -> Option<i32> {
    let pattern = Regex::new(r"\((\d{4})\)\s*$").expect("search year pattern");
    pattern
        .captures(title)
        .and_then(|captures| captures[1].parse::<i32>().ok())
}

fn extract_format(title: &str) -> Option<String> {
    let pattern = Regex::new(r"\(([^()]+)\)\s+\(\d{4}\)\s*$").expect("search format pattern");
    let captures = pattern.captures(title)?;
    let candidate = captures[1].trim();
    is_known_format(candidate).then(|| candidate.to_string())
}

fn is_known_format(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    KNOWN_FORMATS.contains(&value.as_str())
}

fn film_id_from(value: &str) -> Option<u32> {
    let pattern = Regex::new(r"fid=(\d+)").expect("film id pattern");
    pattern
        .captures(value)
        .and_then(|captures| captures[1].parse::<u32>().ok())
}

/// Converts an HTML fragment to plain text. `br` and block-level tags become
/// line breaks, entities are decoded, and blank lines are dropped.
fn html_to_text(fragment: &str) -> String {
    let line_break = Regex::new(r"(?i)<br\s*/?>").expect("line break pattern");
    let block =
        Regex::new(r"(?i)</?(?:p|div|h[1-6]|li|ul|ol|dl|dt|dd|tr|td|th|table|blockquote)\b[^>]*>")
            .expect("block tag pattern");
    let with_line_breaks = line_break.replace_all(fragment, "\n");
    let prepared = block.replace_all(&with_line_breaks, "\n");
    let document = Html::parse_fragment(&prepared);
    let raw: String = document.root_element().text().collect();
    normalize_lines(&raw)
}

/// Plain text with the fragment's line structure collapsed into spaces.
fn element_text(element: ElementRef<'_>) -> String {
    html_to_text(&element.inner_html()).replace('\n', " ")
}

/// Plain text preserving line breaks.
fn text_lines(element: ElementRef<'_>) -> String {
    html_to_text(&element.inner_html())
}

fn normalize_lines(text: &str) -> String {
    text.split('\n')
        .map(clean_inline)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn clean_inline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn css(selector: &str) -> Selector {
    Selector::parse(selector).expect("valid CSS selector")
}

/// Splits a spec into non-empty lines, treating DVDCompare's `None` as empty.
fn spec_lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.eq_ignore_ascii_case("none"))
        .map(str::to_string)
        .collect()
}

fn split_list_value(value: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() || part.eq_ignore_ascii_case("none") {
            continue;
        }
        if !items.iter().any(|seen| seen.eq_ignore_ascii_case(part)) {
            items.push(part.to_string());
        }
    }
    items
}

/// Converts a DVDCompare soundtrack line such as
/// `English Dolby Digital 2.0 Surround` to the app's collection style,
/// `English · Dolby Digital · Dolby Surround`.
fn parse_audio_line(line: &str) -> String {
    let line = line.trim();
    let lower = line.to_ascii_lowercase();

    let mut best: Option<(usize, usize, &str)> = None;
    for (needle, canonical) in AUDIO_FORMATS {
        let Some(position) = lower.find(&needle.to_ascii_lowercase()) else {
            continue;
        };
        let length = needle.len();
        let replace = best.is_none_or(|(best_position, best_length, _)| {
            position < best_position || (position == best_position && length > best_length)
        });
        if replace {
            best = Some((position, length, canonical));
        }
    }

    let Some((position, length, format)) = best else {
        return line.to_string();
    };

    let language = line[..position].trim().trim_end_matches(['-', ':']).trim();
    let channels = normalize_channels(line[position + length..].trim());

    let mut parts: Vec<String> = Vec::new();
    if !language.is_empty() {
        parts.push(language.to_string());
    }
    parts.push(format.to_string());
    if !channels.is_empty() {
        parts.push(channels);
    }
    parts.join(" · ")
}

fn normalize_channels(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("none") {
        return String::new();
    }

    let lower = value.to_ascii_lowercase();
    if lower == "mono" || lower == "1.0" || lower.starts_with("1.0 ") {
        return "Mono".to_string();
    }
    if lower == "2.0 mono" || lower.starts_with("2.0 mono") {
        return "Mono".to_string();
    }
    if lower == "2-channel stereo" || lower == "2.0 stereo" || lower == "stereo" {
        return "2-Channel Stereo".to_string();
    }
    if lower == "2.0 surround" || lower.starts_with("2.0 surround") {
        return "Dolby Surround".to_string();
    }
    if lower == "2.0" {
        return "2.0".to_string();
    }

    value.to_string()
}

/// Picks the result matching the movie's year and disc format. DVD pages have
/// no `disc_format` in the search result, so they score for DVD movies.
pub fn preferred_result(results: &[SearchResult], year: Option<u16>, format: DiscFormat) -> usize {
    let expected = match format {
        DiscFormat::Dvd => "DVD",
        DiscFormat::BluRay => "Blu-ray",
        DiscFormat::UltraHd => "Blu-ray 4K",
    };

    let mut best = 0;
    let mut best_score = -1;
    for (index, result) in results.iter().enumerate() {
        let mut score = 0;
        if result.year.is_some() && result.year == year {
            score += 4;
        }
        match result.disc_format.as_deref() {
            Some(disc_format) if disc_format.eq_ignore_ascii_case(expected) => score += 2,
            None if format == DiscFormat::Dvd => score += 1,
            _ => {}
        }
        if score > best_score {
            best = index;
            best_score = score;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_release() -> Release {
        serde_json::from_str(
            r#"{
                "name": "R1 America - Media Blasters / Shriek Show - 25th Anniversary Special Edition",
                "year": 2004,
                "discs": [
                    {
                        "number": 1,
                        "format": "DVD",
                        "is_film": true,
                        "features": [
                            {"title": "US Theatrical Trailer"},
                            {"title": "International Trailer"}
                        ]
                    }
                ],
                "specs": {
                    "Subtitles": "English",
                    "Soundtrack(s)": "English Dolby Digital 5.1\nEnglish Dolby Digital 2.0 Surround\nEnglish Dolby Digital 2.0 Mono\nItalian Dolby Digital 1.0",
                    "Case type": "Keep Case",
                    "UPC Number": "631595032192"
                }
            }"#,
        )
        .expect("sample release")
    }

    fn sample_cuts() -> Vec<Cut> {
        serde_json::from_str(
            r#"[
                {
                    "text": "R0 America- Blue Underground - No cuts (91:12 NTSC)",
                    "region": "R0",
                    "release": "America- Blue Underground",
                    "status": "No cuts",
                    "status_detail": null,
                    "runtime": "91:12 NTSC",
                    "notes": []
                },
                {
                    "text": "R0 40th Anniversary Edition America- Blue Underground - No cuts (91:24 NTSC)",
                    "region": "R0",
                    "release": "40th Anniversary Edition America- Blue Underground",
                    "status": "No cuts",
                    "status_detail": null,
                    "runtime": "91:24 NTSC",
                    "notes": []
                },
                {
                    "text": "R0 The Lucio Fulci Collection America- Anchor Bay Entertainment - Yes - Missing a few seconds (91:00 NTSC):",
                    "region": "R0",
                    "release": "The Lucio Fulci Collection America- Anchor Bay Entertainment",
                    "status": "Yes",
                    "status_detail": "Missing a few seconds",
                    "runtime": "91:00 NTSC",
                    "notes": [
                        "Opening frames / music and the shark attack sequence are missing footage."
                    ]
                },
                {
                    "text": "R1 25th Anniversary Special Edition America- Media Blasters / Shriek Show - No cuts (91:15 NTSC)",
                    "region": "R1",
                    "release": "25th Anniversary Special Edition America- Media Blasters / Shriek Show",
                    "status": "No cuts",
                    "status_detail": null,
                    "runtime": "91:15 NTSC",
                    "notes": []
                }
            ]"#,
        )
        .expect("sample cuts")
    }

    #[test]
    fn search_titles_lose_the_year_and_format_suffixes() {
        assert_eq!(
            clean_search_title("Zombie AKA Zombie Flesh Eaters (1979)", None),
            "Zombie AKA Zombie Flesh Eaters"
        );
        assert_eq!(
            clean_search_title(
                "Zombie AKA Zombie Flesh Eaters AKA Zombi 2 (Blu-ray) (1979)",
                Some("Blu-ray")
            ),
            "Zombie AKA Zombie Flesh Eaters AKA Zombi 2"
        );
        assert_eq!(clean_search_title("Unrelated", None), "Unrelated");
    }

    #[test]
    fn parses_search_results_from_html() {
        let html = r#"
            <h2>Search results for <i>Nope</i></h2>
            <ul>
                <li><a href="film.php?fid=62572">Nope (Blu-ray 4K)				(2022)</a></li>
                <li><a href="film.php?fid=62571">Nope (Blu-ray)				(2022)</a></li>
                <li><a href="film.php?fid=62571">Nope duplicate (Blu-ray) (2022)</a></li>
            </ul>
        "#;

        let results = parse_search_html(html).expect("parse search");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].film_id, 62572);
        assert_eq!(results[0].title, "Nope");
        assert_eq!(results[0].year, Some(2022));
        assert_eq!(results[0].disc_format.as_deref(), Some("Blu-ray 4K"));
        assert_eq!(results[1].film_id, 62571);
        assert_eq!(results[1].title, "Nope");
        assert_eq!(results[1].disc_format.as_deref(), Some("Blu-ray"));
    }

    #[test]
    fn parses_single_result_javascript_redirects() {
        let html = r#"
            <h2>Search results for <i>Zombie Flesh Eaters</i></h2>
            <script>location.href="film.php?fid=1";</script>
        "#;

        let results = parse_search_html(html).expect("parse search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].film_id, 1);
        assert_eq!(results[0].title, "Zombie Flesh Eaters");
    }

    #[test]
    fn parses_a_film_page_with_releases() {
        let html = r##"
            <div class="description">
                <h2 style="display:inline">Zombie (Blu-ray) (1979)</h2>
                <form action="film.php?fid=1" method="post"></form>
                <a href="#1">R0 America - Blue Underground
                    <span class="disc-release-year">[2004]</span></a>
            </div>

            <ul class="dvd">
                <li><div class="label">&nbsp;</div><div class="description">
                    <h3><a name="1">R0 America - Blue Underground
                        <span class="disc-release-year">[2004 Release]</span></h3>
                </div></li>
                <li><div class="label">Extras:</div><div class="description">
                    <b>DISC ONE (Blu-ray)</b><br />* The Film<br />Trailer (2:00)<br />- Sub Feature (1:00)
                </div></li>
                <li><div class="label">Subtitles:</div><div class="description">English</div></li>
                <li><div class="label">Soundtrack(s):</div><div class="description">English Dolby Digital 5.1<br />Italian Dolby Digital 1.0</div></li>
                <li><div class="label">Case type:</div><div class="description">Keep Case</div></li>
                <li><div class="label">UPC Number:</div><div class="description">631595032192</div></li>
            </ul>

            <ul class="dvd">
                <li><div class="label">&nbsp;</div><div class="description">
                    <h3>CUTS:</h3>
                    <ul>
                        <li>R0 America- Blue Underground - No cuts (91:12 NTSC)</li>
                    </ul>
                </div></li>
            </ul>
        "##;

        let film = parse_film_html(html).expect("parse film");
        assert_eq!(film.title, "Zombie");
        assert_eq!(film.year, Some(1979));
        assert_eq!(film.format.as_deref(), Some("Blu-ray"));
        assert_eq!(film.film_id, Some(1));
        assert_eq!(film.releases.len(), 1);

        let release = &film.releases[0];
        assert_eq!(release.name, "R0 America - Blue Underground");
        assert_eq!(release.year, Some(2004));
        assert_eq!(release.discs.len(), 1);
        assert!(release.discs[0].is_film);
        assert_eq!(release.discs[0].format, "Blu-ray");
        assert_eq!(release.discs[0].features.len(), 2);
        assert_eq!(release.specs["Subtitles"], "English");
        assert_eq!(release.specs["Case type"], "Keep Case");
        assert_eq!(release.specs["UPC Number"], "631595032192");
        assert_eq!(
            release.specs["Soundtrack(s)"],
            "English Dolby Digital 5.1\nItalian Dolby Digital 1.0"
        );

        assert_eq!(film.cuts.len(), 1);
        assert_eq!(film.cuts[0].status, "No cuts");
        assert_eq!(film.cuts[0].runtime.as_deref(), Some("91:12 NTSC"));
        let details = Details::from_release(release, &film.cuts);
        assert_eq!(
            details.audio,
            vec![
                "English · Dolby Digital · 5.1".to_string(),
                "Italian · Dolby Digital · Mono".to_string(),
            ]
        );
        assert_eq!(details.cuts, "No cuts (91:12 NTSC)");
    }

    #[test]
    fn maps_audio_tracks_to_the_collection_style() {
        assert_eq!(
            parse_audio_line("English Dolby Digital 5.1"),
            "English · Dolby Digital · 5.1"
        );
        assert_eq!(
            parse_audio_line("English Dolby Digital 2.0 Surround"),
            "English · Dolby Digital · Dolby Surround"
        );
        assert_eq!(
            parse_audio_line("English Dolby Digital 2.0 Mono"),
            "English · Dolby Digital · Mono"
        );
        assert_eq!(
            parse_audio_line("Italian Dolby Digital 1.0"),
            "Italian · Dolby Digital · Mono"
        );
        assert_eq!(
            parse_audio_line("English DTS-HD Master Audio 5.1"),
            "English · DTS-HD Master Audio · 5.1"
        );
        assert_eq!(
            parse_audio_line("English LPCM 2.0 Stereo"),
            "English · PCM · 2-Channel Stereo"
        );
        assert_eq!(
            parse_audio_line("Music Dolby Digital 2.0"),
            "Music · Dolby Digital · 2.0"
        );
        assert_eq!(parse_audio_line("Unknown mix"), "Unknown mix");
    }

    #[test]
    fn builds_details_from_a_release() {
        let details = Details::from_release(&sample_release(), &sample_cuts());
        assert_eq!(
            details.audio,
            vec![
                "English · Dolby Digital · 5.1".to_string(),
                "English · Dolby Digital · Dolby Surround".to_string(),
                "English · Dolby Digital · Mono".to_string(),
                "Italian · Dolby Digital · Mono".to_string(),
            ]
        );
        assert_eq!(details.subtitles, vec!["English".to_string()]);
        assert_eq!(details.case_type, "Keep Case");
        assert_eq!(details.upc, "631595032192");
        assert_eq!(details.cuts, "No cuts (91:15 NTSC)");
    }

    #[test]
    fn matches_cuts_to_the_selected_release() {
        let release: Release = serde_json::from_str(
            r#"{
                "name": "R0 America - Blue Underground - 40th Anniversary Edition",
                "specs": {}
            }"#,
        )
        .expect("release");
        let details = Details::from_release(&release, &sample_cuts());
        assert_eq!(
            details.cuts, "No cuts (91:24 NTSC)",
            "the edition must not match the plain release"
        );
    }

    #[test]
    fn formats_cut_details_and_notes() {
        let cuts = sample_cuts();
        let release: Release = serde_json::from_str(
            r#"{
                "name": "R0 America - Anchor Bay Entertainment - The Lucio Fulci Collection",
                "specs": {}
            }"#,
        )
        .expect("release");
        let details = Details::from_release(&release, &cuts);
        assert_eq!(
            details.cuts,
            "Yes - Missing a few seconds (91:00 NTSC)\n\
             Opening frames / music and the shark attack sequence are missing footage."
        );
    }

    #[test]
    fn parses_cut_entries_with_a_null_region() {
        let cuts: Vec<Cut> = serde_json::from_str(
            r#"[{
                "text": "Blu-ray ALL France- Universal Pictures - No cuts (130:30).",
                "region": null,
                "release": "Blu-ray ALL France- Universal Pictures",
                "status": "No cuts",
                "status_detail": ".",
                "runtime": "130:30",
                "notes": []
            }]"#,
        )
        .expect("cuts with null region");
        assert_eq!(cuts[0].region, None);

        let release: Release = serde_json::from_str(
            r#"{"name": "Blu-ray ALL France - Universal Pictures", "specs": {}}"#,
        )
        .expect("release");
        let details = Details::from_release(&release, &cuts);
        assert_eq!(details.cuts, "No cuts (130:30)");
    }

    #[test]
    fn parses_cut_html_without_a_region() {
        let cut = parse_cut("Blu-ray ALL France- Universal Pictures - No cuts (130:30).")
            .expect("cut entry");
        assert_eq!(cut.region, None);
        assert_eq!(cut.release, "Blu-ray ALL France- Universal Pictures");
        assert_eq!(cut.status, "No cuts");
        assert_eq!(cut.runtime.as_deref(), Some("130:30"));
        assert_eq!(format_cut(&cut), "No cuts (130:30)");
    }

    #[test]
    fn ignores_punctuation_only_cut_details() {
        let cut = Cut {
            status: "No cuts".to_string(),
            status_detail: Some(".".to_string()),
            runtime: Some("130:30".to_string()),
            ..Default::default()
        };
        assert_eq!(format_cut(&cut), "No cuts (130:30)");
    }

    #[test]
    fn leaves_cuts_empty_when_the_release_has_no_cut_entry() {
        let release: Release =
            serde_json::from_str(r#"{"name": "R0 America - Blue Underground", "specs": {}}"#)
                .expect("release");
        let details = Details::from_release(&release, &[]);
        assert!(details.cuts.is_empty());
    }

    #[test]
    fn treats_none_specs_as_empty() {
        let release: Release = serde_json::from_str(
            r#"{
                "name": "R0 Test",
                "specs": {
                    "Subtitles": "None",
                    "Soundtrack(s)": "None",
                    "Case type": "None",
                    "UPC Number": ""
                }
            }"#,
        )
        .expect("release");

        let details = Details::from_release(&release, &[]);
        assert!(details.audio.is_empty());
        assert!(details.subtitles.is_empty());
        assert!(details.case_type.is_empty());
        assert!(details.upc.is_empty());
    }

    #[test]
    fn compares_and_applies_release_fields() {
        let details = Details::from_release(&sample_release(), &sample_cuts());
        let mut movie = Movie {
            title: "Zombie".to_string(),
            audio: details.audio.clone(),
            upc: "000000000000".to_string(),
            ..Default::default()
        };

        let changes = details.changes(&movie);
        let audio = changes
            .iter()
            .find(|change| change.field == Field::Audio)
            .expect("audio row");
        assert!(!audio.selected, "unchanged audio should not be selected");
        let subtitles = changes
            .iter()
            .find(|change| change.field == Field::Subtitles)
            .expect("subtitles row");
        assert!(subtitles.selected);
        let case_type = changes
            .iter()
            .find(|change| change.field == Field::CaseType)
            .expect("case row");
        assert!(case_type.selected);
        let upc = changes
            .iter()
            .find(|change| change.field == Field::Upc)
            .expect("upc row");
        assert!(upc.selected);
        let cuts = changes
            .iter()
            .find(|change| change.field == Field::Cuts)
            .expect("cuts row");
        assert!(cuts.selected);
        assert_eq!(cuts.incoming, "No cuts (91:15 NTSC)");

        details.apply(
            &mut movie,
            [Field::Subtitles, Field::CaseType, Field::Upc, Field::Cuts].into_iter(),
        );
        assert_eq!(movie.subtitles, vec!["English".to_string()]);
        assert_eq!(movie.case_type, "Keep Case");
        assert_eq!(movie.upc, "631595032192");
        assert_eq!(movie.cuts, "No cuts (91:15 NTSC)");
        assert_eq!(
            movie.audio, details.audio,
            "unselected audio stays unchanged"
        );
    }

    #[test]
    fn prefers_the_result_matching_the_movies_year_and_format() {
        let results = vec![
            SearchResult {
                title: "Zombie".to_string(),
                year: Some(1979),
                film_id: 1,
                disc_format: None,
            },
            SearchResult {
                title: "Zombie".to_string(),
                year: Some(1979),
                film_id: 19325,
                disc_format: Some("Blu-ray".to_string()),
            },
            SearchResult {
                title: "Zombie".to_string(),
                year: Some(1979),
                film_id: 53904,
                disc_format: Some("Blu-ray 4K".to_string()),
            },
        ];

        assert_eq!(preferred_result(&results, Some(1979), DiscFormat::Dvd), 0);
        assert_eq!(
            preferred_result(&results, Some(1979), DiscFormat::BluRay),
            1
        );
        assert_eq!(
            preferred_result(&results, Some(1979), DiscFormat::UltraHd),
            2
        );
    }

    #[test]
    #[ignore = "contacts dvdcompare.net"]
    fn live_search_finds_zombie_flesh_eaters() {
        let results = search("zombie flesh eaters").expect("search");
        assert!(results.iter().any(|result| result.film_id == 1));
    }

    #[test]
    #[ignore = "contacts dvdcompare.net"]
    fn live_film_lists_releases() {
        let film = film(1).expect("film");
        assert_eq!(film.year, Some(1979));
        assert!(!film.releases.is_empty());
    }

    #[test]
    #[ignore = "contacts dvdcompare.net"]
    fn live_film_matches_cuts_to_every_release() {
        let film = film(1).expect("film");
        assert!(!film.cuts.is_empty());
        for release in &film.releases {
            let details = Details::from_release(release, &film.cuts);
            assert!(!details.cuts.is_empty(), "{} has no cuts", release.name);
        }
        let details = Details::from_release(&film.releases[0], &film.cuts);
        assert!(details.cuts.starts_with("No cuts"));
        assert!(details.cuts.contains("91:12 NTSC"));
    }

    #[test]
    #[ignore = "contacts dvdcompare.net"]
    fn live_nope_cuts_format_cleanly() {
        for fid in [62571, 62572] {
            let film = film(fid).expect("film");
            assert!(!film.cuts.is_empty(), "fid {fid} has no cuts");
            for release in &film.releases {
                let details = Details::from_release(release, &film.cuts);
                assert!(
                    !details.cuts.is_empty(),
                    "fid {fid}: {} has no cuts",
                    release.name
                );
                assert!(
                    !details.cuts.contains(" - ."),
                    "fid {fid}: {}: {}",
                    release.name,
                    details.cuts
                );
            }
            let details = Details::from_release(&film.releases[0], &film.cuts);
            assert_eq!(details.cuts, "No cuts (130:30)", "fid {fid}");
        }
    }
}
