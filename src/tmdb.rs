//! The Movie Database lookup and mapping of its response to movie fields.
//!
//! TMDB accepts either a v3 API key (sent as a query parameter) or a v4 read
//! access token (sent as a bearer token). Both are stored the same way, so the
//! user can paste whichever one the TMDB settings page shows.

use std::io::Read;
use std::time::Duration;

use serde_json::Value;

use crate::model::{join_list, normalize_country};
use crate::omdb;

const API_BASE: &str = "https://api.themoviedb.org/3";
const USER_AGENT: &str = concat!("KinoCatalog/", env!("CARGO_PKG_VERSION"));
const TIMEOUT_SECONDS: u64 = 20;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CAST: usize = 30;

/// A title returned by the TMDB movie search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub id: u64,
    pub title: String,
    pub original_title: String,
    pub year: Option<u16>,
    pub overview: String,
}

/// Searches TMDB by title. The caller lets the user choose among the results
/// before details are fetched by TMDB id.
pub fn search(api_key: &str, title: &str) -> Result<Vec<SearchResult>, String> {
    let credential = credential(api_key)?;
    let title = title.trim();
    if title.is_empty() {
        return Err("The movie has no title to look up.".to_string());
    }

    let agent = http_agent();
    let request = agent
        .get(&format!("{API_BASE}/search/movie"))
        .query("query", title)
        .query("language", "en-US")
        .query("include_adult", "false");
    let response = authorize(request, credential)
        .call()
        .map_err(|error| request_error("searching", error))?;
    let value = response_json(response, "search")?;
    Ok(search_results_from_json(&value))
}

/// Fetches details for the TMDB id chosen from the search results.
pub fn fetch(api_key: &str, id: u64) -> Result<omdb::Details, String> {
    let credential = credential(api_key)?;

    let agent = http_agent();
    let request = agent
        .get(&format!("{API_BASE}/movie/{id}"))
        .query("language", "en-US")
        .query(
            "append_to_response",
            "credits,external_ids,alternative_titles",
        );
    let response = authorize(request, credential)
        .call()
        .map_err(|error| request_error("loading details", error))?;
    let value = response_json(response, "details")?;
    Ok(details_from_json(&value))
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(TIMEOUT_SECONDS))
        .redirects(5)
        .user_agent(USER_AGENT)
        .build()
}

fn credential(api_key: &str) -> Result<&str, String> {
    let api_key = api_key.trim();
    let api_key = api_key
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("bearer "))
        .map(|_| api_key[7..].trim())
        .unwrap_or(api_key);
    if api_key.is_empty() {
        return Err("The TMDB API key or read access token is empty.".to_string());
    }
    Ok(api_key)
}

/// A v4 read access token is a JWT and starts with `eyJ`; a v3 API key is a
/// short hexadecimal string. This lets one saved credential work with either.
fn authorize(request: ureq::Request, credential: &str) -> ureq::Request {
    if credential.starts_with("eyJ") {
        request.set("Authorization", &format!("Bearer {credential}"))
    } else {
        request.query("api_key", credential)
    }
}

fn request_error(context: &str, error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(401, response) => {
            let message = response_message(response);
            match message {
                Some(message) => {
                    format!("TMDB rejected the API key or read access token (HTTP 401): {message}")
                }
                None => "TMDB rejected the API key or read access token (HTTP 401).".to_string(),
            }
        }
        ureq::Error::Status(404, _) => "TMDB could not find that title (HTTP 404).".to_string(),
        ureq::Error::Status(429, _) => {
            "TMDB rate limit reached. Wait a moment and try again.".to_string()
        }
        ureq::Error::Status(code, response) => {
            let message = response_message(response);
            match message {
                Some(message) => format!("TMDB returned HTTP {code} while {context}: {message}"),
                None => format!("TMDB returned HTTP {code} while {context}."),
            }
        }
        ureq::Error::Transport(error) => format!("Could not reach TMDB: {error}"),
    }
}

/// Reads a short TMDB error message from an HTTP error response.
fn response_message(response: ureq::Response) -> Option<String> {
    let mut body = String::new();
    response
        .into_reader()
        .take(64 * 1024)
        .read_to_string(&mut body)
        .ok()?;
    let value: Value = serde_json::from_str(&body).ok()?;
    text(&value, "status_message")
}

fn response_json(response: ureq::Response, context: &str) -> Result<Value, String> {
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_string(&mut body)
        .map_err(|error| format!("Could not read the TMDB {context} response: {error}"))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err("TMDB returned an unexpectedly large response.".to_string());
    }

    let value: Value = serde_json::from_str(&body)
        .map_err(|error| format!("Could not parse the TMDB {context} response: {error}"))?;
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(text(&value, "status_message")
            .unwrap_or_else(|| "TMDB could not complete the request.".to_string()));
    }
    Ok(value)
}

fn search_results_from_json(value: &Value) -> Vec<SearchResult> {
    value
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(search_result_from_json)
        .collect()
}

fn search_result_from_json(value: &Value) -> Option<SearchResult> {
    let id = value.get("id")?.as_u64()?;
    let title = text(value, "title").or_else(|| text(value, "original_title"))?;
    let original_title = text(value, "original_title").unwrap_or_else(|| title.clone());
    Some(SearchResult {
        id,
        title,
        original_title,
        year: date_year(value, "release_date"),
        overview: text(value, "overview").unwrap_or_default(),
    })
}

fn details_from_json(value: &Value) -> omdb::Details {
    omdb::Details {
        year: date_year(value, "release_date"),
        imdb_id: imdb_id(value),
        genres: named_objects(value, "genres"),
        directors: crew_names(value, "Directing", Some("Director")),
        writers: writer_names(value),
        actors: actor_names(value),
        plot: text(value, "overview").unwrap_or_default(),
        country: normalize_country(&country(value)),
        alternative_titles: alternative_titles(value),
    }
}

fn imdb_id(value: &Value) -> String {
    value
        .get("external_ids")
        .and_then(|ids| text(ids, "imdb_id"))
        .or_else(|| text(value, "imdb_id"))
        .unwrap_or_default()
}

fn named_objects(value: &Value, key: &str) -> Vec<String> {
    let mut names = Vec::new();
    for item in value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = text(item, "name") {
            push_unique(&mut names, &name);
        }
    }
    names
}

fn country(value: &Value) -> String {
    let countries = named_objects(value, "production_countries");
    if !countries.is_empty() {
        return join_list(&countries);
    }

    let origins: Vec<String> = value
        .get("origin_country")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|code| !code.trim().is_empty())
        .map(str::to_string)
        .collect();
    join_list(&origins)
}

/// Collects the alternative titles returned by the `alternative_titles`
/// append-to-response block. Identical titles are listed once while the order
/// from TMDB is kept.
fn alternative_titles(value: &Value) -> Vec<String> {
    let mut titles = Vec::new();

    for item in value
        .get("alternative_titles")
        .and_then(|block| block.get("titles"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(title) = text(item, "title") {
            push_unique(&mut titles, &title);
        }
    }
    titles
}

fn crew(value: &Value) -> &[Value] {
    value
        .get("credits")
        .and_then(|credits| credits.get("crew"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn crew_names(value: &Value, department: &str, job: Option<&str>) -> Vec<String> {
    let mut names = Vec::new();
    for member in crew(value) {
        let member_department = text(member, "department").unwrap_or_default();
        let member_job = text(member, "job").unwrap_or_default();
        if !member_department.eq_ignore_ascii_case(department) {
            continue;
        }
        if job.is_some_and(|job| !member_job.eq_ignore_ascii_case(job)) {
            continue;
        }
        if let Some(name) = text(member, "name") {
            push_unique(&mut names, &name);
        }
    }
    names
}

fn writer_names(value: &Value) -> Vec<String> {
    crew_names(value, "Writing", None)
}

fn actor_names(value: &Value) -> Vec<String> {
    let mut names = Vec::new();
    for member in value
        .get("credits")
        .and_then(|credits| credits.get("cast"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = text(member, "name") {
            push_unique(&mut names, &name);
        }
        if names.len() >= MAX_CAST {
            break;
        }
    }
    names
}

fn push_unique(names: &mut Vec<String>, name: &str) {
    if !names.iter().any(|seen| seen.eq_ignore_ascii_case(name)) {
        names.push(name.to_string());
    }
}

fn date_year(value: &Value, key: &str) -> Option<u16> {
    let date = text(value, key)?;
    let year = date.split('-').next()?.trim();
    if year.len() != 4 {
        return None;
    }
    year.parse::<u16>().ok().filter(|year| *year > 0)
}

fn text(value: &Value, key: &str) -> Option<String> {
    let text = value.get(key)?.as_str()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_search() -> Value {
        serde_json::json!({
            "page": 1,
            "results": [
                {
                    "id": 274,
                    "title": "The Silence of the Lambs",
                    "original_title": "The Silence of the Lambs",
                    "release_date": "1991-02-14",
                    "overview": "A young F.B.I. cadet must receive the help of an incarcerated cannibal."
                },
                {
                    "id": 12345,
                    "title": "The Silence of the Lambs",
                    "original_title": "Il silenzio degli innocenti",
                    "release_date": "",
                    "overview": ""
                }
            ]
        })
    }

    fn sample_details() -> Value {
        serde_json::json!({
            "id": 274,
            "title": "The Silence of the Lambs",
            "original_title": "The Silence of the Lambs",
            "release_date": "1991-02-14",
            "runtime": 119,
            "overview": "A young F.B.I. cadet must receive the help of an incarcerated cannibal.",
            "imdb_id": "tt0102926",
            "genres": [
                {"id": 80, "name": "Crime"},
                {"id": 18, "name": "Drama"},
                {"id": 53, "name": "Thriller"}
            ],
            "production_countries": [
                {"iso_3166_1": "US", "name": "United States of America"}
            ],
            "credits": {
                "cast": [
                    {"name": "Jodie Foster", "character": "Clarice Starling", "order": 0},
                    {"name": "Anthony Hopkins", "character": "Dr. Hannibal Lecter", "order": 1},
                    {"name": "Scott Glenn", "character": "Jack Crawford", "order": 2}
                ],
                "crew": [
                    {"name": "Jonathan Demme", "department": "Directing", "job": "Director"},
                    {"name": "Thomas Harris", "department": "Writing", "job": "Novel"},
                    {"name": "Ted Tally", "department": "Writing", "job": "Screenplay"},
                    {"name": "Jonathan Demme", "department": "Writing", "job": "Screenplay"}
                ]
            },
            "external_ids": {
                "imdb_id": "tt0102926"
            },
            "alternative_titles": {
                "titles": [
                    {"iso_3166_1": "US", "title": "The Silence of the Lambs", "type": ""},
                    {"iso_3166_1": "DE", "title": "Das Schweigen der Lämmer", "type": ""},
                    {"iso_3166_1": "FR", "title": "Le silence des agneaux", "type": ""},
                    {"iso_3166_1": "FR", "title": "Le silence des agneaux", "type": ""}
                ]
            }
        })
    }

    #[test]
    fn parses_search_results() {
        let results = search_results_from_json(&sample_search());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, 274);
        assert_eq!(results[0].year, Some(1991));
        assert_eq!(results[1].original_title, "Il silenzio degli innocenti");
        assert_eq!(results[1].year, None);
    }

    #[test]
    fn maps_details_fields() {
        let details = details_from_json(&sample_details());
        assert_eq!(details.year, Some(1991));
        assert_eq!(details.imdb_id, "tt0102926");
        assert_eq!(details.genres, vec!["Crime", "Drama", "Thriller"]);
        assert_eq!(details.directors, vec!["Jonathan Demme"]);
        assert_eq!(
            details.writers,
            vec!["Thomas Harris", "Ted Tally", "Jonathan Demme"]
        );
        assert_eq!(
            details.actors,
            vec!["Jodie Foster", "Anthony Hopkins", "Scott Glenn"]
        );
        assert_eq!(details.country, "United States");
        assert!(details.plot.starts_with("A young F.B.I."));
        assert_eq!(
            details.alternative_titles,
            vec![
                "The Silence of the Lambs",
                "Das Schweigen der Lämmer",
                "Le silence des agneaux"
            ]
        );
    }

    #[test]
    fn falls_back_to_origin_country_codes() {
        let details = details_from_json(&serde_json::json!({
            "origin_country": ["US", "GB"],
            "title": "Example"
        }));
        assert_eq!(details.country, "US, GB");
    }

    #[test]
    fn recognizes_both_tmdb_credential_styles() {
        assert_eq!(credential("  abc123  ").unwrap(), "abc123");
        assert_eq!(
            credential("Bearer eyJhbGciOiJIUzI1NiJ9.token").unwrap(),
            "eyJhbGciOiJIUzI1NiJ9.token"
        );
        assert!(credential("   ").is_err());
    }

    #[test]
    #[ignore = "contacts TMDB and needs TMDB_API_KEY"]
    fn live_lookup_returns_the_expected_profile() {
        let key = std::env::var("TMDB_API_KEY").expect("set TMDB_API_KEY");
        let results = search(&key, "The Silence of the Lambs").unwrap();
        let result = results
            .iter()
            .find(|result| result.year == Some(1991))
            .expect("search should return the 1991 film");
        let details = fetch(&key, result.id).unwrap();
        assert_eq!(details.year, Some(1991));
        assert_eq!(details.imdb_id, "tt0102926");
        assert!(
            details
                .directors
                .iter()
                .any(|name| name == "Jonathan Demme")
        );
    }
}
