use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontFamily, FontId, Label, Layout,
    Margin, Rect, RichText, Sense, Shape, Stroke, StrokeKind, TextEdit, TextStyle, Ui, Vec2,
};

use crate::covers::{self, CoverIndex, Covers};
use crate::dvdcompare;
use crate::import;
use crate::letterboxd;
use crate::lookup;
use crate::model::{
    Actor, Collection, DiscFormat, Movie, join_list, new_manual_id, normalize_upc, sort_title_for,
    split_list, split_multiline,
};
use crate::omdb;
use crate::storage;
use crate::tmdb;
use crate::update;

const INK: Color32 = Color32::from_rgb(10, 14, 20);
const TEXT: Color32 = Color32::from_rgb(230, 233, 239);
const MUTED: Color32 = Color32::from_rgb(147, 156, 171);
const ACCENT: Color32 = Color32::from_rgb(101, 157, 255);
const ACCENT_BG: Color32 = Color32::from_rgb(38, 58, 92);
const SELECTED_BG: Color32 = Color32::from_rgb(33, 47, 71);
const PANEL_BG: Color32 = Color32::from_rgb(22, 25, 31);
const SIDE_BG: Color32 = Color32::from_rgb(26, 29, 36);
const TOP_BG: Color32 = Color32::from_rgb(16, 18, 23);
const CARD_BG: Color32 = Color32::from_rgb(33, 37, 46);
const BORDER: Color32 = Color32::from_rgb(48, 54, 66);
const ERROR: Color32 = Color32::from_rgb(255, 123, 114);
const SUCCESS: Color32 = Color32::from_rgb(126, 216, 148);

const DVD_FG: Color32 = Color32::from_rgb(111, 200, 179);
const DVD_BG: Color32 = Color32::from_rgb(27, 53, 49);
const BD_FG: Color32 = Color32::from_rgb(111, 168, 255);
const BD_BG: Color32 = Color32::from_rgb(28, 43, 69);
const UHD_FG: Color32 = Color32::from_rgb(255, 196, 107);
const UHD_BG: Color32 = Color32::from_rgb(57, 44, 24);

pub struct KinoCatalogApp {
    collection: Collection,
    data_path: PathBuf,
    selected: Option<String>,
    search: String,
    format_filter: Option<DiscFormat>,
    only_3d: bool,
    only_missing_covers: bool,
    only_missing_year: bool,
    only_missing_original_title: bool,
    filtered: Vec<usize>,
    list_dirty: bool,
    editor: Option<Editor>,
    import_dialog: Option<ImportDialog>,
    pending_delete: Option<String>,
    status: Option<StatusMessage>,
    show_full_cast: bool,
    omdb_key: Option<String>,
    omdb_dialog: Option<OmdbDialog>,
    tmdb_key: Option<String>,
    tmdb_dialog: Option<TmdbDialog>,
    invelos_dialog: Option<InvelosDialog>,
    dvdcompare_dialog: Option<DvdCompareDialog>,
    covers: CoverIndex,
    cover_textures: CoverTextures,
    cover_viewer: Option<PathBuf>,
    show_about: bool,
    update_dialog: Option<UpdateDialog>,
}

struct Editor {
    editing_id: Option<String>,
    form: MovieForm,
    barcode: String,
    lookup: LookupState,
    focus_barcode: bool,
}

/// State of the barcode lookup running in the background.
enum LookupState {
    Idle,
    FindingVariants {
        barcode: String,
        receiver: Receiver<Result<Vec<lookup::ProfileSummary>, String>>,
    },
    Choosing {
        variants: Vec<lookup::ProfileSummary>,
        selected: usize,
    },
    Fetching(Receiver<Result<lookup::MovieDetails, String>>),
    Error(String),
    Done(String),
}

impl LookupState {
    fn busy(&self) -> bool {
        matches!(self, Self::FindingVariants { .. } | Self::Fetching(_))
    }
}

impl Editor {
    fn new() -> Self {
        Self {
            editing_id: None,
            form: MovieForm::default(),
            barcode: String::new(),
            lookup: LookupState::Idle,
            focus_barcode: true,
        }
    }

    fn edit(movie: &Movie) -> Self {
        Self {
            editing_id: Some(movie.id.clone()),
            form: MovieForm::from_movie(movie),
            barcode: String::new(),
            lookup: LookupState::Idle,
            focus_barcode: false,
        }
    }
}

#[derive(Default)]
struct ImportDialog {
    path: String,
    error: Option<String>,
}

struct OmdbDialog {
    movie_id: String,
    title: String,
    year: Option<u16>,
    state: OmdbState,
}

struct TmdbDialog {
    movie_id: String,
    title: String,
    query: String,
    last_query: String,
    year: Option<u16>,
    results: Vec<tmdb::SearchResult>,
    selected: Option<usize>,
    state: TmdbState,
}

struct InvelosDialog {
    movie_id: String,
    title: String,
    upc: String,
    locality: String,
    variants: Vec<lookup::ProfileSummary>,
    selected: Option<usize>,
    state: InvelosState,
}

struct DvdCompareDialog {
    movie_id: String,
    title: String,
    year: Option<u16>,
    format: DiscFormat,
    query: String,
    last_query: String,
    results: Vec<dvdcompare::SearchResult>,
    selected: Option<usize>,
    film: Option<dvdcompare::Film>,
    release_selected: Option<usize>,
    state: DvdCompareState,
}

struct UpdateDialog {
    state: UpdateState,
}

enum UpdateState {
    Checking(Receiver<Result<update::Outcome, String>>),
    Finished(Result<update::Outcome, String>),
}

enum OmdbState {
    /// No usable API key is known, so ask for one before fetching.
    RequestKey {
        input: String,
        error: Option<String>,
    },
    /// The request runs on a worker thread so the window stays responsive.
    Fetching(Receiver<Result<omdb::Details, String>>),
    /// The response is ready and the user chooses which fields to apply.
    Comparing {
        details: omdb::Details,
        changes: Vec<omdb::Change>,
    },
    Failed {
        message: String,
    },
}

enum OmdbAction {
    Close,
    Retry,
    ChangeKey,
    SaveKey(String),
    Save,
}

enum TmdbState {
    /// No usable API key is known, so ask for one before searching.
    RequestKey {
        input: String,
        error: Option<String>,
    },
    /// The title search runs on a worker thread.
    Searching(Receiver<Result<Vec<tmdb::SearchResult>, String>>),
    /// Several titles may match; the user picks the right one.
    Choosing,
    /// Details for the chosen TMDB id are being loaded.
    Fetching {
        result: tmdb::SearchResult,
        receiver: Receiver<Result<omdb::Details, String>>,
    },
    Comparing {
        result: tmdb::SearchResult,
        details: Box<omdb::Details>,
        changes: Vec<omdb::Change>,
    },
    Failed {
        message: String,
        retry: TmdbRetry,
    },
}

#[derive(Clone, Copy)]
enum TmdbRetry {
    Search,
    Details,
}

enum TmdbAction {
    Close,
    Retry,
    SearchAgain,
    ChooseAnother,
    ChangeKey,
    SaveKey(String),
    Fetch(usize),
    Save,
}

enum InvelosState {
    /// The barcode is being looked up in the variant service.
    FindingVariants(Receiver<Result<Vec<lookup::ProfileSummary>, String>>),
    /// Several localities exist for the barcode; the user picks one.
    Choosing,
    /// The chosen profile is being downloaded.
    Fetching {
        profile_id: String,
        receiver: Receiver<Result<lookup::MovieDetails, String>>,
    },
    Comparing {
        details: Box<lookup::MovieDetails>,
        changes: Vec<lookup::Change>,
    },
    Failed {
        message: String,
        retry: InvelosRetry,
    },
}

#[derive(Clone, Copy)]
enum InvelosRetry {
    Variants,
    Profile,
}

enum InvelosAction {
    Close,
    Retry,
    ChooseAnother,
    Fetch(usize),
    Save,
}

enum DvdCompareState {
    /// The title search runs on a worker thread against dvdcompare.net.
    Searching(Receiver<Result<Vec<dvdcompare::SearchResult>, String>>),
    /// The user picks the right film from the search results.
    ChoosingFilm,
    /// The chosen film page and all its releases are being loaded.
    FetchingFilm {
        result: dvdcompare::SearchResult,
        receiver: Receiver<Result<dvdcompare::Film, String>>,
    },
    /// The film is loaded; the user picks one of its regional releases.
    ChoosingRelease,
    /// The release is loaded and the user chooses which fields to apply.
    Comparing {
        release: dvdcompare::Release,
        details: Box<dvdcompare::Details>,
        changes: Vec<dvdcompare::Change>,
    },
    Failed {
        message: String,
        retry: DvdCompareRetry,
    },
}

#[derive(Clone, Copy)]
enum DvdCompareRetry {
    Search,
    Film,
}

enum DvdCompareAction {
    Close,
    Retry,
    SearchAgain,
    ChooseAnotherFilm,
    ChooseAnotherRelease,
    FetchFilm(usize),
    CompareRelease(usize),
    Save,
}

struct StatusMessage {
    text: String,
    kind: StatusKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Success,
    Error,
}

impl StatusMessage {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Info,
        }
    }

    fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Success,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Error,
        }
    }
}

#[derive(Default)]
struct MovieForm {
    title: String,
    original_title: String,
    alternative_titles: String,
    sort_title: String,
    format: DiscFormat,
    is_3d: bool,
    year: String,
    runtime: String,
    rating: String,
    genres: String,
    regions: String,
    studios: String,
    directors: String,
    cast: String,
    overview: String,
    upc: String,
    locality: String,
    collection_number: String,
    case_type: String,
    cuts: String,
    purchase_date: String,
    purchase_price: String,
    location: String,
    notes: String,
    error: Option<String>,
}

impl MovieForm {
    fn from_movie(movie: &Movie) -> Self {
        Self {
            title: movie.title.clone(),
            original_title: movie.original_title.clone(),
            alternative_titles: movie.alternative_titles.join("\n"),
            // Only a lookup supplies a different sort title; when editing, an
            // empty value keeps the existing one unless the title changes.
            sort_title: String::new(),
            format: movie.format,
            is_3d: movie.is_3d,
            year: movie.year.map(|year| year.to_string()).unwrap_or_default(),
            runtime: movie
                .runtime_minutes
                .map(|minutes| minutes.to_string())
                .unwrap_or_default(),
            rating: movie.rating.clone(),
            genres: join_list(&movie.genres),
            regions: join_list(&movie.regions),
            studios: movie.studios.join("\n"),
            directors: movie.directors.join("\n"),
            cast: movie
                .actors
                .iter()
                .map(|actor| {
                    if actor.role.is_empty() {
                        actor.name.clone()
                    } else {
                        format!("{} as {}", actor.name, actor.role)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            overview: movie.overview.clone(),
            upc: movie.upc.clone(),
            locality: movie.locality.clone(),
            collection_number: movie.collection_number.clone(),
            case_type: movie.case_type.clone(),
            cuts: movie.cuts.clone(),
            purchase_date: movie.purchase_date.clone(),
            purchase_price: movie.purchase_price.clone(),
            location: movie.location.clone(),
            notes: movie.notes.clone(),
            error: None,
        }
    }

    fn apply_to(&self, movie: &mut Movie) -> Result<(), String> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err("Please enter a title.".to_string());
        }

        let sort_title = self.sort_title.trim();
        if !sort_title.is_empty() {
            movie.sort_title = sort_title.to_string();
        } else if movie.title != title || movie.sort_title.trim().is_empty() {
            movie.sort_title = sort_title_for(title);
        }

        movie.title = title.to_string();
        movie.original_title = self.original_title.trim().to_string();
        movie.alternative_titles = split_multiline(&self.alternative_titles);
        movie.format = self.format;
        movie.is_3d = self.is_3d;
        movie.year = parse_optional_number(&self.year, "Year")?;
        movie.runtime_minutes = parse_optional_number(&self.runtime, "Runtime")?;
        movie.rating = self.rating.trim().to_string();
        movie.genres = split_list(&self.genres);
        movie.regions = split_list(&self.regions);
        movie.studios = split_multiline(&self.studios);
        movie.directors = split_multiline(&self.directors);
        movie.actors = parse_cast(&self.cast, &movie.actors);
        movie.overview = self.overview.trim().to_string();
        movie.upc = normalize_upc(&self.upc);
        movie.locality = self.locality.trim().to_string();
        movie.collection_number = self.collection_number.trim().to_string();
        movie.case_type = self.case_type.trim().to_string();
        movie.cuts = self.cuts.trim().to_string();
        movie.purchase_date = self.purchase_date.trim().to_string();
        movie.purchase_price = self.purchase_price.trim().to_string();
        movie.location = self.location.trim().to_string();
        movie.notes = self.notes.trim().to_string();
        Ok(())
    }
}

fn parse_optional_number<T>(value: &str, label: &str) -> Result<Option<T>, String>
where
    T: std::str::FromStr,
{
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<T>()
        .map(Some)
        .map_err(|_| format!("{label} must be a number."))
}

fn parse_cast(input: &str, previous: &[Actor]) -> Vec<Actor> {
    let mut actors = Vec::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let (name, role) = if let Some((name, role)) = line.split_once(" as ") {
            (name.trim(), role.trim())
        } else if let Some((name, role)) = line.split_once(" - ") {
            (name.trim(), role.trim())
        } else {
            (line, "")
        };

        if name.is_empty() {
            continue;
        }

        let mut actor = Actor::new(name, role);
        if let Some(previous) = previous
            .iter()
            .find(|previous| previous.name.eq_ignore_ascii_case(name))
        {
            actor.voice = previous.voice;
            actor.uncredited = previous.uncredited;
        }
        actors.push(actor);
    }
    actors
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorAction {
    Save,
    Cancel,
}

impl KinoCatalogApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        collection: Collection,
        data_path: PathBuf,
        load_error: Option<String>,
    ) -> Self {
        configure_style(&cc.egui_ctx);

        let omdb_key = storage::load_omdb_key(&data_path);
        let tmdb_key = storage::load_tmdb_key(&data_path);
        let mut app = Self {
            collection,
            data_path,
            selected: None,
            search: String::new(),
            format_filter: None,
            only_3d: false,
            only_missing_covers: false,
            only_missing_year: false,
            only_missing_original_title: false,
            filtered: Vec::new(),
            list_dirty: true,
            editor: None,
            import_dialog: None,
            pending_delete: None,
            status: load_error.map(StatusMessage::error),
            show_full_cast: false,
            omdb_key,
            omdb_dialog: None,
            tmdb_key,
            tmdb_dialog: None,
            invelos_dialog: None,
            dvdcompare_dialog: None,
            covers: CoverIndex::scan(&covers::directory()),
            cover_textures: CoverTextures::default(),
            cover_viewer: None,
            show_about: false,
            update_dialog: None,
        };

        app.collection.sort();
        app.refresh_filter();
        if let Some(index) = app.filtered.first() {
            app.selected = Some(app.collection.movies[*index].id.clone());
        }
        app
    }

    fn refresh_filter(&mut self) {
        self.filtered = filter_movies(
            &self.collection,
            &self.covers,
            &self.search,
            self.format_filter,
            self.only_3d,
            MissingFilters {
                covers: self.only_missing_covers,
                year: self.only_missing_year,
                original_title: self.only_missing_original_title,
            },
        );
    }

    fn open_add(&mut self) {
        self.editor = Some(Editor::new());
    }

    fn open_edit(&mut self, movie: &Movie) {
        self.editor = Some(Editor::edit(movie));
    }

    fn commit(&mut self, message: impl Into<String>) {
        self.list_dirty = true;
        let message = message.into();
        match storage::save(&self.data_path, &mut self.collection) {
            Ok(()) => self.status = Some(StatusMessage::success(message)),
            Err(error) => {
                self.status = Some(StatusMessage::error(format!(
                    "The collection changed in memory, but could not be saved: {error}"
                )));
            }
        }
    }

    /// Parses and merges an XML file. The caller decides how to present the
    /// resulting message, so validation errors can stay inside the dialog.
    fn import_file(&mut self, path: &Path) -> Result<String, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let parsed = import::parse_collection(&bytes)?;
        let total = parsed.movies.len();
        let first_id = parsed.movies.first().map(|movie| movie.id.clone());
        let stats = self.collection.merge_imported(parsed.movies);
        self.list_dirty = true;

        let selection_is_missing = self
            .selected
            .as_ref()
            .is_none_or(|id| self.collection.find(id).is_none());
        if selection_is_missing {
            self.selected = first_id;
        }

        let skipped = if parsed.skipped > 0 {
            format!(", {} skipped", parsed.skipped)
        } else {
            String::new()
        };

        Ok(format!(
            "Imported {total} titles from {}: {} added, {} updated{skipped}.",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            stats.added,
            stats.updated
        ))
    }

    /// Asks for a Letterboxd ZIP export and merges its ratings into the
    /// collection. The native file picker is opened directly from the toolbar,
    /// so there is no separate import dialog.
    fn import_letterboxd(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Letterboxd export", &["zip"])
            .set_title("Select a Letterboxd export")
            .pick_file()
        else {
            return;
        };

        match self.apply_letterboxd_file(&path) {
            Ok(message) => self.commit(message),
            Err(error) => self.status = Some(StatusMessage::error(error)),
        }
    }

    fn apply_letterboxd_file(&mut self, path: &Path) -> Result<String, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let ratings = letterboxd::parse_export(&bytes)?;
        if ratings.is_empty() {
            return Err("The Letterboxd export did not contain any ratings.".to_string());
        }

        let stats = letterboxd::apply(&mut self.collection, &ratings);
        self.list_dirty = true;
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let skipped = if stats.skipped > 0 {
            format!(", {} skipped", stats.skipped)
        } else {
            String::new()
        };
        Ok(format!(
            "Imported {} ratings from {file_name}: {} movies rated{skipped}.",
            stats.entries, stats.rated
        ))
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (new_movie, focus_search) = ctx.input(|input| {
            let command = input.modifiers.command || input.modifiers.ctrl;
            (
                command && input.key_pressed(egui::Key::N),
                command && input.key_pressed(egui::Key::F),
            )
        });

        if new_movie && self.editor.is_none() && self.import_dialog.is_none() {
            self.open_add();
        }
        if focus_search {
            ctx.memory_mut(|memory| memory.request_focus(egui::Id::unique("search_field")));
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| {
                    path.extension().is_some_and(|extension| {
                        extension.to_string_lossy().eq_ignore_ascii_case("xml")
                    })
                })
                .collect()
        });

        for path in dropped {
            match self.import_file(&path) {
                Ok(message) => self.commit(message),
                Err(error) => {
                    self.status = Some(StatusMessage::error(error));
                    self.import_dialog = Some(ImportDialog {
                        path: path.display().to_string(),
                        error: None,
                    });
                }
            }
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let mut add = false;
        let mut import = false;
        let mut import_letterboxd = false;
        let mut open_about = false;
        let mut check_updates = false;

        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(TOP_BG)
                    .inner_margin(Margin::symmetric(18, 11)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("KinoCatalog").size(21.0).strong().color(TEXT));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("DVD & Blu-ray collection")
                            .size(11.5)
                            .color(MUTED),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let menu_button = hamburger_button(ui);
                        egui::Popup::menu(&menu_button)
                            .align(egui::RectAlign::BOTTOM_END)
                            .show(|ui| {
                                ui.set_min_width(190.0);
                                if ui
                                    .button("Import Letterboxd…")
                                    .on_hover_text("Import ratings from a Letterboxd .zip export")
                                    .clicked()
                                {
                                    import_letterboxd = true;
                                    ui.close();
                                }
                                if ui
                                    .button("Import XML…")
                                    .on_hover_text("Import a DVD Profiler Collection.xml")
                                    .clicked()
                                {
                                    import = true;
                                    ui.close();
                                }
                                if ui
                                    .button("Add movie")
                                    .on_hover_text("Add a movie manually (Ctrl+N)")
                                    .clicked()
                                {
                                    add = true;
                                    ui.close();
                                }
                                ui.separator();
                                if ui
                                    .button("Update to new version…")
                                    .on_hover_text(
                                        "Check GitHub for a newer release and install it",
                                    )
                                    .clicked()
                                {
                                    check_updates = true;
                                    ui.close();
                                }
                                if ui
                                    .button("About")
                                    .on_hover_text("About KinoCatalog")
                                    .clicked()
                                {
                                    open_about = true;
                                    ui.close();
                                }
                            });
                    });
                });
            });

        if add {
            self.open_add();
        }
        if import {
            self.import_dialog = Some(ImportDialog::default());
        }
        if import_letterboxd {
            self.import_letterboxd();
        }
        if open_about {
            self.show_about = true;
        }
        if check_updates {
            self.start_update_check();
        }
    }

    fn list_panel(&mut self, ui: &mut Ui) {
        egui::Panel::left("collection_panel")
            .resizable(true)
            .default_size(344.0)
            .size_range(280.0..=560.0)
            .frame(
                egui::Frame::new()
                    .fill(SIDE_BG)
                    .inner_margin(Margin::symmetric(12, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("COLLECTION").size(10.5).strong().color(MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{} titles", self.collection.movies.len()))
                                .size(11.0)
                                .color(MUTED),
                        );
                    });
                });

                ui.add_space(7.0);
                ui.horizontal(|ui| {
                    // Always reserve room for the Clear button. Reserving it only
                    // when the field is non-empty would widen this row on the
                    // frame the first character is typed, which makes the
                    // resizable panel grow.
                    let width = (ui.available_width() - 80.0).max(120.0);
                    let response = ui.add(
                        TextEdit::singleline(&mut self.search)
                            .id(egui::Id::unique("search_field"))
                            .desired_width(width)
                            .hint_text("Search title, actor, year…"),
                    );
                    if response.changed() {
                        self.list_dirty = true;
                    }
                    if !self.search.is_empty() && ui.small_button("Clear").clicked() {
                        self.search.clear();
                        self.list_dirty = true;
                    }
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let active = usize::from(self.only_missing_covers)
                        + usize::from(self.only_missing_year)
                        + usize::from(self.only_missing_original_title);
                    let label = if active > 0 {
                        format!("Filter by ({active}) ▾")
                    } else {
                        "Filter by ▾".to_string()
                    };
                    let response = ui.add(egui::Button::new(RichText::new(label).size(11.5)));
                    egui::Popup::menu(&response).show(|ui| {
                        ui.set_min_width(190.0);
                        if ui
                            .checkbox(&mut self.only_missing_covers, "Missing covers")
                            .on_hover_text("Show only movies without front or back cover pictures")
                            .changed()
                        {
                            self.list_dirty = true;
                        }
                        if ui
                            .checkbox(&mut self.only_missing_year, "Missing year")
                            .on_hover_text("Show only movies without a release year")
                            .changed()
                        {
                            self.list_dirty = true;
                        }
                        if ui
                            .checkbox(
                                &mut self.only_missing_original_title,
                                "Missing original title",
                            )
                            .on_hover_text("Show only movies without an original title")
                            .changed()
                        {
                            self.list_dirty = true;
                        }
                    });
                });

                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if ui
                        .selectable_label(
                            self.format_filter.is_none(),
                            RichText::new("All").size(11.5),
                        )
                        .clicked()
                    {
                        self.format_filter = None;
                        self.only_3d = false;
                        self.list_dirty = true;
                    }
                    for format in DiscFormat::ALL {
                        if ui
                            .selectable_label(
                                self.format_filter == Some(format),
                                RichText::new(format.label()).size(11.5),
                            )
                            .clicked()
                        {
                            self.format_filter = Some(format);
                            self.list_dirty = true;
                        }
                    }
                    if ui
                        .selectable_label(self.only_3d, RichText::new("3D Blu-ray").size(11.5))
                        .on_hover_text("Show only 3D editions")
                        .clicked()
                    {
                        self.only_3d = !self.only_3d;
                        self.list_dirty = true;
                    }
                });

                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "Showing {} of {}",
                        self.filtered.len(),
                        self.collection.movies.len()
                    ))
                    .size(10.5)
                    .color(MUTED),
                );
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                if self.filtered.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(if self.collection.movies.is_empty() {
                                "No movies yet"
                            } else {
                                "No matches"
                            })
                            .size(14.0)
                            .color(MUTED),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(if self.collection.movies.is_empty() {
                                "Import a collection or add a title."
                            } else {
                                "Try a different search or filter."
                            })
                            .size(11.5)
                            .color(MUTED),
                        );
                    });
                    return;
                }

                let filtered = self.filtered.clone();
                egui::ScrollArea::vertical()
                    .id_salt("movie_list")
                    .auto_shrink([false, false])
                    .show_rows(ui, 44.0, filtered.len(), |ui, range| {
                        for index in range {
                            let movie = &self.collection.movies[filtered[index]];
                            let selected = self.selected.as_deref() == Some(movie.id.as_str());
                            if movie_row(ui, movie, selected).clicked() {
                                self.selected = Some(movie.id.clone());
                                self.show_full_cast = false;
                            }
                        }
                    });
            });
    }

    fn details_panel(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .inner_margin(Margin::symmetric(24, 18)),
            )
            .show(ui, |ui| {
                self.details_ui(ui);
            });
    }

    fn details_ui(&mut self, ui: &mut Ui) {
        let Some(id) = self.selected.clone() else {
            self.empty_details(ui);
            return;
        };
        let Some(movie) = self.collection.find(&id).cloned() else {
            self.selected = None;
            self.empty_details(ui);
            return;
        };

        let mut edit = false;
        let mut delete = false;
        let mut update_omdb = false;
        let mut update_tmdb = false;
        let mut update_invelos = false;
        let mut update_dvdcompare = false;
        let mut show_full_cast = self.show_full_cast;
        let movie_covers = self.covers.for_upc(&movie.upc).cloned();

        egui::ScrollArea::vertical()
            .id_salt(format!("details_{}", movie.id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(900.0);

                ui.horizontal(|ui| {
                    format_badge(ui, &movie);
                    if !movie.collection_number.is_empty() {
                        ui.label(
                            RichText::new(format!("Collection #{}", movie.collection_number))
                                .size(11.5)
                                .color(MUTED),
                        );
                    }

                    ui.with_layout(
                        Layout::right_to_left(Align::Center).with_main_wrap(true),
                        |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Delete").color(ERROR))
                                    .fill(Color32::from_rgb(54, 32, 36)),
                            )
                            .clicked()
                        {
                            delete = true;
                        }
                        if ui.button("Edit").clicked() {
                            edit = true;
                        }
                        if ui
                            .button("Update from OMDB")
                            .on_hover_text(
                                "Compare this movie with OMDb and choose fields to update",
                            )
                            .clicked()
                        {
                            update_omdb = true;
                        }
                        if ui
                            .button("Update from TMDB")
                            .on_hover_text("Search The Movie Database and choose fields to update")
                            .clicked()
                        {
                            update_tmdb = true;
                        }
                        if ui
                            .button("Update from Invelos")
                            .on_hover_text(
                                "Look up this movie's barcode in Invelos and choose fields to update",
                            )
                            .clicked()
                        {
                            update_invelos = true;
                        }
                        if ui
                            .button("Update from DVDCompare")
                            .on_hover_text(
                                "Search DVDCompare, choose a release, and compare disc details",
                            )
                            .clicked()
                        {
                            update_dvdcompare = true;
                        }
                    });
                });

                ui.add_space(5.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&movie.title).size(29.0).strong().color(TEXT));
                    if let Some(rating) = movie.letterboxd_rating {
                        ui.add_space(8.0);
                        draw_rating_stars(ui, rating);
                    }
                });

                let mut meta = Vec::new();
                if !movie.year_label().is_empty() {
                    meta.push(movie.year_label());
                }
                if !movie.runtime_label().is_empty() {
                    meta.push(movie.runtime_label());
                }
                if !movie.rating.is_empty() {
                    meta.push(movie.rating.clone());
                }
                if !meta.is_empty() {
                    ui.add_space(3.0);
                    ui.label(RichText::new(meta.join("   ·   ")).size(13.0).color(MUTED));
                }

                if !movie.genres.is_empty() {
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        for genre in &movie.genres {
                            chip(ui, genre);
                        }
                    });
                }

                if let Some(covers) = &movie_covers {
                    section_title(ui, "Covers");
                    draw_covers(ui, &mut self.cover_textures, covers, &mut self.cover_viewer);
                }

                if !movie.overview.is_empty() {
                    section_title(ui, "Synopsis");
                    ui.label(RichText::new(&movie.overview).size(13.5).color(TEXT));
                }

                section_title(ui, "Details");
                let mut facts: Vec<(&str, String)> = vec![
                    ("Format", movie.format_label()),
                    ("Original title", movie.original_title.clone()),
                    ("Alternative titles", join_list(&movie.alternative_titles)),
                    ("Released", movie.released.clone()),
                    (
                        if movie.format_label().starts_with("Blu-ray") {
                            "Area Code"
                        } else {
                            "Region"
                        },
                        if movie.regions.is_empty() {
                            String::new()
                        } else {
                            join_list(&movie.regions)
                        },
                    ),
                    (
                        "Discs",
                        match movie.disc_count {
                            0 => String::new(),
                            1 => "1 disc".to_string(),
                            count => format!("{count} discs"),
                        },
                    ),
                    ("Case", movie.case_type.clone()),
                    ("Country", movie.country.clone()),
                    ("Locality", movie.locality.clone()),
                    ("UPC", movie.upc.clone()),
                    ("Rating details", movie.rating_details.clone()),
                    ("Location", movie.location.clone()),
                ];
                let imdb_url = imdb_url(&movie.imdb_id);
                facts.retain(|(_, value)| !value.trim().is_empty());
                if facts.is_empty() && imdb_url.is_none() {
                    ui.label(RichText::new("No extra details.").size(12.0).color(MUTED));
                } else {
                    egui::Grid::new("movie_facts")
                        .num_columns(2)
                        .spacing([26.0, 7.0])
                        .min_col_width(118.0)
                        .show(ui, |ui| {
                            for (label, value) in facts {
                                fact(ui, label, &value);
                            }
                            if let Some(url) = &imdb_url {
                                ui.label(RichText::new("IMDb").size(11.5).color(MUTED));
                                ui.hyperlink_to(RichText::new(url).size(13.0).color(ACCENT), url)
                                    .on_hover_text("Open this title on IMDb");
                                ui.end_row();
                            }
                        });
                }

                let mut credits: Vec<(&str, String)> = Vec::new();
                if !movie.directors.is_empty() {
                    credits.push(("Directed by", join_list(&movie.directors)));
                }
                if !movie.writers.is_empty() {
                    credits.push(("Written by", join_list(&movie.writers)));
                }
                if !movie.studios.is_empty() {
                    credits.push(("Studio", join_list(&movie.studios)));
                }
                if !movie.media_companies.is_empty() {
                    credits.push(("Distributor", join_list(&movie.media_companies)));
                }
                if !credits.is_empty() {
                    section_title(ui, "Credits");
                    egui::Grid::new("movie_credits")
                        .num_columns(2)
                        .spacing([26.0, 7.0])
                        .min_col_width(118.0)
                        .show(ui, |ui| {
                            for (label, value) in credits {
                                fact(ui, label, &value);
                            }
                        });
                }

                if !movie.actors.is_empty() {
                    section_title(ui, "Cast");
                    let limit = if show_full_cast {
                        movie.actors.len()
                    } else {
                        15.min(movie.actors.len())
                    };
                    for actor in movie.actors.iter().take(limit) {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&actor.name).size(13.0).strong());
                            if !actor.role.is_empty() {
                                ui.label(RichText::new(&actor.role).size(12.5).color(MUTED));
                            }
                            if actor.voice {
                                ui.label(RichText::new("voice").size(10.0).color(ACCENT));
                            }
                            if actor.uncredited {
                                ui.label(RichText::new("uncredited").size(10.0).color(MUTED));
                            }
                        });
                    }
                    if movie.actors.len() > 15 {
                        if show_full_cast {
                            if ui.link("Show fewer").clicked() {
                                show_full_cast = false;
                            }
                        } else if ui
                            .link(format!("Show all {} cast members", movie.actors.len()))
                            .clicked()
                        {
                            show_full_cast = true;
                        }
                    }
                }

                if !movie.audio.is_empty() || !movie.subtitles.is_empty() {
                    section_title(ui, "Audio & subtitles");
                    if !movie.audio.is_empty() {
                        ui.label(RichText::new("Audio").size(11.5).strong().color(MUTED));
                        string_list(ui, &movie.audio);
                    }
                    if !movie.subtitles.is_empty() {
                        ui.add_space(5.0);
                        ui.label(RichText::new("Subtitles").size(11.5).strong().color(MUTED));
                        ui.label(
                            RichText::new(join_list(&movie.subtitles))
                                .size(13.0)
                                .color(TEXT),
                        );
                    }
                }

                if !movie.cuts.is_empty() {
                    section_title(ui, "Cuts");
                    ui.label(RichText::new(&movie.cuts).size(13.0).color(TEXT));
                }

                if !movie.purchase_date.is_empty()
                    || !movie.purchase_price.is_empty()
                    || !movie.purchase_place.is_empty()
                    || !movie.location.is_empty()
                {
                    section_title(ui, "Purchase");
                    egui::Grid::new("movie_purchase")
                        .num_columns(2)
                        .spacing([26.0, 7.0])
                        .min_col_width(118.0)
                        .show(ui, |ui| {
                            if !movie.purchase_date.is_empty() {
                                fact(ui, "Purchased", &movie.purchase_date);
                            }
                            if !movie.purchase_price.is_empty() {
                                fact(ui, "Price", &movie.purchase_price);
                            }
                            if !movie.purchase_place.is_empty() {
                                fact(ui, "Place", &movie.purchase_place);
                            }
                            if !movie.location.is_empty() {
                                fact(ui, "Location", &movie.location);
                            }
                        });
                }

                if !movie.notes.is_empty() {
                    section_title(ui, "Notes");
                    ui.label(RichText::new(&movie.notes).size(13.0).color(TEXT));
                }

                ui.add_space(24.0);
            });

        self.show_full_cast = show_full_cast;

        if edit {
            self.open_edit(&movie);
        }
        if delete {
            self.pending_delete = Some(movie.id.clone());
        }
        if update_omdb {
            self.open_omdb(&movie);
            ui.ctx().request_repaint();
        }
        if update_tmdb {
            self.open_tmdb(&movie);
            ui.ctx().request_repaint();
        }
        if update_invelos {
            self.open_invelos(&movie);
            ui.ctx().request_repaint();
        }
        if update_dvdcompare {
            self.open_dvdcompare(&movie);
            ui.ctx().request_repaint();
        }
    }

    /// Starts the OMDb workflow for a movie, asking for an API key when none
    /// has been stored yet.
    fn open_omdb(&mut self, movie: &Movie) {
        self.tmdb_dialog = None;
        self.invelos_dialog = None;
        self.dvdcompare_dialog = None;
        let dialog = OmdbDialog {
            movie_id: movie.id.clone(),
            title: movie.title.clone(),
            year: movie.year,
            state: OmdbState::RequestKey {
                input: self.omdb_key.clone().unwrap_or_default(),
                error: None,
            },
        };

        if let Some(key) = self.omdb_key.clone() {
            self.start_omdb_fetch(dialog, key);
        } else {
            self.omdb_dialog = Some(dialog);
        }
    }

    fn start_omdb_fetch(&mut self, mut dialog: OmdbDialog, key: String) {
        let (sender, receiver) = mpsc::channel();
        let title = dialog.title.clone();
        let year = dialog.year;
        let worker_key = key.clone();
        std::thread::spawn(move || {
            let _ = sender.send(omdb::fetch(&worker_key, &title, year));
        });
        dialog.state = OmdbState::Fetching(receiver);
        self.omdb_key = Some(key);
        self.omdb_dialog = Some(dialog);
    }

    /// Polls a running OMDb request and draws the key, progress, comparison
    /// and error windows. One dialog instance carries the movie through the
    /// whole workflow.
    fn omdb_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.omdb_dialog.take() else {
            return;
        };

        if let OmdbState::Fetching(receiver) = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("The OMDb lookup stopped unexpectedly.".to_string()));
                }
            }

            if let Some(result) = finished {
                match result {
                    Ok(details) => {
                        let Some(movie) = self.collection.find(&dialog.movie_id) else {
                            self.status = Some(StatusMessage::error(
                                "This movie is no longer in the collection.".to_string(),
                            ));
                            return;
                        };
                        let changes = details.changes(movie);
                        dialog.state = OmdbState::Comparing { details, changes };
                    }
                    Err(message) => dialog.state = OmdbState::Failed { message },
                }
            }
        }

        let mut open = true;
        let mut action = None;
        let lookup = match dialog.year {
            Some(year) => format!("“{}” ({year})", dialog.title),
            None => format!("“{}”", dialog.title),
        };

        match &mut dialog.state {
            OmdbState::RequestKey { input, error } => {
                egui::Window::new("OMDb API key")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(470.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!("Enter your OMDb API key to update {lookup}."))
                                .size(12.5)
                                .color(MUTED),
                        );
                        ui.add_space(8.0);
                        ui.add(
                            TextEdit::singleline(input)
                                .desired_width(f32::INFINITY)
                                .hint_text("OMDb API key"),
                        );
                        ui.add_space(5.0);
                        ui.hyperlink_to(
                            RichText::new("Get a free key at omdbapi.com")
                                .size(11.5)
                                .color(ACCENT),
                            "https://www.omdbapi.com/apikey.aspx",
                        );

                        if let Some(error) = error {
                            ui.add_space(5.0);
                            ui.label(RichText::new(error.as_str()).size(12.0).color(ERROR));
                        }

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Save and continue").strong().color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                let key = input.trim().to_string();
                                if key.is_empty() {
                                    *error = Some("Enter the API key.".to_string());
                                } else {
                                    action = Some(OmdbAction::SaveKey(key));
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(OmdbAction::Close);
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "The key is stored beside your collection in {}.",
                                storage::display_path(&storage::omdb_key_path(&self.data_path))
                            ))
                            .size(10.5)
                            .color(MUTED),
                        );
                    });
            }
            OmdbState::Fetching(_) => {
                egui::Window::new("Update from OMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(380.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!("Looking up {lookup} on OMDb…"))
                                    .size(12.5)
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(10.0);
                        if ui.button("Cancel").clicked() {
                            action = Some(OmdbAction::Close);
                        }
                    });
            }
            OmdbState::Comparing { changes, .. } => {
                egui::Window::new("Update from OMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(660.0)
                    .max_height(720.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Choose which OMDb values to save for {lookup}. Click a field to select it."
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Select all").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = true;
                                }
                            }
                            if ui.button("Select none").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = false;
                                }
                            }
                        });
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(440.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, change) in changes.iter_mut().enumerate() {
                                    change_row(ui, index, change, "OMDb");
                                    ui.add_space(7.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let any_selected = changes.iter().any(|change| change.selected);
                            if ui
                                .add_enabled(
                                    any_selected,
                                    egui::Button::new(
                                        RichText::new("Update movie").strong().color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                action = Some(OmdbAction::Save);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(OmdbAction::Close);
                            }
                        });
                    });
            }
            OmdbState::Failed { message } => {
                egui::Window::new("Update from OMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(470.0)
                    .show(ctx, |ui| {
                        ui.label(RichText::new(message.as_str()).size(12.5).color(ERROR));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Check the key, the title, or your connection.")
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Try again").clicked() {
                                action = Some(OmdbAction::Retry);
                            }
                            if ui.button("Change API key").clicked() {
                                action = Some(OmdbAction::ChangeKey);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(OmdbAction::Close);
                            }
                        });
                    });
            }
        }

        if !open && action.is_none() {
            action = Some(OmdbAction::Close);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            action = Some(OmdbAction::Close);
        }

        match action {
            Some(OmdbAction::Close) => {
                self.omdb_dialog = None;
            }
            Some(OmdbAction::SaveKey(key)) => match storage::save_omdb_key(&self.data_path, &key) {
                Ok(()) => self.start_omdb_fetch(dialog, key),
                Err(error) => {
                    if let OmdbState::RequestKey { error: slot, .. } = &mut dialog.state {
                        *slot = Some(error);
                    }
                    self.omdb_dialog = Some(dialog);
                }
            },
            Some(OmdbAction::Retry) => {
                if let Some(key) = self.omdb_key.clone() {
                    self.start_omdb_fetch(dialog, key);
                } else {
                    dialog.state = OmdbState::RequestKey {
                        input: String::new(),
                        error: None,
                    };
                    self.omdb_dialog = Some(dialog);
                }
            }
            Some(OmdbAction::ChangeKey) => {
                dialog.state = OmdbState::RequestKey {
                    input: self.omdb_key.clone().unwrap_or_default(),
                    error: None,
                };
                self.omdb_dialog = Some(dialog);
            }
            Some(OmdbAction::Save) => {
                let applied = match &dialog.state {
                    OmdbState::Comparing { details, changes } => {
                        let fields: Vec<_> = changes
                            .iter()
                            .filter(|change| change.selected)
                            .map(|change| change.field)
                            .collect();
                        let count = fields.len();
                        self.collection.find_mut(&dialog.movie_id).map(|movie| {
                            details.apply(movie, fields.into_iter());
                            (movie.title.clone(), count)
                        })
                    }
                    _ => None,
                };
                self.omdb_dialog = None;
                match applied {
                    Some((title, count)) if count > 0 => self.commit(format!(
                        "Updated “{title}” from OMDb ({count} field{}).",
                        if count == 1 { "" } else { "s" }
                    )),
                    Some(_) => {
                        self.status = Some(StatusMessage::info("No fields were selected."));
                    }
                    None => {
                        self.status = Some(StatusMessage::error(
                            "This movie is no longer in the collection.".to_string(),
                        ));
                    }
                }
            }
            None => {
                self.omdb_dialog = Some(dialog);
            }
        }
    }

    /// Starts the TMDB workflow. The title search runs first; the user then
    /// chooses the matching result before details are loaded by TMDB id.
    fn open_tmdb(&mut self, movie: &Movie) {
        self.omdb_dialog = None;
        self.invelos_dialog = None;
        self.dvdcompare_dialog = None;
        let dialog = TmdbDialog {
            movie_id: movie.id.clone(),
            title: movie.title.clone(),
            query: movie.title.clone(),
            last_query: movie.title.clone(),
            year: movie.year,
            results: Vec::new(),
            selected: None,
            state: TmdbState::RequestKey {
                input: self.tmdb_key.clone().unwrap_or_default(),
                error: None,
            },
        };

        if let Some(key) = self.tmdb_key.clone() {
            self.start_tmdb_search(dialog, key);
        } else {
            self.tmdb_dialog = Some(dialog);
        }
    }

    fn start_tmdb_search(&mut self, mut dialog: TmdbDialog, key: String) {
        let (sender, receiver) = mpsc::channel();
        let query = dialog.query.trim().to_string();
        dialog.last_query = query.clone();
        let worker_key = key.clone();
        std::thread::spawn(move || {
            let _ = sender.send(tmdb::search(&worker_key, &query));
        });
        dialog.results.clear();
        dialog.selected = None;
        dialog.state = TmdbState::Searching(receiver);
        self.tmdb_key = Some(key);
        self.tmdb_dialog = Some(dialog);
    }

    fn start_tmdb_details(
        &mut self,
        mut dialog: TmdbDialog,
        key: String,
        result: tmdb::SearchResult,
    ) {
        let (sender, receiver) = mpsc::channel();
        let worker_key = key.clone();
        let id = result.id;
        std::thread::spawn(move || {
            let _ = sender.send(tmdb::fetch(&worker_key, id));
        });
        dialog.selected = dialog
            .results
            .iter()
            .position(|candidate| candidate.id == result.id);
        dialog.state = TmdbState::Fetching { result, receiver };
        self.tmdb_key = Some(key);
        self.tmdb_dialog = Some(dialog);
    }

    /// Polls the TMDB search and detail requests, then draws the key, search
    /// results, progress, comparison and error windows.
    fn tmdb_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.tmdb_dialog.take() else {
            return;
        };

        if let TmdbState::Searching(receiver) = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("The TMDB search stopped unexpectedly.".to_string()));
                }
            }

            if let Some(result) = finished {
                match result {
                    Ok(results) if results.is_empty() => {
                        dialog.state = TmdbState::Failed {
                            message: format!(
                                "TMDB found no matches for “{}”.",
                                dialog.query.trim()
                            ),
                            retry: TmdbRetry::Search,
                        };
                    }
                    Ok(results) => {
                        let preferred = results
                            .iter()
                            .position(|result| result.year == dialog.year)
                            .unwrap_or(0);
                        dialog.selected = Some(preferred);
                        dialog.results = results;
                        dialog.state = TmdbState::Choosing;
                    }
                    Err(message) => {
                        dialog.state = TmdbState::Failed {
                            message,
                            retry: TmdbRetry::Search,
                        };
                    }
                }
            }
        }

        if let TmdbState::Fetching { receiver, .. } = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("The TMDB lookup stopped unexpectedly.".to_string()));
                }
            }

            if let Some(result) = finished {
                match result {
                    Ok(mut details) => {
                        let chosen = match &dialog.state {
                            TmdbState::Fetching { result, .. } => result.clone(),
                            _ => unreachable!("only a fetching dialog can finish a lookup"),
                        };
                        add_picked_title(
                            &mut details,
                            &dialog.last_query,
                            &dialog.title,
                            &chosen.title,
                        );
                        let Some(movie) = self.collection.find(&dialog.movie_id) else {
                            self.status = Some(StatusMessage::error(
                                "This movie is no longer in the collection.".to_string(),
                            ));
                            return;
                        };
                        let changes = details.changes(movie);
                        dialog.state = TmdbState::Comparing {
                            result: chosen,
                            details: Box::new(details),
                            changes,
                        };
                    }
                    Err(message) => {
                        dialog.state = TmdbState::Failed {
                            message,
                            retry: TmdbRetry::Details,
                        };
                    }
                }
            }
        }

        let mut open = true;
        let mut action = None;
        let lookup = match dialog.year {
            Some(year) => format!("“{}” ({year})", dialog.title),
            None => format!("“{}”", dialog.title),
        };

        match &mut dialog.state {
            TmdbState::RequestKey { input, error } => {
                egui::Window::new("TMDB API key")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(490.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Enter your TMDB API key or read access token to update {lookup}."
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(8.0);
                        ui.add(
                            TextEdit::singleline(input)
                                .desired_width(f32::INFINITY)
                                .hint_text("TMDB API key (v3) or read access token (v4)"),
                        );
                        ui.add_space(5.0);
                        ui.hyperlink_to(
                            RichText::new("Get a key at themoviedb.org")
                                .size(11.5)
                                .color(ACCENT),
                            "https://www.themoviedb.org/settings/api",
                        );

                        if let Some(error) = error {
                            ui.add_space(5.0);
                            ui.label(RichText::new(error.as_str()).size(12.0).color(ERROR));
                        }

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Save and continue").strong().color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                let key = input.trim().to_string();
                                if key.is_empty() {
                                    *error = Some(
                                        "Enter the TMDB API key or read access token.".to_string(),
                                    );
                                } else {
                                    action = Some(TmdbAction::SaveKey(key));
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(TmdbAction::Close);
                            }
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "The key is stored beside your collection in {}.",
                                storage::display_path(&storage::tmdb_key_path(&self.data_path))
                            ))
                            .size(10.5)
                            .color(MUTED),
                        );
                    });
            }
            TmdbState::Searching(_) => {
                egui::Window::new("Update from TMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(400.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!("Searching TMDB for {lookup}…"))
                                    .size(12.5)
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(10.0);
                        if ui.button("Cancel").clicked() {
                            action = Some(TmdbAction::Close);
                        }
                    });
            }
            TmdbState::Choosing => {
                egui::Window::new("Update from TMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(620.0)
                    .max_height(720.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Choose the correct match for {lookup}. TMDB returned {} result{}.",
                                dialog.results.len(),
                                if dialog.results.len() == 1 { "" } else { "s" }
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Search").size(12.0).color(MUTED));
                            let response = ui.add(
                                TextEdit::singleline(&mut dialog.query)
                                    .desired_width(360.0)
                                    .hint_text("Movie title"),
                            );
                            let enter = response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter));
                            if ui.button("Search again").clicked() || enter {
                                action = Some(TmdbAction::SearchAgain);
                            }
                        });
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, result) in dialog.results.iter().enumerate() {
                                    let selected = dialog.selected == Some(index);
                                    if tmdb_result_row(ui, index, result, selected).clicked() {
                                        dialog.selected = Some(index);
                                    }
                                    ui.add_space(6.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let selected = dialog.selected;
                            if ui
                                .add_enabled(
                                    selected.is_some(),
                                    egui::Button::new(
                                        RichText::new("Load selected title").strong().color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                                && let Some(index) = selected
                            {
                                action = Some(TmdbAction::Fetch(index));
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(TmdbAction::Close);
                            }
                        });
                    });
            }
            TmdbState::Fetching { result, .. } => {
                egui::Window::new("Update from TMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(420.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!(
                                    "Loading “{}”{} from TMDB…",
                                    result.title,
                                    result
                                        .year
                                        .map(|year| format!(" ({year})"))
                                        .unwrap_or_default()
                                ))
                                .size(12.5)
                                .color(TEXT),
                            );
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Choose another title").clicked() {
                                action = Some(TmdbAction::ChooseAnother);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(TmdbAction::Close);
                            }
                        });
                    });
            }
            TmdbState::Comparing {
                result: matched,
                changes,
                ..
            } => {
                egui::Window::new("Update from TMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(660.0)
                    .max_height(720.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Choose which TMDB values to save for {lookup}. Matched “{}”{}.",
                                matched.title,
                                matched
                                    .year
                                    .map(|year| format!(" ({year})"))
                                    .unwrap_or_default()
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Select all").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = true;
                                }
                            }
                            if ui.button("Select none").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = false;
                                }
                            }
                        });
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(440.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, change) in changes.iter_mut().enumerate() {
                                    change_row(ui, index, change, "TMDB");
                                    ui.add_space(7.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let any_selected = changes.iter().any(|change| change.selected);
                            if ui
                                .add_enabled(
                                    any_selected,
                                    egui::Button::new(
                                        RichText::new("Update movie").strong().color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                action = Some(TmdbAction::Save);
                            }
                            if ui.button("Choose another title").clicked() {
                                action = Some(TmdbAction::ChooseAnother);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(TmdbAction::Close);
                            }
                        });
                    });
            }
            TmdbState::Failed { message, retry } => {
                egui::Window::new("Update from TMDB")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(490.0)
                    .show(ctx, |ui| {
                        ui.label(RichText::new(message.as_str()).size(12.5).color(ERROR));
                        ui.add_space(6.0);
                        if matches!(*retry, TmdbRetry::Search) {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Title").size(12.0).color(MUTED));
                                ui.add(
                                    TextEdit::singleline(&mut dialog.query)
                                        .desired_width(320.0)
                                        .hint_text("Movie title"),
                                );
                            });
                            ui.add_space(6.0);
                        }
                        ui.label(
                            RichText::new("Check the key, the title, or your connection.")
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Try again").clicked() {
                                action = Some(TmdbAction::Retry);
                            }
                            if matches!(*retry, TmdbRetry::Details)
                                && ui.button("Choose another title").clicked()
                            {
                                action = Some(TmdbAction::ChooseAnother);
                            }
                            if ui.button("Change API key").clicked() {
                                action = Some(TmdbAction::ChangeKey);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(TmdbAction::Close);
                            }
                        });
                    });
            }
        }

        if !open && action.is_none() {
            action = Some(TmdbAction::Close);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            action = Some(TmdbAction::Close);
        }

        match action {
            Some(TmdbAction::Close) => {
                self.tmdb_dialog = None;
            }
            Some(TmdbAction::SaveKey(key)) => match storage::save_tmdb_key(&self.data_path, &key) {
                Ok(()) => self.start_tmdb_search(dialog, key),
                Err(error) => {
                    if let TmdbState::RequestKey { error: slot, .. } = &mut dialog.state {
                        *slot = Some(error);
                    }
                    self.tmdb_dialog = Some(dialog);
                }
            },
            Some(TmdbAction::SearchAgain) => {
                if let Some(key) = self.tmdb_key.clone() {
                    self.start_tmdb_search(dialog, key);
                } else {
                    dialog.state = TmdbState::RequestKey {
                        input: String::new(),
                        error: None,
                    };
                    self.tmdb_dialog = Some(dialog);
                }
            }
            Some(TmdbAction::Retry) => {
                let retry = match &dialog.state {
                    TmdbState::Failed { retry, .. } => *retry,
                    _ => TmdbRetry::Search,
                };
                match retry {
                    TmdbRetry::Search => {
                        if let Some(key) = self.tmdb_key.clone() {
                            self.start_tmdb_search(dialog, key);
                        } else {
                            dialog.state = TmdbState::RequestKey {
                                input: String::new(),
                                error: None,
                            };
                            self.tmdb_dialog = Some(dialog);
                        }
                    }
                    TmdbRetry::Details => {
                        let selected = dialog.selected;
                        match (self.tmdb_key.clone(), selected) {
                            (Some(key), Some(index)) => {
                                if let Some(result) = dialog.results.get(index).cloned() {
                                    self.start_tmdb_details(dialog, key, result);
                                } else {
                                    dialog.state = TmdbState::Choosing;
                                    self.tmdb_dialog = Some(dialog);
                                }
                            }
                            _ => {
                                dialog.state = TmdbState::Choosing;
                                self.tmdb_dialog = Some(dialog);
                            }
                        }
                    }
                }
            }
            Some(TmdbAction::ChooseAnother) => {
                dialog.state = TmdbState::Choosing;
                self.tmdb_dialog = Some(dialog);
            }
            Some(TmdbAction::ChangeKey) => {
                dialog.state = TmdbState::RequestKey {
                    input: self.tmdb_key.clone().unwrap_or_default(),
                    error: None,
                };
                self.tmdb_dialog = Some(dialog);
            }
            Some(TmdbAction::Fetch(index)) => {
                dialog.selected = Some(index);
                if let Some(result) = dialog.results.get(index).cloned() {
                    if let Some(key) = self.tmdb_key.clone() {
                        self.start_tmdb_details(dialog, key, result);
                    } else {
                        dialog.state = TmdbState::RequestKey {
                            input: String::new(),
                            error: None,
                        };
                        self.tmdb_dialog = Some(dialog);
                    }
                } else {
                    self.tmdb_dialog = Some(dialog);
                }
            }
            Some(TmdbAction::Save) => {
                let applied = match &dialog.state {
                    TmdbState::Comparing {
                        details, changes, ..
                    } => {
                        let fields: Vec<_> = changes
                            .iter()
                            .filter(|change| change.selected)
                            .map(|change| change.field)
                            .collect();
                        let count = fields.len();
                        self.collection.find_mut(&dialog.movie_id).map(|movie| {
                            details.apply(movie, fields.into_iter());
                            (movie.title.clone(), count)
                        })
                    }
                    _ => None,
                };
                self.tmdb_dialog = None;
                match applied {
                    Some((title, count)) if count > 0 => self.commit(format!(
                        "Updated “{title}” from TMDB ({count} field{}).",
                        if count == 1 { "" } else { "s" }
                    )),
                    Some(_) => {
                        self.status = Some(StatusMessage::info("No fields were selected."));
                    }
                    None => {
                        self.status = Some(StatusMessage::error(
                            "This movie is no longer in the collection.".to_string(),
                        ));
                    }
                }
            }
            None => {
                self.tmdb_dialog = Some(dialog);
            }
        }
    }

    /// Starts the Invelos workflow for a movie by looking up its barcode.
    fn open_invelos(&mut self, movie: &Movie) {
        self.omdb_dialog = None;
        self.tmdb_dialog = None;
        self.dvdcompare_dialog = None;

        let upc = normalize_upc(&movie.upc);
        if upc.is_empty() {
            self.invelos_dialog = None;
            self.status = Some(StatusMessage::error(
                "This movie has no barcode (UPC) to look up in Invelos.".to_string(),
            ));
            return;
        }

        let dialog = InvelosDialog {
            movie_id: movie.id.clone(),
            title: movie.title.clone(),
            upc,
            locality: movie.locality.clone(),
            variants: Vec::new(),
            selected: None,
            state: InvelosState::Failed {
                message: String::new(),
                retry: InvelosRetry::Variants,
            },
        };
        self.start_invelos_variants(dialog);
    }

    fn start_invelos_variants(&mut self, mut dialog: InvelosDialog) {
        let (sender, receiver) = mpsc::channel();
        let upc = dialog.upc.clone();
        std::thread::spawn(move || {
            let _ = sender.send(lookup::variants(&upc));
        });
        dialog.variants.clear();
        dialog.selected = None;
        dialog.state = InvelosState::FindingVariants(receiver);
        self.invelos_dialog = Some(dialog);
    }

    fn start_invelos_profile(&mut self, mut dialog: InvelosDialog, profile_id: String) {
        let (sender, receiver) = mpsc::channel();
        let worker_id = profile_id.clone();
        std::thread::spawn(move || {
            let _ = sender.send(lookup::fetch_profile(&worker_id));
        });
        dialog.state = InvelosState::Fetching {
            profile_id,
            receiver,
        };
        self.invelos_dialog = Some(dialog);
    }

    /// Polls the Invelos variant and profile requests, then draws the progress,
    /// locality chooser, comparison and error windows.
    fn invelos_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.invelos_dialog.take() else {
            return;
        };

        if let InvelosState::FindingVariants(receiver) = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("The Invelos lookup stopped unexpectedly.".to_string()));
                }
            }

            if let Some(result) = finished {
                match result {
                    Ok(variants) if variants.is_empty() => {
                        dialog.state = InvelosState::Failed {
                            message: format!(
                                "Invelos found no profile for barcode {}.",
                                dialog.upc
                            ),
                            retry: InvelosRetry::Variants,
                        };
                    }
                    Ok(variants) => {
                        let selected = preferred_variant(&variants, &dialog.locality);
                        dialog.variants = variants;
                        if dialog.variants.len() == 1 {
                            let profile_id = dialog.variants[0].profile_id.clone();
                            dialog.selected = Some(0);
                            self.start_invelos_profile(dialog, profile_id);
                            return;
                        }
                        dialog.selected = Some(selected);
                        dialog.state = InvelosState::Choosing;
                    }
                    Err(message) => {
                        dialog.state = InvelosState::Failed {
                            message,
                            retry: InvelosRetry::Variants,
                        };
                    }
                }
            }
        }

        if let InvelosState::Fetching { receiver, .. } = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("The Invelos lookup stopped unexpectedly.".to_string()));
                }
            }

            if let Some(result) = finished {
                match result {
                    Ok(details) => {
                        // Downloading a profile may also have saved covers.
                        self.covers = CoverIndex::scan(&covers::directory());
                        let Some(movie) = self.collection.find(&dialog.movie_id) else {
                            self.status = Some(StatusMessage::error(
                                "This movie is no longer in the collection.".to_string(),
                            ));
                            return;
                        };
                        let changes = details.changes(movie);
                        dialog.state = InvelosState::Comparing {
                            details: Box::new(details),
                            changes,
                        };
                    }
                    Err(message) => {
                        dialog.state = InvelosState::Failed {
                            message,
                            retry: InvelosRetry::Profile,
                        };
                    }
                }
            }
        }

        let mut open = true;
        let mut action = None;
        let lookup = format!("“{}”", dialog.title);

        match &mut dialog.state {
            InvelosState::FindingVariants(_) => {
                egui::Window::new("Update from Invelos")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(420.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!(
                                    "Looking up barcode {} in Invelos…",
                                    dialog.upc
                                ))
                                .size(12.5)
                                .color(TEXT),
                            );
                        });
                        ui.add_space(10.0);
                        if ui.button("Cancel").clicked() {
                            action = Some(InvelosAction::Close);
                        }
                    });
            }
            InvelosState::Choosing => {
                egui::Window::new("Update from Invelos")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(580.0)
                    .max_height(640.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Barcode {} has {} Invelos profiles. Choose the release to use for {lookup}.",
                                dialog.upc,
                                dialog.variants.len()
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(8.0);
                        let preferred = lookup::locality();
                        egui::ScrollArea::vertical()
                            .max_height(380.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, variant) in dialog.variants.iter().enumerate() {
                                    let selected = dialog.selected == Some(index);
                                    let label = variant_label(variant, &preferred);
                                    if ui
                                        .selectable_label(
                                            selected,
                                            RichText::new(label).size(12.5),
                                        )
                                        .clicked()
                                    {
                                        dialog.selected = Some(index);
                                    }
                                    ui.add_space(4.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let selected = dialog.selected;
                            if ui
                                .add_enabled(
                                    selected.is_some(),
                                    egui::Button::new(
                                        RichText::new("Load selected profile")
                                            .strong()
                                            .color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                                && let Some(index) = selected
                            {
                                action = Some(InvelosAction::Fetch(index));
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(InvelosAction::Close);
                            }
                        });
                    });
            }
            InvelosState::Fetching { profile_id, .. } => {
                egui::Window::new("Update from Invelos")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(440.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!(
                                    "Loading Invelos profile {profile_id} for {lookup}…"
                                ))
                                .size(12.5)
                                .color(TEXT),
                            );
                        });
                        ui.add_space(10.0);
                        if ui.button("Cancel").clicked() {
                            action = Some(InvelosAction::Close);
                        }
                    });
            }
            InvelosState::Comparing { details, changes } => {
                egui::Window::new("Update from Invelos")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(660.0)
                    .max_height(720.0)
                    .show(ctx, |ui| {
                        let locality = if details.locality.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", details.locality.trim())
                        };
                        ui.label(
                            RichText::new(format!(
                                "Choose which Invelos values to save for {lookup}. Profile {}{locality}.",
                                details.profile_id
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Select all").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = true;
                                }
                            }
                            if ui.button("Select none").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = false;
                                }
                            }
                        });
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(440.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, change) in changes.iter_mut().enumerate() {
                                    change_row(ui, index, change, "Invelos");
                                    ui.add_space(7.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let any_selected = changes.iter().any(|change| change.selected);
                            if ui
                                .add_enabled(
                                    any_selected,
                                    egui::Button::new(
                                        RichText::new("Update movie").strong().color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                action = Some(InvelosAction::Save);
                            }
                            if ui.button("Choose another profile").clicked() {
                                action = Some(InvelosAction::ChooseAnother);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(InvelosAction::Close);
                            }
                        });
                    });
            }
            InvelosState::Failed { message, retry } => {
                egui::Window::new("Update from Invelos")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(500.0)
                    .show(ctx, |ui| {
                        ui.label(RichText::new(message.as_str()).size(12.5).color(ERROR));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Check the barcode, the locality, or your connection.")
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Try again").clicked() {
                                action = Some(InvelosAction::Retry);
                            }
                            if matches!(*retry, InvelosRetry::Profile)
                                && ui.button("Choose another profile").clicked()
                            {
                                action = Some(InvelosAction::ChooseAnother);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(InvelosAction::Close);
                            }
                        });
                    });
            }
        }

        if !open && action.is_none() {
            action = Some(InvelosAction::Close);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            action = Some(InvelosAction::Close);
        }

        match action {
            Some(InvelosAction::Close) => {
                self.invelos_dialog = None;
            }
            Some(InvelosAction::Retry) => {
                let retry = match &dialog.state {
                    InvelosState::Failed { retry, .. } => *retry,
                    _ => InvelosRetry::Variants,
                };
                match retry {
                    InvelosRetry::Variants => self.start_invelos_variants(dialog),
                    InvelosRetry::Profile => {
                        let profile_id = dialog
                            .selected
                            .and_then(|index| dialog.variants.get(index))
                            .map(|variant| variant.profile_id.clone());
                        if let Some(profile_id) = profile_id {
                            self.start_invelos_profile(dialog, profile_id);
                        } else {
                            dialog.state = InvelosState::Choosing;
                            self.invelos_dialog = Some(dialog);
                        }
                    }
                }
            }
            Some(InvelosAction::ChooseAnother) => {
                dialog.state = InvelosState::Choosing;
                self.invelos_dialog = Some(dialog);
            }
            Some(InvelosAction::Fetch(index)) => {
                dialog.selected = Some(index);
                let profile_id = dialog
                    .variants
                    .get(index)
                    .map(|variant| variant.profile_id.clone());
                if let Some(profile_id) = profile_id {
                    self.start_invelos_profile(dialog, profile_id);
                } else {
                    self.invelos_dialog = Some(dialog);
                }
            }
            Some(InvelosAction::Save) => {
                let applied = match &dialog.state {
                    InvelosState::Comparing { details, changes } => {
                        let fields: Vec<_> = changes
                            .iter()
                            .filter(|change| change.selected)
                            .map(|change| change.field)
                            .collect();
                        let count = fields.len();
                        self.collection.find_mut(&dialog.movie_id).map(|movie| {
                            details.apply(movie, fields.into_iter());
                            (movie.title.clone(), count)
                        })
                    }
                    _ => None,
                };
                self.invelos_dialog = None;
                match applied {
                    Some((title, count)) if count > 0 => self.commit(format!(
                        "Updated “{title}” from Invelos ({count} field{}).",
                        if count == 1 { "" } else { "s" }
                    )),
                    Some(_) => {
                        self.status = Some(StatusMessage::info("No fields were selected."));
                    }
                    None => {
                        self.status = Some(StatusMessage::error(
                            "This movie is no longer in the collection.".to_string(),
                        ));
                    }
                }
            }
            None => {
                self.invelos_dialog = Some(dialog);
            }
        }
    }

    /// Starts the DVDCompare workflow by searching for the movie's title.
    fn open_dvdcompare(&mut self, movie: &Movie) {
        self.omdb_dialog = None;
        self.tmdb_dialog = None;
        self.invelos_dialog = None;

        let dialog = DvdCompareDialog {
            movie_id: movie.id.clone(),
            title: movie.title.clone(),
            year: movie.year.filter(|year| *year > 0),
            format: movie.format,
            query: movie.title.clone(),
            last_query: String::new(),
            results: Vec::new(),
            selected: None,
            film: None,
            release_selected: None,
            state: DvdCompareState::ChoosingFilm,
        };
        self.start_dvdcompare_search(dialog);
    }

    fn start_dvdcompare_search(&mut self, mut dialog: DvdCompareDialog) {
        let (sender, receiver) = mpsc::channel();
        let query = dialog.query.trim().to_string();
        dialog.last_query = query.clone();
        std::thread::spawn(move || {
            let _ = sender.send(dvdcompare::search(&query));
        });
        dialog.results.clear();
        dialog.selected = None;
        dialog.film = None;
        dialog.release_selected = None;
        dialog.state = DvdCompareState::Searching(receiver);
        self.dvdcompare_dialog = Some(dialog);
    }

    fn start_dvdcompare_film(
        &mut self,
        mut dialog: DvdCompareDialog,
        result: dvdcompare::SearchResult,
    ) {
        let (sender, receiver) = mpsc::channel();
        let film_id = result.film_id;
        std::thread::spawn(move || {
            let _ = sender.send(dvdcompare::film(film_id));
        });
        dialog.selected = dialog
            .results
            .iter()
            .position(|candidate| candidate.film_id == result.film_id);
        dialog.film = None;
        dialog.release_selected = None;
        dialog.state = DvdCompareState::FetchingFilm { result, receiver };
        self.dvdcompare_dialog = Some(dialog);
    }

    /// Polls the DVDCompare search and film requests, then draws the search,
    /// release chooser, comparison and error windows.
    fn dvdcompare_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dvdcompare_dialog.take() else {
            return;
        };

        if let DvdCompareState::Searching(receiver) = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err(
                        "The DVDCompare search stopped unexpectedly.".to_string()
                    ));
                }
            }

            if let Some(result) = finished {
                match result {
                    Ok(results) if results.is_empty() => {
                        dialog.state = DvdCompareState::Failed {
                            message: format!(
                                "DVDCompare found no matches for “{}”.",
                                dialog.last_query
                            ),
                            retry: DvdCompareRetry::Search,
                        };
                    }
                    Ok(results) => {
                        let preferred =
                            dvdcompare::preferred_result(&results, dialog.year, dialog.format);
                        dialog.selected = Some(preferred);
                        dialog.results = results;
                        dialog.state = DvdCompareState::ChoosingFilm;
                    }
                    Err(message) => {
                        dialog.state = DvdCompareState::Failed {
                            message,
                            retry: DvdCompareRetry::Search,
                        };
                    }
                }
            }
        }

        if let DvdCompareState::FetchingFilm { receiver, .. } = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err(
                        "The DVDCompare film lookup stopped unexpectedly.".to_string()
                    ));
                }
            }

            if let Some(result) = finished {
                match result {
                    Ok(film) if film.releases.is_empty() => {
                        dialog.state = DvdCompareState::Failed {
                            message: format!("DVDCompare has no releases for “{}”.", film.title),
                            retry: DvdCompareRetry::Film,
                        };
                    }
                    Ok(film) => {
                        dialog.release_selected = Some(0);
                        dialog.film = Some(film);
                        dialog.state = DvdCompareState::ChoosingRelease;
                    }
                    Err(message) => {
                        dialog.state = DvdCompareState::Failed {
                            message,
                            retry: DvdCompareRetry::Film,
                        };
                    }
                }
            }
        }

        let mut open = true;
        let mut action = None;
        let lookup = match dialog.year {
            Some(year) => format!("“{}” ({year})", dialog.title),
            None => format!("“{}”", dialog.title),
        };

        match &mut dialog.state {
            DvdCompareState::Searching(_) => {
                egui::Window::new("Update from DVDCompare")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(420.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!("Searching DVDCompare for {lookup}…"))
                                    .size(12.5)
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(10.0);
                        if ui.button("Cancel").clicked() {
                            action = Some(DvdCompareAction::Close);
                        }
                    });
            }
            DvdCompareState::ChoosingFilm => {
                egui::Window::new("Update from DVDCompare")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(680.0)
                    .max_height(720.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Choose the right DVDCompare film for {lookup}. The search returned {} result{}.",
                                dialog.results.len(),
                                if dialog.results.len() == 1 { "" } else { "s" }
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Search").size(12.0).color(MUTED));
                            let response = ui.add(
                                TextEdit::singleline(&mut dialog.query)
                                    .desired_width(400.0)
                                    .hint_text("Film title"),
                            );
                            let enter = response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter));
                            if ui.button("Search again").clicked() || enter {
                                action = Some(DvdCompareAction::SearchAgain);
                            }
                        });
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(430.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, result) in dialog.results.iter().enumerate() {
                                    let selected = dialog.selected == Some(index);
                                    if dvdcompare_result_row(ui, index, result, selected)
                                        .clicked()
                                    {
                                        dialog.selected = Some(index);
                                    }
                                    ui.add_space(6.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let selected = dialog.selected;
                            if ui
                                .add_enabled(
                                    selected.is_some(),
                                    egui::Button::new(
                                        RichText::new("Load selected film")
                                            .strong()
                                            .color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                                && let Some(index) = selected
                            {
                                action = Some(DvdCompareAction::FetchFilm(index));
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(DvdCompareAction::Close);
                            }
                        });
                    });
            }
            DvdCompareState::FetchingFilm { result, .. } => {
                egui::Window::new("Update from DVDCompare")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(440.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new(format!(
                                    "Loading “{}”{} from DVDCompare…",
                                    result.title,
                                    result
                                        .year
                                        .map(|year| format!(" ({year})"))
                                        .unwrap_or_default()
                                ))
                                .size(12.5)
                                .color(TEXT),
                            );
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Choose another film").clicked() {
                                action = Some(DvdCompareAction::ChooseAnotherFilm);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(DvdCompareAction::Close);
                            }
                        });
                    });
            }
            DvdCompareState::ChoosingRelease => {
                if let Some(film) = dialog.film.clone() {
                    egui::Window::new("Update from DVDCompare")
                        .open(&mut open)
                        .collapsible(false)
                        .resizable(true)
                        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                        .default_width(720.0)
                        .max_height(740.0)
                        .show(ctx, |ui| {
                            let year = film
                                .year
                                .map(|year| format!(" ({year})"))
                                .unwrap_or_default();
                            let fid = film
                                .film_id
                                .map(|id| format!(" · fid {id}"))
                                .unwrap_or_default();
                            let format = film
                                .format
                                .as_deref()
                                .map(|format| format!(" · {format}"))
                                .unwrap_or_default();
                            ui.label(
                                RichText::new(format!(
                                    "Choose the release to use for {lookup}. “{}{year}” has {} release{}{format}{fid}.",
                                    film.title.trim(),
                                    film.releases.len(),
                                    if film.releases.len() == 1 { "" } else { "s" }
                                ))
                                .size(12.5)
                                .color(MUTED),
                            );
                            ui.add_space(8.0);
                            egui::ScrollArea::vertical()
                                .max_height(470.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    for (index, release) in film.releases.iter().enumerate() {
                                        let selected = dialog.release_selected == Some(index);
                                        if dvdcompare_release_row(ui, index, release, selected)
                                            .clicked()
                                        {
                                            dialog.release_selected = Some(index);
                                        }
                                        ui.add_space(6.0);
                                    }
                                });

                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                let selected = dialog.release_selected;
                                if ui
                                    .add_enabled(
                                        selected.is_some(),
                                        egui::Button::new(
                                            RichText::new("Compare selected release")
                                                .strong()
                                                .color(INK),
                                        )
                                        .fill(ACCENT),
                                    )
                                    .clicked()
                                    && let Some(index) = selected
                                {
                                    action = Some(DvdCompareAction::CompareRelease(index));
                                }
                                if ui.button("Choose another film").clicked() {
                                    action = Some(DvdCompareAction::ChooseAnotherFilm);
                                }
                                if ui.button("Cancel").clicked() {
                                    action = Some(DvdCompareAction::Close);
                                }
                            });
                        });
                } else {
                    action = Some(DvdCompareAction::Close);
                }
            }
            DvdCompareState::Comparing {
                release, changes, ..
            } => {
                let film = dialog.film.as_ref();
                let film_title = film
                    .map(|film| film.title.trim().to_string())
                    .unwrap_or_else(|| dialog.title.clone());
                let film_year = film
                    .and_then(|film| film.year)
                    .map(|year| format!(" ({year})"))
                    .unwrap_or_default();
                let release_year = release
                    .year
                    .map(|year| format!(" [{year}]"))
                    .unwrap_or_default();

                egui::Window::new("Update from DVDCompare")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(680.0)
                    .max_height(740.0)
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Choose which DVDCompare values to save for {lookup}. “{film_title}”{film_year} · {}{release_year}.",
                                release.name
                            ))
                            .size(12.5)
                            .color(MUTED),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Select all").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = true;
                                }
                            }
                            if ui.button("Select none").clicked() {
                                for change in changes.iter_mut() {
                                    change.selected = false;
                                }
                            }
                        });
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(450.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, change) in changes.iter_mut().enumerate() {
                                    change_row(ui, index, change, "DVDCompare");
                                    ui.add_space(7.0);
                                }
                            });

                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let any_selected = changes.iter().any(|change| change.selected);
                            if ui
                                .add_enabled(
                                    any_selected,
                                    egui::Button::new(
                                        RichText::new("Update movie").strong().color(INK),
                                    )
                                    .fill(ACCENT),
                                )
                                .clicked()
                            {
                                action = Some(DvdCompareAction::Save);
                            }
                            if ui.button("Choose another release").clicked() {
                                action = Some(DvdCompareAction::ChooseAnotherRelease);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(DvdCompareAction::Close);
                            }
                        });
                    });
            }
            DvdCompareState::Failed { message, retry } => {
                egui::Window::new("Update from DVDCompare")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .default_width(520.0)
                    .show(ctx, |ui| {
                        ui.label(RichText::new(message.as_str()).size(12.5).color(ERROR));
                        ui.add_space(6.0);
                        if matches!(*retry, DvdCompareRetry::Search) {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Title").size(12.0).color(MUTED));
                                ui.add(
                                    TextEdit::singleline(&mut dialog.query)
                                        .desired_width(340.0)
                                        .hint_text("Film title"),
                                );
                            });
                            ui.add_space(6.0);
                        }
                        ui.label(
                            RichText::new("Check the title, the release list, or your connection.")
                                .size(11.0)
                                .color(MUTED),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Try again").clicked() {
                                action = Some(DvdCompareAction::Retry);
                            }
                            if !matches!(*retry, DvdCompareRetry::Search)
                                && !dialog.results.is_empty()
                                && ui.button("Choose another film").clicked()
                            {
                                action = Some(DvdCompareAction::ChooseAnotherFilm);
                            }
                            if ui.button("Cancel").clicked() {
                                action = Some(DvdCompareAction::Close);
                            }
                        });
                    });
            }
        }

        if !open && action.is_none() {
            action = Some(DvdCompareAction::Close);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            action = Some(DvdCompareAction::Close);
        }

        match action {
            Some(DvdCompareAction::Close) => {
                self.dvdcompare_dialog = None;
            }
            Some(DvdCompareAction::Retry) => {
                let retry = match &dialog.state {
                    DvdCompareState::Failed { retry, .. } => *retry,
                    _ => DvdCompareRetry::Search,
                };
                match retry {
                    DvdCompareRetry::Search => self.start_dvdcompare_search(dialog),
                    DvdCompareRetry::Film => {
                        let result = dialog
                            .selected
                            .and_then(|index| dialog.results.get(index))
                            .cloned();
                        if let Some(result) = result {
                            self.start_dvdcompare_film(dialog, result);
                        } else {
                            dialog.state = DvdCompareState::ChoosingFilm;
                            self.dvdcompare_dialog = Some(dialog);
                        }
                    }
                }
            }
            Some(DvdCompareAction::SearchAgain) => {
                self.start_dvdcompare_search(dialog);
            }
            Some(DvdCompareAction::ChooseAnotherFilm) => {
                dialog.state = DvdCompareState::ChoosingFilm;
                self.dvdcompare_dialog = Some(dialog);
            }
            Some(DvdCompareAction::ChooseAnotherRelease) => {
                dialog.state = DvdCompareState::ChoosingRelease;
                self.dvdcompare_dialog = Some(dialog);
            }
            Some(DvdCompareAction::FetchFilm(index)) => {
                dialog.selected = Some(index);
                let result = dialog.results.get(index).cloned();
                if let Some(result) = result {
                    self.start_dvdcompare_film(dialog, result);
                } else {
                    self.dvdcompare_dialog = Some(dialog);
                }
            }
            Some(DvdCompareAction::CompareRelease(index)) => {
                let release = dialog
                    .film
                    .as_ref()
                    .and_then(|film| film.releases.get(index))
                    .cloned();
                if let Some(release) = release {
                    let Some(movie) = self.collection.find(&dialog.movie_id) else {
                        self.status = Some(StatusMessage::error(
                            "This movie is no longer in the collection.".to_string(),
                        ));
                        return;
                    };
                    let cuts = dialog
                        .film
                        .as_ref()
                        .map(|film| film.cuts.clone())
                        .unwrap_or_default();
                    let details = dvdcompare::Details::from_release(&release, &cuts);
                    let changes = details.changes(movie);
                    dialog.release_selected = Some(index);
                    dialog.state = DvdCompareState::Comparing {
                        release,
                        details: Box::new(details),
                        changes,
                    };
                }
                self.dvdcompare_dialog = Some(dialog);
            }
            Some(DvdCompareAction::Save) => {
                let applied = match &dialog.state {
                    DvdCompareState::Comparing {
                        details, changes, ..
                    } => {
                        let fields: Vec<_> = changes
                            .iter()
                            .filter(|change| change.selected)
                            .map(|change| change.field)
                            .collect();
                        let count = fields.len();
                        self.collection.find_mut(&dialog.movie_id).map(|movie| {
                            details.apply(movie, fields.into_iter());
                            (movie.title.clone(), count)
                        })
                    }
                    _ => None,
                };
                self.dvdcompare_dialog = None;
                match applied {
                    Some((title, count)) if count > 0 => self.commit(format!(
                        "Updated “{title}” from DVDCompare ({count} field{}).",
                        if count == 1 { "" } else { "s" }
                    )),
                    Some(_) => {
                        self.status = Some(StatusMessage::info("No fields were selected."));
                    }
                    None => {
                        self.status = Some(StatusMessage::error(
                            "This movie is no longer in the collection.".to_string(),
                        ));
                    }
                }
            }
            None => {
                self.dvdcompare_dialog = Some(dialog);
            }
        }
    }

    fn cover_viewer_ui(&mut self, ctx: &egui::Context) {
        let Some(path) = self.cover_viewer.clone() else {
            return;
        };

        let mut open = true;
        let mut close = false;
        egui::Window::new("Cover")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                if let Some(texture) = self.cover_textures.get(ctx, &path) {
                    let natural = texture.size_vec2();
                    let scale = (620.0 / natural.x).min(720.0 / natural.y);
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(
                            texture.id(),
                            natural * scale,
                        ))
                        .corner_radius(CornerRadius::same(6)),
                    );
                } else {
                    ui.label(
                        RichText::new("Could not read this cover image.")
                            .size(12.5)
                            .color(ERROR),
                    );
                }

                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });

        if !open || close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.cover_viewer = None;
        }
    }

    fn start_update_check(&mut self) {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(update::check_and_install());
        });
        self.update_dialog = Some(UpdateDialog {
            state: UpdateState::Checking(receiver),
        });
    }

    fn update_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.update_dialog.take() else {
            return;
        };

        if let UpdateState::Checking(receiver) = &dialog.state {
            let mut finished = None;
            match receiver.try_recv() {
                Ok(result) => finished = Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("The update check stopped unexpectedly.".to_string()));
                }
            }
            if let Some(result) = finished {
                dialog.state = UpdateState::Finished(result);
            }
        }

        let mut open = true;
        let mut close = false;
        egui::Window::new("Update KinoCatalog")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                match &dialog.state {
                    UpdateState::Checking(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                RichText::new("Checking GitHub for a new release…")
                                    .size(13.0)
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "If a newer version is available, it will be downloaded and installed.",
                            )
                            .size(11.5)
                            .color(MUTED),
                        );
                    }
                    UpdateState::Finished(Ok(update::Outcome::UpToDate { current, latest })) => {
                        ui.label(
                            RichText::new("KinoCatalog is up to date.")
                                .size(14.0)
                                .strong()
                                .color(SUCCESS),
                        );
                        ui.add_space(4.0);
                        let message = if current == latest {
                            format!("Version {current} is the latest release.")
                        } else {
                            format!("You are running {current}; the latest release is {latest}.")
                        };
                        ui.label(RichText::new(message).size(12.0).color(MUTED));
                    }
                    UpdateState::Finished(Ok(update::Outcome::Installed {
                        previous,
                        installed,
                    })) => {
                        ui.label(
                            RichText::new(format!("KinoCatalog {installed} has been installed."))
                                .size(14.0)
                                .strong()
                                .color(SUCCESS),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "Updated from {previous}. Restart the app to use the new version."
                            ))
                            .size(12.0)
                            .color(MUTED),
                        );
                    }
                    UpdateState::Finished(Err(message)) => {
                        ui.label(
                            RichText::new("Could not update KinoCatalog.")
                                .size(14.0)
                                .strong()
                                .color(ERROR),
                        );
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(RichText::new(message).size(12.0).color(TEXT)).wrap(),
                        );
                    }
                }

                ui.add_space(12.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });

        if open && !close {
            self.update_dialog = Some(dialog);
        }
    }

    fn about_ui(&mut self, ctx: &egui::Context) {
        if !self.show_about {
            return;
        }

        let mut open = true;
        let mut close = false;
        egui::Window::new("About")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(220.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("KinoCatalog by nowme")
                            .size(16.0)
                            .strong()
                            .color(TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                            .size(12.5)
                            .color(MUTED),
                    );
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });

        if !open || close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.show_about = false;
        }
    }

    fn empty_details(&mut self, ui: &mut Ui) {
        let mut add = false;
        let mut import = false;
        let empty = self.collection.movies.is_empty();

        ui.vertical_centered(|ui| {
            ui.add_space(130.0);
            ui.label(
                RichText::new(if empty {
                    "Your collection is empty"
                } else {
                    "Nothing selected"
                })
                .size(25.0)
                .strong()
                .color(TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(if empty {
                    "Import your DVD Profiler export, or add a movie by hand."
                } else {
                    "Choose a title from the list on the left."
                })
                .size(13.5)
                .color(MUTED),
            );

            if empty {
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Import Collection.xml").strong().color(INK),
                            )
                            .fill(ACCENT),
                        )
                        .clicked()
                    {
                        import = true;
                    }
                    if ui.button("Add a movie").clicked() {
                        add = true;
                    }
                });
            }
        });

        if add {
            self.open_add();
        }
        if import {
            self.import_dialog = Some(ImportDialog::default());
        }
    }

    fn footer(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(TOP_BG)
                    .inner_margin(Margin::symmetric(16, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    match &self.status {
                        Some(status) => {
                            let color = match status.kind {
                                StatusKind::Info => MUTED,
                                StatusKind::Success => SUCCESS,
                                StatusKind::Error => ERROR,
                            };
                            ui.label(RichText::new(&status.text).size(11.0).color(color));
                        }
                        None => {
                            ui.label(
                                RichText::new(format!(
                                    "{} titles · ready",
                                    self.collection.movies.len()
                                ))
                                .size(11.0)
                                .color(MUTED),
                            );
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(storage::display_path(&self.data_path))
                                .size(10.5)
                                .color(MUTED),
                        )
                        .on_hover_text(format!(
                            "Collection database: {}",
                            self.data_path.display()
                        ));
                    });
                });
            });
    }

    fn editor_ui(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.editor.take() else {
            return;
        };

        // A finished lookup may have downloaded covers, so rebuild the index.
        if poll_lookup(&mut editor, ctx) {
            self.covers = CoverIndex::scan(&covers::directory());
        }

        let mut open = true;
        let mut action = None;
        let mut lookup_requested = false;
        let mut profile_requested = None;
        let title = if editor.editing_id.is_some() {
            "Edit movie"
        } else {
            "Add movie"
        };

        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(680.0)
            .max_height(760.0)
            .show(ctx, |ui| {
                draw_editor(
                    ui,
                    &mut editor,
                    &mut action,
                    &mut lookup_requested,
                    &mut profile_requested,
                );
            });

        if let Some(profile_id) = profile_requested {
            start_profile_fetch(&mut editor, profile_id);
            ctx.request_repaint();
        } else if lookup_requested {
            start_lookup(&mut editor);
            ctx.request_repaint();
        }

        if !open {
            action = Some(EditorAction::Cancel);
        }

        match action {
            Some(EditorAction::Save) => {
                let mut movie = match &editor.editing_id {
                    Some(id) => match self.collection.find(id).cloned() {
                        Some(movie) => movie,
                        None => {
                            editor.form.error =
                                Some("This movie is no longer in the collection.".to_string());
                            self.editor = Some(editor);
                            return;
                        }
                    },
                    None => Movie {
                        id: new_manual_id(),
                        ..Default::default()
                    },
                };

                match editor.form.apply_to(&mut movie) {
                    Ok(()) => {
                        let existed = self.collection.find(&movie.id).is_some();
                        let title = movie.title.clone();
                        self.collection.upsert(movie.clone());
                        self.collection.sort();
                        self.selected = Some(movie.id.clone());
                        self.show_full_cast = false;
                        self.commit(if existed {
                            format!("Saved “{title}”.")
                        } else {
                            format!("Added “{title}”.")
                        });
                    }
                    Err(error) => {
                        editor.form.error = Some(error);
                        self.editor = Some(editor);
                    }
                }
            }
            Some(EditorAction::Cancel) => {
                self.status = Some(StatusMessage::info("Edit cancelled."));
            }
            None => {
                self.editor = Some(editor);
            }
        }
    }

    fn import_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.import_dialog.take() else {
            return;
        };

        let mut open = true;
        let mut browse = false;
        let mut import = false;
        let mut cancel = false;

        egui::Window::new("Import collection")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(580.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Import a DVD Profiler XML export. Existing titles are updated and your notes are kept.",
                    )
                    .size(12.5)
                    .color(MUTED),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - 94.0).max(220.0);
                    ui.add(
                        TextEdit::singleline(&mut dialog.path)
                            .id(egui::Id::unique("import_path"))
                            .desired_width(width)
                            .hint_text("/path/to/Collection.xml"),
                    );
                    if ui.button("Browse…").clicked() {
                        browse = true;
                    }
                });

                if let Some(error) = &dialog.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(error).size(12.0).color(ERROR));
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Import").strong().color(INK),
                            )
                            .fill(ACCENT),
                        )
                        .clicked()
                    {
                        import = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                ui.add_space(5.0);
                ui.label(
                    RichText::new("Tip: you can also drop an XML file onto the window.")
                        .size(10.5)
                        .color(MUTED),
                );
            });

        if browse {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("XML collection", &["xml"])
                .set_title("Select a DVD Profiler collection export")
                .pick_file()
            {
                dialog.path = path.display().to_string();
                dialog.error = None;
            }
            self.import_dialog = Some(dialog);
            return;
        }

        if import {
            if dialog.path.trim().is_empty() {
                dialog.error = Some("Enter a file path, or use Browse….".to_string());
                self.import_dialog = Some(dialog);
                return;
            }

            let path = PathBuf::from(dialog.path.trim());
            match self.import_file(&path) {
                Ok(message) => {
                    self.import_dialog = None;
                    self.commit(message);
                }
                Err(error) => {
                    dialog.error = Some(error);
                    self.import_dialog = Some(dialog);
                }
            }
            return;
        }

        if cancel || !open {
            self.import_dialog = None;
        } else {
            self.import_dialog = Some(dialog);
        }
    }

    fn delete_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.pending_delete.clone() else {
            return;
        };
        let Some(movie) = self.collection.find(&id) else {
            self.pending_delete = None;
            return;
        };

        let title = movie.title.clone();
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;

        egui::Window::new("Delete movie")
            .open(&mut open)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(400.0)
            .show(ctx, |ui| {
                ui.label(format!("Remove “{title}” from your collection?"));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("This cannot be undone.")
                        .size(11.0)
                        .color(MUTED),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Delete").strong().color(ERROR))
                                .fill(Color32::from_rgb(54, 32, 36)),
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if confirm {
            self.collection.remove(&id);
            if self.selected.as_deref() == Some(id.as_str()) {
                self.selected = None;
            }
            self.pending_delete = None;
            self.commit(format!("Removed “{title}”."));
        } else if cancel || !open {
            self.pending_delete = None;
        }
    }
}

impl eframe::App for KinoCatalogApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.handle_dropped_files(&ctx);

        if self.list_dirty {
            self.refresh_filter();
            self.list_dirty = false;
        }

        self.top_bar(ui);
        self.footer(ui);
        self.list_panel(ui);
        self.details_panel(ui);

        self.editor_ui(&ctx);
        self.omdb_ui(&ctx);
        self.tmdb_ui(&ctx);
        self.invelos_ui(&ctx);
        self.dvdcompare_ui(&ctx);
        self.import_ui(&ctx);
        self.delete_ui(&ctx);
        self.cover_viewer_ui(&ctx);
        self.update_ui(&ctx);
        self.about_ui(&ctx);
    }
}

enum FinishedLookup {
    Variants {
        barcode: String,
        result: Result<Vec<lookup::ProfileSummary>, String>,
    },
    Profile(Result<lookup::MovieDetails, String>),
    Disconnected,
}

/// Checks a running lookup and fills the form when it finishes. Returns true
/// when the cover folder should be rescanned.
fn poll_lookup(editor: &mut Editor, ctx: &egui::Context) -> bool {
    let finished = match &editor.lookup {
        LookupState::FindingVariants { barcode, receiver } => match receiver.try_recv() {
            Ok(result) => FinishedLookup::Variants {
                barcode: barcode.clone(),
                result,
            },
            Err(TryRecvError::Empty) => {
                // The lookup thread has no way to wake the UI, so poll again soon.
                ctx.request_repaint_after(Duration::from_millis(150));
                return false;
            }
            Err(TryRecvError::Disconnected) => FinishedLookup::Disconnected,
        },
        LookupState::Fetching(receiver) => match receiver.try_recv() {
            Ok(result) => FinishedLookup::Profile(result),
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(150));
                return false;
            }
            Err(TryRecvError::Disconnected) => FinishedLookup::Disconnected,
        },
        _ => return false,
    };

    match finished {
        FinishedLookup::Variants {
            barcode,
            result: Ok(variants),
        } => {
            if variants.len() > 1 {
                let preferred = lookup::locality();
                let selected = variants
                    .iter()
                    .position(|variant| variant.locality == preferred)
                    .unwrap_or(0);
                editor.lookup = LookupState::Choosing { variants, selected };
            } else if let Some(profile) = variants.first() {
                start_profile_fetch(editor, profile.profile_id.clone());
            } else {
                // Older barcode-only lookups still work when the variant list
                // is empty, so preserve the direct locality lookup as a
                // fallback.
                start_locality_fetch(editor, &barcode);
            }
            false
        }
        FinishedLookup::Variants {
            result: Err(error), ..
        } => {
            editor.lookup = LookupState::Error(error);
            false
        }
        FinishedLookup::Profile(Ok(details)) => {
            let message = if details.front_cover || details.back_cover {
                format!(
                    "Filled from Invelos profile {}. Cover images were saved.",
                    details.profile_id
                )
            } else {
                format!(
                    "Filled from Invelos profile {}. No cover images were available.",
                    details.profile_id
                )
            };
            fill_from_lookup(&mut editor.form, &details);
            editor.barcode = details.upc;
            editor.lookup = LookupState::Done(message);
            true
        }
        FinishedLookup::Profile(Err(error)) => {
            editor.lookup = LookupState::Error(error);
            false
        }
        FinishedLookup::Disconnected => {
            editor.lookup =
                LookupState::Error("The lookup stopped before returning a result.".to_string());
            false
        }
    }
}

fn start_lookup(editor: &mut Editor) {
    if editor.lookup.busy() {
        return;
    }

    let barcode = editor.barcode.trim().to_string();
    if barcode.is_empty() {
        editor.lookup = LookupState::Error("Scan or type a barcode first.".to_string());
        return;
    }

    let (sender, receiver) = mpsc::channel();
    let requested = barcode.clone();
    std::thread::spawn(move || {
        let _ = sender.send(lookup::variants(&requested));
    });
    editor.lookup = LookupState::FindingVariants { barcode, receiver };
}

fn start_locality_fetch(editor: &mut Editor, barcode: &str) {
    let barcode = barcode.to_string();
    let locality = lookup::locality();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(lookup::fetch(&barcode, &locality));
    });
    editor.lookup = LookupState::Fetching(receiver);
}

fn start_profile_fetch(editor: &mut Editor, profile_id: String) {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(lookup::fetch_profile(&profile_id));
    });
    editor.lookup = LookupState::Fetching(receiver);
}

fn fill_from_lookup(form: &mut MovieForm, details: &lookup::MovieDetails) {
    form.title = details.title.clone();
    form.original_title = details.original_title.clone();
    form.sort_title = details.sort_title.clone();
    form.format = details.format;
    form.year = details
        .year
        .map(|year| year.to_string())
        .unwrap_or_default();
    form.runtime = details
        .runtime_minutes
        .map(|minutes| minutes.to_string())
        .unwrap_or_default();
    form.rating = details.rating.clone();
    form.directors = details.directors.join("\n");
    form.cast = details
        .cast
        .iter()
        .map(|(name, role)| {
            if role.is_empty() {
                name.clone()
            } else {
                format!("{name} as {role}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    form.overview = details.synopsis.clone();
    form.upc = details.upc.clone();
    form.locality = details.locality.clone();
    form.error = None;
}

fn variant_label(variant: &lookup::ProfileSummary, preferred: &str) -> String {
    let locality = match lookup::locality_name(&variant.locality) {
        Some(name) => format!("{name} ({})", variant.locality),
        None => format!("Locality {}", variant.locality),
    };
    let mut label = format!("{locality} — {}", variant.title);
    if !variant.edition.is_empty() {
        label.push_str(" — ");
        label.push_str(&variant.edition);
    }
    if variant.locality == preferred {
        label.push_str("  · preferred");
    }
    label
}

/// Picks the variant matching the movie's own locality first, then the
/// configured preferred locality, then the first profile.
fn preferred_variant(variants: &[lookup::ProfileSummary], movie_locality: &str) -> usize {
    let movie_locality = movie_locality.trim();
    if !movie_locality.is_empty()
        && let Some(index) = variants.iter().position(|variant| {
            variant.locality == movie_locality
                || lookup::locality_name(&variant.locality)
                    .is_some_and(|name| name.eq_ignore_ascii_case(movie_locality))
        })
    {
        return index;
    }

    let preferred = lookup::locality();
    variants
        .iter()
        .position(|variant| variant.locality == preferred)
        .unwrap_or(0)
}

fn draw_lookup_row(
    ui: &mut Ui,
    editor: &mut Editor,
    lookup_requested: &mut bool,
    profile_requested: &mut Option<String>,
) {
    let running = editor.lookup.busy();
    let mut submit = false;

    ui.horizontal(|ui| {
        field_label(ui, "Barcode");
        let response = ui.add_enabled(
            !running,
            TextEdit::singleline(&mut editor.barcode)
                .id(egui::Id::unique("barcode_field"))
                .desired_width(220.0)
                .hint_text("Scan or type a UPC"),
        );
        if editor.focus_barcode {
            response.request_focus();
            editor.focus_barcode = false;
        }
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        if ui
            .add_enabled(!running, egui::Button::new("Look up"))
            .clicked()
        {
            submit = true;
        }
        if running {
            ui.spinner();
            ui.label(RichText::new("Searching Invelos…").size(11.5).color(MUTED));
        }
    });

    match &mut editor.lookup {
        LookupState::Idle => {
            ui.add(
                Label::new(
                    RichText::new(
                        "Scan a barcode or type a UPC and press Enter to fill the details from Invelos.",
                    )
                    .size(11.0)
                    .color(MUTED),
                )
                .wrap(),
            );
        }
        LookupState::FindingVariants { .. } | LookupState::Fetching(_) => {}
        LookupState::Choosing {
            variants, selected, ..
        } => {
            ui.add_space(4.0);
            ui.add(
                Label::new(
                    RichText::new(
                        "This barcode has profiles in several localities. Choose the correct one:",
                    )
                    .size(11.5)
                    .color(MUTED),
                )
                .wrap(),
            );
            ui.add_space(2.0);
            let preferred = lookup::locality();
            for (index, variant) in variants.iter().enumerate() {
                ui.radio_value(
                    selected,
                    index,
                    RichText::new(variant_label(variant, &preferred)).size(12.0),
                );
            }
            ui.add_space(4.0);
            if ui.button("Load selected profile").clicked()
                && let Some(variant) = variants.get(*selected)
            {
                *profile_requested = Some(variant.profile_id.clone());
            }
        }
        LookupState::Error(error) => {
            ui.add(Label::new(RichText::new(error.as_str()).size(11.5).color(ERROR)).wrap());
        }
        LookupState::Done(message) => {
            ui.add(Label::new(RichText::new(message.as_str()).size(11.5).color(SUCCESS)).wrap());
        }
    }

    if submit {
        *lookup_requested = true;
    }
}

fn draw_editor(
    ui: &mut Ui,
    editor: &mut Editor,
    action: &mut Option<EditorAction>,
    lookup_requested: &mut bool,
    profile_requested: &mut Option<String>,
) {
    if editor.editing_id.is_none() {
        draw_lookup_row(ui, editor, lookup_requested, profile_requested);
        ui.add_space(8.0);
    }

    ui.label(
        RichText::new("Only the title is required. Empty fields are hidden in the details view.")
            .size(11.5)
            .color(MUTED),
    );
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .id_salt("editor_form")
        .max_height(540.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let form = &mut editor.form;
            egui::Grid::new("movie_form")
                .num_columns(2)
                .spacing([14.0, 9.0])
                .min_col_width(105.0)
                .show(ui, |ui| {
                    field_label(ui, "Title");
                    ui.add(
                        TextEdit::singleline(&mut form.title)
                            .desired_width(460.0)
                            .hint_text("Required"),
                    );
                    ui.end_row();

                    field_label(ui, "Original title");
                    ui.add(
                        TextEdit::singleline(&mut form.original_title)
                            .desired_width(460.0)
                            .hint_text("Optional"),
                    );
                    ui.end_row();

                    field_label(ui, "Alternative titles");
                    ui.add(
                        TextEdit::multiline(&mut form.alternative_titles)
                            .desired_width(460.0)
                            .desired_rows(2)
                            .hint_text("One per line"),
                    );
                    ui.end_row();

                    field_label(ui, "Format");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("format_combo")
                            .selected_text(form.format.label())
                            .width(160.0)
                            .show_ui(ui, |ui| {
                                for format in DiscFormat::ALL {
                                    ui.selectable_value(&mut form.format, format, format.label());
                                }
                            });
                        ui.add_space(4.0);
                        ui.checkbox(&mut form.is_3d, "3D")
                            .on_hover_text("Stereoscopic Blu-ray or anaglyph DVD");
                    });
                    ui.end_row();

                    field_label(ui, "Year");
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut form.year)
                                .desired_width(74.0)
                                .hint_text("1999"),
                        );
                        ui.label(RichText::new("Runtime (min)").size(11.5).color(MUTED));
                        ui.add(
                            TextEdit::singleline(&mut form.runtime)
                                .desired_width(74.0)
                                .hint_text("120"),
                        );
                    });
                    ui.end_row();

                    field_label(ui, "Rating");
                    ui.add(
                        TextEdit::singleline(&mut form.rating)
                            .desired_width(220.0)
                            .hint_text("e.g. PG-13, 15, R"),
                    );
                    ui.end_row();

                    field_label(ui, "Genres");
                    ui.add(
                        TextEdit::singleline(&mut form.genres)
                            .desired_width(460.0)
                            .hint_text("Comma separated, e.g. Drama, Crime"),
                    );
                    ui.end_row();

                    field_label(
                        ui,
                        if form.format == DiscFormat::BluRay {
                            "Area code"
                        } else {
                            "Region"
                        },
                    );
                    ui.add(
                        TextEdit::singleline(&mut form.regions)
                            .desired_width(220.0)
                            .hint_text("Comma separated, e.g. 1, 2 or A, B"),
                    );
                    ui.end_row();

                    field_label(ui, "Studios");
                    ui.add(
                        TextEdit::multiline(&mut form.studios)
                            .desired_width(460.0)
                            .desired_rows(2)
                            .hint_text("One per line"),
                    );
                    ui.end_row();

                    field_label(ui, "Directors");
                    ui.add(
                        TextEdit::multiline(&mut form.directors)
                            .desired_width(460.0)
                            .desired_rows(2)
                            .hint_text("One per line"),
                    );
                    ui.end_row();

                    field_label(ui, "Cast");
                    ui.add(
                        TextEdit::multiline(&mut form.cast)
                            .desired_width(460.0)
                            .desired_rows(5)
                            .hint_text("One per line: Name as Role"),
                    );
                    ui.end_row();

                    field_label(ui, "Synopsis");
                    ui.add(
                        TextEdit::multiline(&mut form.overview)
                            .desired_width(460.0)
                            .desired_rows(5),
                    );
                    ui.end_row();

                    field_label(ui, "UPC");
                    ui.add(TextEdit::singleline(&mut form.upc).desired_width(220.0));
                    ui.end_row();

                    field_label(ui, "Locality");
                    ui.add(
                        TextEdit::singleline(&mut form.locality)
                            .desired_width(220.0)
                            .hint_text("e.g. Finland"),
                    );
                    ui.end_row();

                    field_label(ui, "Collection #");
                    ui.add(TextEdit::singleline(&mut form.collection_number).desired_width(220.0));
                    ui.end_row();

                    field_label(ui, "Case type");
                    ui.add(
                        TextEdit::singleline(&mut form.case_type)
                            .desired_width(220.0)
                            .hint_text("e.g. Keep Case, Digipak"),
                    );
                    ui.end_row();

                    field_label(ui, "Cuts");
                    ui.add(
                        TextEdit::multiline(&mut form.cuts)
                            .desired_width(360.0)
                            .desired_rows(3)
                            .hint_text("e.g. No cuts (91:12 NTSC)"),
                    );
                    ui.end_row();

                    field_label(ui, "Purchase date");
                    ui.add(
                        TextEdit::singleline(&mut form.purchase_date)
                            .desired_width(220.0)
                            .hint_text("YYYY-MM-DD"),
                    );
                    ui.end_row();

                    field_label(ui, "Price");
                    ui.add(
                        TextEdit::singleline(&mut form.purchase_price)
                            .desired_width(220.0)
                            .hint_text("e.g. 12.99 USD"),
                    );
                    ui.end_row();

                    field_label(ui, "Shelf location");
                    ui.add(
                        TextEdit::singleline(&mut form.location)
                            .desired_width(220.0)
                            .hint_text("Where the disc is stored"),
                    );
                    ui.end_row();

                    field_label(ui, "Notes");
                    ui.add(
                        TextEdit::multiline(&mut form.notes)
                            .desired_width(460.0)
                            .desired_rows(3),
                    );
                    ui.end_row();
                });
        });

    if let Some(error) = &editor.form.error {
        ui.add_space(8.0);
        ui.label(RichText::new(error).size(12.0).color(ERROR));
    }

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(RichText::new("Save").strong().color(INK)).fill(ACCENT))
            .clicked()
        {
            *action = Some(EditorAction::Save);
        }
        if ui.button("Cancel").clicked() {
            *action = Some(EditorAction::Cancel);
        }
    });
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = PANEL_BG;
        style.visuals.window_fill = CARD_BG;
        style.visuals.faint_bg_color = Color32::from_rgb(30, 34, 42);
        style.visuals.extreme_bg_color = Color32::from_rgb(18, 20, 25);
        style.visuals.text_edit_bg_color = Some(Color32::from_rgb(24, 27, 34));
        style.visuals.selection.bg_fill = ACCENT_BG;
        style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        style.visuals.hyperlink_color = ACCENT;
        style.visuals.error_fg_color = ERROR;
        style.visuals.window_stroke = Stroke::new(1.0, BORDER);
        style.visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);
        style.visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
        style.visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
        style.visuals.widgets.active.corner_radius = CornerRadius::same(6);
        style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(38, 43, 53);
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.menu_margin = Margin::same(6);
        style
            .text_styles
            .insert(TextStyle::Body, FontId::new(14.5, FontFamily::Proportional));
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(22.0, FontFamily::Proportional),
        );
    });
}

fn movie_row(ui: &mut Ui, movie: &Movie, selected: bool) -> egui::Response {
    let fill = if selected {
        SELECTED_BG
    } else {
        Color32::TRANSPARENT
    };

    let mut response = egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 1.0;

            ui.horizontal(|ui| {
                format_badge(ui, movie);
                let mut title = RichText::new(&movie.title).size(13.0);
                if selected {
                    title = title.strong();
                }
                ui.add(Label::new(title).truncate());
            });

            let subtitle = movie.list_subtitle();
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).size(10.5).color(MUTED));
            }
        })
        .response
        .interact(Sense::click());

    if response.hovered() && !selected {
        ui.painter().rect_stroke(
            response.rect,
            CornerRadius::same(6),
            Stroke::new(1.0, BORDER),
            StrokeKind::Inside,
        );
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }

    response
}

fn format_badge(ui: &mut Ui, movie: &Movie) {
    let (foreground, background) = match movie.format {
        DiscFormat::Dvd => (DVD_FG, DVD_BG),
        DiscFormat::BluRay => (BD_FG, BD_BG),
        DiscFormat::UltraHd => (UHD_FG, UHD_BG),
    };

    egui::Frame::new()
        .fill(background)
        .corner_radius(CornerRadius::same(3))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.set_min_width(28.0);
            ui.label(
                RichText::new(movie.format_badge_label())
                    .size(10.0)
                    .strong()
                    .color(foreground),
            );
        })
        .response
        .on_hover_text(movie.format_label());
}

/// Toolbar button with three stacked lines that opens the action menu.
fn hamburger_button(ui: &mut Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(40.0, 30.0), Sense::click());
    let fill = if response.hovered() {
        Color32::from_rgb(45, 51, 63)
    } else {
        CARD_BG
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(7),
        fill,
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );

    let center = rect.center();
    for offset in [-4.5, 0.0, 4.5] {
        painter.line_segment(
            [
                egui::pos2(center.x - 8.0, center.y + offset),
                egui::pos2(center.x + 8.0, center.y + offset),
            ],
            Stroke::new(1.7, TEXT),
        );
    }

    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Menu")
}

/// One selectable search result in the TMDB chooser.
fn dvdcompare_result_row(
    ui: &mut Ui,
    index: usize,
    result: &dvdcompare::SearchResult,
    selected: bool,
) -> egui::Response {
    let fill = if selected { SELECTED_BG } else { CARD_BG };
    let stroke = if selected {
        Stroke::new(1.5, ACCENT)
    } else {
        Stroke::new(1.0, BORDER)
    };

    let content = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    Label::new(RichText::new(&result.title).size(13.5).strong().color(TEXT)).wrap(),
                );
                if let Some(year) = result.year {
                    ui.label(RichText::new(format!("({year})")).size(12.0).color(MUTED));
                }
                chip(ui, result.disc_format.as_deref().unwrap_or("DVD"));
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new(format!("DVDCompare film id {}", result.film_id))
                    .size(11.0)
                    .color(MUTED),
            );
        })
        .response;

    ui.interact(
        content.rect,
        ui.make_persistent_id(("dvdcompare_result", index)),
        Sense::click(),
    )
    .on_hover_cursor(CursorIcon::PointingHand)
}

fn dvdcompare_release_row(
    ui: &mut Ui,
    index: usize,
    release: &dvdcompare::Release,
    selected: bool,
) -> egui::Response {
    let fill = if selected { SELECTED_BG } else { CARD_BG };
    let stroke = if selected {
        Stroke::new(1.5, ACCENT)
    } else {
        Stroke::new(1.0, BORDER)
    };

    let content = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                Label::new(
                    RichText::new(release.name.trim())
                        .size(13.5)
                        .strong()
                        .color(TEXT),
                )
                .wrap(),
            );
            let summary = dvdcompare_release_summary(release);
            if !summary.is_empty() {
                ui.add_space(3.0);
                ui.add(Label::new(RichText::new(summary).size(11.5).color(MUTED)).wrap());
            }
        })
        .response;

    ui.interact(
        content.rect,
        ui.make_persistent_id(("dvdcompare_release", index)),
        Sense::click(),
    )
    .on_hover_cursor(CursorIcon::PointingHand)
}

fn dvdcompare_release_summary(release: &dvdcompare::Release) -> String {
    let mut parts = Vec::new();
    if let Some(year) = release.year {
        parts.push(year.to_string());
    }
    let discs = release.discs.len();
    if discs > 0 {
        parts.push(format!("{discs} disc{}", if discs == 1 { "" } else { "s" }));
    }
    let film_discs: Vec<String> = release
        .discs
        .iter()
        .filter(|disc| disc.is_film)
        .map(|disc| disc.number.to_string())
        .collect();
    if !film_discs.is_empty() {
        parts.push(format!("film on disc {}", film_discs.join(", ")));
    }
    if let Some(format) = release
        .discs
        .iter()
        .map(|disc| disc.format.trim())
        .find(|format| !format.is_empty())
    {
        parts.push(format.to_string());
    }
    let extras: usize = release.discs.iter().map(|disc| disc.features.len()).sum();
    if extras > 0 {
        parts.push(format!(
            "{extras} extra{}",
            if extras == 1 { "" } else { "s" }
        ));
    }
    for label in ["TV System", "Aspect Ratio", "Case type", "UPC Number"] {
        let Some(value) = release
            .specs
            .get(label)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("none"))
        else {
            continue;
        };
        if label == "UPC Number" {
            parts.push(format!("UPC {value}"));
        } else {
            parts.push(format!("{label}: {value}"));
        }
    }
    parts.join(" · ")
}

fn tmdb_result_row(
    ui: &mut Ui,
    index: usize,
    result: &tmdb::SearchResult,
    selected: bool,
) -> egui::Response {
    let fill = if selected { SELECTED_BG } else { CARD_BG };
    let stroke = if selected {
        Stroke::new(1.5, ACCENT)
    } else {
        Stroke::new(1.0, BORDER)
    };

    let content = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&result.title).size(13.5).strong().color(TEXT));
                if let Some(year) = result.year {
                    ui.label(RichText::new(format!("({year})")).size(12.0).color(MUTED));
                }
            });
            if !result.original_title.is_empty() && result.original_title != result.title {
                ui.label(
                    RichText::new(format!("Original title: {}", result.original_title))
                        .size(11.0)
                        .italics()
                        .color(MUTED),
                );
            }
            let overview = preview_text(&result.overview, 220);
            if !overview.is_empty() {
                ui.add_space(3.0);
                ui.add(Label::new(RichText::new(overview).size(11.5).color(MUTED)).wrap());
            }
        })
        .response;

    ui.interact(
        content.rect,
        ui.make_persistent_id(("tmdb_result", index)),
        Sense::click(),
    )
    .on_hover_cursor(CursorIcon::PointingHand)
}

fn preview_text(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut preview: String = text.chars().take(max_chars).collect();
    preview.push('…');
    preview
}

/// Missing-metadata filters selected in the **Filter by** menu.
#[derive(Debug, Clone, Copy, Default)]
struct MissingFilters {
    covers: bool,
    year: bool,
    original_title: bool,
}

/// Applies the collection search, format, 3D and missing-metadata filters.
fn filter_movies(
    collection: &Collection,
    covers: &CoverIndex,
    search: &str,
    format: Option<DiscFormat>,
    only_3d: bool,
    missing: MissingFilters,
) -> Vec<usize> {
    let mut indices = collection.filter_indices(search, format, only_3d);
    if missing.covers {
        indices.retain(|index| covers.for_upc(&collection.movies[*index].upc).is_none());
    }
    if missing.year {
        indices.retain(|index| collection.movies[*index].year.is_none_or(|year| year == 0));
    }
    if missing.original_title {
        indices.retain(|index| collection.movies[*index].original_title.trim().is_empty());
    }
    indices
}

/// Keeps the TMDB title the user picked as an alternative title when it differs
/// from both the title that was searched for and the movie's own title.
fn add_picked_title(
    details: &mut omdb::Details,
    searched_title: &str,
    movie_title: &str,
    picked_title: &str,
) {
    let picked = picked_title.trim();
    if picked.is_empty() {
        return;
    }

    let matches = |title: &str| title.trim().eq_ignore_ascii_case(picked);
    if matches(searched_title) || matches(movie_title) {
        return;
    }
    if details
        .alternative_titles
        .iter()
        .any(|title| matches(title))
    {
        return;
    }

    details.alternative_titles.insert(0, picked.to_string());
}

/// One clickable metadata comparison row. The incoming provider value is shown
/// on the left and the movie's current value beside it. Only the provider side
/// is framed, and that frame turns green when the field is selected.
trait MetadataChange {
    fn label(&self) -> &'static str;
    fn current(&self) -> &str;
    fn incoming(&self) -> &str;
    fn selected(&self) -> bool;
    fn set_selected(&mut self, selected: bool);
}

impl MetadataChange for omdb::Change {
    fn label(&self) -> &'static str {
        self.label
    }

    fn current(&self) -> &str {
        &self.current
    }

    fn incoming(&self) -> &str {
        &self.incoming
    }

    fn selected(&self) -> bool {
        self.selected
    }

    fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
    }
}

impl MetadataChange for lookup::Change {
    fn label(&self) -> &'static str {
        self.label
    }

    fn current(&self) -> &str {
        &self.current
    }

    fn incoming(&self) -> &str {
        &self.incoming
    }

    fn selected(&self) -> bool {
        self.selected
    }

    fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
    }
}

impl MetadataChange for dvdcompare::Change {
    fn label(&self) -> &'static str {
        self.label
    }

    fn current(&self) -> &str {
        &self.current
    }

    fn incoming(&self) -> &str {
        &self.incoming
    }

    fn selected(&self) -> bool {
        self.selected
    }

    fn set_selected(&mut self, selected: bool) {
        self.selected = selected;
    }
}

fn change_row<C: MetadataChange>(
    ui: &mut Ui,
    index: usize,
    change: &mut C,
    source: &str,
) -> egui::Response {
    let selected = change.selected();
    let content = ui
        .vertical(|ui| {
            ui.label(
                RichText::new(change.label())
                    .strong()
                    .size(12.0)
                    .color(TEXT),
            );
            ui.add_space(4.0);
            ui.columns(2, |columns| {
                value_box(
                    &mut columns[0],
                    source,
                    change.incoming(),
                    "No value from the provider",
                    selected,
                );
                value_box(&mut columns[1], "Current", change.current(), "Empty", false);
            });
        })
        .response;

    let response = ui
        .interact(
            content.rect,
            ui.make_persistent_id(("metadata_change", index)),
            Sense::click(),
        )
        .on_hover_cursor(CursorIcon::PointingHand);
    if response.clicked() {
        change.set_selected(!selected);
    }
    response
}

fn value_box(ui: &mut Ui, caption: &str, value: &str, empty: &str, selected: bool) {
    let fill = if selected {
        Color32::from_rgb(24, 44, 34)
    } else {
        CARD_BG
    };
    let stroke = if selected {
        Stroke::new(1.6, SUCCESS)
    } else {
        Stroke::new(1.0, BORDER)
    };

    egui::Frame::new()
        .inner_margin(Margin::symmetric(10, 7))
        .corner_radius(CornerRadius::same(7))
        .fill(fill)
        .stroke(stroke)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(caption)
                    .size(10.5)
                    .strong()
                    .color(if selected { SUCCESS } else { MUTED }),
            );
            ui.add_space(2.0);
            if value.trim().is_empty() {
                ui.label(RichText::new(empty).size(12.0).color(MUTED));
            } else {
                ui.add(Label::new(RichText::new(value).size(12.5).color(TEXT)).wrap());
            }
        });
}

fn chip(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(CARD_BG)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(TEXT));
        });
}

fn section_title(ui: &mut Ui, title: &str) {
    ui.add_space(16.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(10.0)
            .strong()
            .color(ACCENT),
    );
    ui.add_space(1.0);
    ui.separator();
    ui.add_space(6.0);
}

/// Decoded cover images, kept as GPU textures between frames.
#[derive(Default)]
struct CoverTextures {
    textures: HashMap<PathBuf, egui::TextureHandle>,
    order: VecDeque<PathBuf>,
}

impl CoverTextures {
    /// Returns the texture for an image, decoding and uploading it on first use.
    /// The least recently loaded covers are dropped to keep memory bounded.
    fn get(&mut self, ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
        if let Some(texture) = self.textures.get(path) {
            return Some(texture.clone());
        }

        let image = image::open(path).ok()?.to_rgba8();
        let size = [image.width() as usize, image.height() as usize];
        let pixels = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
        let texture = ctx.load_texture(
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            pixels,
            egui::TextureOptions::LINEAR,
        );

        self.textures.insert(path.to_path_buf(), texture.clone());
        self.order.push_back(path.to_path_buf());
        while self.order.len() > 12 {
            if let Some(oldest) = self.order.pop_front() {
                self.textures.remove(&oldest);
            }
        }

        Some(texture)
    }
}

fn draw_covers(
    ui: &mut Ui,
    textures: &mut CoverTextures,
    covers: &Covers,
    viewer: &mut Option<PathBuf>,
) {
    let width = ((ui.available_width() - 24.0) / 2.0).clamp(120.0, 250.0);
    let height = width * 1.4;

    ui.horizontal(|ui| {
        let mut first = true;
        for (path, label) in [
            (covers.front.as_deref(), "Front"),
            (covers.back.as_deref(), "Back"),
        ] {
            let Some(path) = path else {
                continue;
            };
            if !first {
                ui.add_space(12.0);
            }
            first = false;

            ui.vertical(|ui| {
                if let Some(texture) = textures.get(ui.ctx(), path) {
                    let response = ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(
                            texture.id(),
                            Vec2::new(width, height),
                        ))
                        .corner_radius(CornerRadius::same(6))
                        .sense(Sense::click()),
                    );
                    if response.clicked() {
                        *viewer = Some(path.to_path_buf());
                    }
                    response
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .on_hover_text("Click to enlarge");
                } else {
                    let (rect, _) =
                        ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), CARD_BG);
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "Cover not readable",
                        FontId::proportional(11.0),
                        MUTED,
                    );
                }

                ui.add_space(3.0);
                ui.label(RichText::new(label).size(11.0).color(MUTED));
            });
        }
    });
}

const STAR_GOLD: Color32 = Color32::from_rgb(246, 196, 67);
const STAR_OUTLINE: Color32 = Color32::from_rgb(108, 117, 133);

/// Draws a five-star Letterboxd rating with half-star precision.
fn draw_rating_stars(ui: &mut Ui, rating: f32) {
    const COUNT: usize = 5;
    const SIZE: f32 = 19.0;
    const GAP: f32 = 3.0;

    let rating = rating.clamp(0.0, COUNT as f32);
    let width = SIZE * COUNT as f32 + GAP * (COUNT - 1) as f32;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, SIZE), Sense::hover());
    let _ = response.on_hover_text(format!(
        "Letterboxd rating: {}",
        letterboxd_rating_text(rating)
    ));

    let painter = ui.painter();
    for index in 0..COUNT {
        let center = egui::pos2(
            rect.left() + SIZE / 2.0 + index as f32 * (SIZE + GAP),
            rect.center().y,
        );
        let points = star_points(center, SIZE * 0.48, SIZE * 0.21);
        let filled = (rating - index as f32).clamp(0.0, 1.0);
        if filled > 0.0 {
            let star_rect = Rect::from_center_size(center, Vec2::splat(SIZE));
            let clip = if filled >= 1.0 {
                star_rect
            } else {
                Rect::from_min_max(
                    star_rect.min,
                    egui::pos2(star_rect.left() + SIZE * filled, star_rect.max.y),
                )
            };
            fill_star(&painter.with_clip_rect(clip), center, &points);
        }
        painter.add(Shape::closed_line(points, Stroke::new(1.0, STAR_OUTLINE)));
    }
}

fn fill_star(painter: &egui::Painter, center: egui::Pos2, points: &[egui::Pos2]) {
    // A star is concave, so fill it as a fan of triangles around its center
    // instead of a single convex polygon. A single mesh keeps the triangles
    // from showing antialiased seams.
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, STAR_GOLD);
    for &point in points {
        mesh.colored_vertex(point, STAR_GOLD);
    }
    for index in 0..points.len() {
        mesh.add_triangle(
            0,
            (index + 1) as u32,
            ((index + 1) % points.len() + 1) as u32,
        );
    }
    painter.add(Shape::mesh(mesh));
}

fn star_points(center: egui::Pos2, outer: f32, inner: f32) -> Vec<egui::Pos2> {
    (0..10)
        .map(|index| {
            let radius = if index % 2 == 0 { outer } else { inner };
            let angle = -std::f32::consts::FRAC_PI_2 + index as f32 * std::f32::consts::PI / 5.0;
            egui::pos2(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        })
        .collect()
}

fn letterboxd_rating_text(rating: f32) -> String {
    if rating.fract() == 0.0 {
        format!("{} / 5", rating as u32)
    } else {
        format!("{rating} / 5")
    }
}

fn fact(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).size(11.5).color(MUTED));
    ui.add(
        egui::Label::new(RichText::new(value).size(13.0).color(TEXT))
            .wrap()
            .halign(Align::Min),
    );
    ui.end_row();
}

/// Builds the canonical IMDb title URL, accepting only `tt` followed by digits.
fn imdb_url(imdb_id: &str) -> Option<String> {
    let id = imdb_id.trim();
    let digits = id.strip_prefix("tt")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(format!("https://www.imdb.com/title/{id}/"))
}

fn field_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(MUTED));
}

fn string_list(ui: &mut Ui, items: &[String]) {
    for item in items {
        ui.horizontal(|ui| {
            ui.label(RichText::new("•").size(12.0).color(MUTED));
            ui.label(RichText::new(item).size(12.5).color(TEXT));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hamburger_button_draws_three_lines() {
        let context = egui::Context::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                hamburger_button(ui);
            });
        });
        let lines = output
            .shapes
            .iter()
            .filter(|clipped| matches!(clipped.shape, Shape::LineSegment { .. }))
            .count();
        output.textures_delta.clear();
        assert_eq!(lines, 3);
    }

    #[test]
    fn editing_the_title_rebuilds_the_sort_title() {
        let original = Movie {
            id: "1".to_string(),
            title: "English Patient, the".to_string(),
            sort_title: "English Patient, the".to_string(),
            ..Default::default()
        };
        let mut form = MovieForm::from_movie(&original);
        form.title = "The English Patient".to_string();

        let mut movie = original.clone();
        form.apply_to(&mut movie).unwrap();
        assert_eq!(movie.title, "The English Patient");
        assert_eq!(movie.sort_title, "English Patient, The");
        assert_eq!(movie.sort_key(), "english patient, the");

        form.sort_title = "Custom title".to_string();
        form.apply_to(&mut movie).unwrap();
        assert_eq!(movie.sort_title, "Custom title");
    }

    #[test]
    fn editing_regions_updates_the_movie() {
        let original = Movie {
            id: "1".to_string(),
            title: "Alien".to_string(),
            regions: vec!["1".to_string(), "2".to_string()],
            ..Default::default()
        };
        let mut form = MovieForm::from_movie(&original);
        assert_eq!(form.regions, "1, 2");

        form.regions = "2; 4, 4".to_string();
        let mut movie = original.clone();
        form.apply_to(&mut movie).unwrap();
        assert_eq!(movie.regions, vec!["2", "4"]);

        form.regions.clear();
        form.apply_to(&mut movie).unwrap();
        assert!(movie.regions.is_empty());
    }

    #[test]
    fn editing_the_upc_keeps_only_digits() {
        let original = Movie {
            id: "1".to_string(),
            title: "Alien".to_string(),
            upc: "012236-047605".to_string(),
            ..Default::default()
        };
        let mut form = MovieForm::from_movie(&original);
        assert_eq!(form.upc, "012236-047605");

        form.upc = " 044-007 867020 ".to_string();
        let mut movie = original.clone();
        form.apply_to(&mut movie).unwrap();
        assert_eq!(movie.upc, "044007867020");

        form.upc.clear();
        form.apply_to(&mut movie).unwrap();
        assert!(movie.upc.is_empty());
    }

    #[test]
    fn prefers_the_variant_matching_the_movies_locality() {
        let variant = |locality: &str, title: &str| lookup::ProfileSummary {
            profile_id: format!("044007867020.{locality}"),
            locality: locality.to_string(),
            title: title.to_string(),
            edition: String::new(),
        };
        let variants = vec![
            variant("0", "United States release"),
            variant("4", "United Kingdom release"),
            variant("16", "Finnish release"),
        ];

        assert_eq!(preferred_variant(&variants, "United Kingdom"), 1);
        assert_eq!(preferred_variant(&variants, "4"), 1);
        assert_eq!(preferred_variant(&variants[..1], "Finland"), 0);
    }

    #[test]
    fn filters_movies_with_missing_covers_years_and_original_titles() {
        let collection = Collection {
            version: 1,
            movies: vec![
                Movie {
                    id: "1".to_string(),
                    title: "With front".to_string(),
                    original_title: "With front original".to_string(),
                    upc: "111".to_string(),
                    year: Some(1979),
                    ..Default::default()
                },
                Movie {
                    id: "2".to_string(),
                    title: "Without covers".to_string(),
                    upc: "222".to_string(),
                    ..Default::default()
                },
                Movie {
                    id: "3".to_string(),
                    title: "With back".to_string(),
                    original_title: "   ".to_string(),
                    upc: "333".to_string(),
                    year: Some(0),
                    ..Default::default()
                },
                Movie {
                    id: "4".to_string(),
                    title: "No UPC".to_string(),
                    ..Default::default()
                },
            ],
        };
        let covers = CoverIndex::from_file_names(
            Path::new("/covers"),
            ["111f.jpg".to_string(), "333b.jpg".to_string()],
        );
        let ids = |indices: Vec<usize>| {
            indices
                .into_iter()
                .map(|index| collection.movies[index].id.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "",
                None,
                false,
                MissingFilters::default()
            )),
            vec!["1", "2", "3", "4"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "",
                None,
                false,
                MissingFilters {
                    covers: true,
                    ..Default::default()
                }
            )),
            vec!["2", "4"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "",
                None,
                false,
                MissingFilters {
                    year: true,
                    ..Default::default()
                }
            )),
            vec!["2", "3", "4"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "",
                None,
                false,
                MissingFilters {
                    original_title: true,
                    ..Default::default()
                }
            )),
            vec!["2", "3", "4"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "",
                None,
                false,
                MissingFilters {
                    covers: true,
                    year: true,
                    ..Default::default()
                }
            )),
            vec!["2", "4"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "",
                None,
                false,
                MissingFilters {
                    covers: true,
                    original_title: true,
                    ..Default::default()
                }
            )),
            vec!["2", "4"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "",
                None,
                false,
                MissingFilters {
                    year: true,
                    original_title: true,
                    ..Default::default()
                }
            )),
            vec!["2", "3", "4"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "without",
                None,
                false,
                MissingFilters {
                    covers: true,
                    ..Default::default()
                }
            )),
            vec!["2"]
        );
        assert_eq!(
            ids(filter_movies(
                &collection,
                &covers,
                "without",
                None,
                false,
                MissingFilters {
                    original_title: true,
                    ..Default::default()
                }
            )),
            vec!["2"]
        );
    }

    #[test]
    fn long_fact_values_wrap_within_the_available_width() {
        let context = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(360.0, 400.0),
            )),
            ..Default::default()
        };
        let long = "Das Schweigen der Lämmer, Le silence des agneaux, ".repeat(8);

        let run = || {
            context.run_ui(input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_max_width(320.0);
                    egui::Grid::new("fact_wrap_test")
                        .num_columns(2)
                        .min_col_width(105.0)
                        .show(ui, |ui| {
                            fact(ui, "Alternative titles", &long);
                        });
                });
            })
        };

        // A new grid spends its first frame sizing itself.
        let mut output = run();
        output.textures_delta.clear();
        output = run();

        let rows = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                Shape::Text(text) if text.galley.text() == long => Some(text.galley.rows.len()),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        output.textures_delta.clear();
        assert!(rows > 1, "a long fact value should wrap onto several lines");
    }

    #[test]
    fn builds_imdb_title_links() {
        assert_eq!(
            imdb_url("tt0064116").as_deref(),
            Some("https://www.imdb.com/title/tt0064116/")
        );
        assert_eq!(
            imdb_url(" tt0064116 ").as_deref(),
            Some("https://www.imdb.com/title/tt0064116/")
        );
        assert_eq!(imdb_url(""), None);
        assert_eq!(imdb_url("nm0000123"), None);
        assert_eq!(imdb_url("tt12ab"), None);
    }

    #[test]
    fn clicking_an_omdb_field_toggles_its_selection() {
        fn run_frame(
            context: &egui::Context,
            input: egui::RawInput,
            change: &mut omdb::Change,
            rect: &mut Rect,
        ) {
            let mut output = context.run_ui(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    *rect = change_row(ui, 0, change, "OMDb").rect;
                });
            });
            output.textures_delta.clear();
        }

        let context = egui::Context::default();
        let screen = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(800.0, 600.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let mut change = omdb::Change {
            field: omdb::Field::Genres,
            label: "Genres",
            current: "Crime".to_string(),
            incoming: "Crime, Drama".to_string(),
            selected: false,
        };
        let mut rect = Rect::NOTHING;

        run_frame(&context, input(), &mut change, &mut rect);
        assert!(!change.selected);
        let pos = rect.center();

        let mut moved = input();
        moved.events.push(egui::Event::PointerMoved(pos));
        run_frame(&context, moved, &mut change, &mut rect);

        let mut pressed = input();
        pressed.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        run_frame(&context, pressed, &mut change, &mut rect);

        let mut released = input();
        released.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        run_frame(&context, released, &mut change, &mut rect);

        assert!(change.selected);
    }

    #[test]
    fn adds_a_different_picked_title_as_an_alternative() {
        let mut details = omdb::Details {
            alternative_titles: vec!["Le silence des agneaux".to_string()],
            ..Default::default()
        };

        add_picked_title(
            &mut details,
            "Silence of the Lambs",
            "The Silence of the Lambs",
            "The Silence of the Lambs (1991)",
        );
        assert_eq!(
            details.alternative_titles,
            vec![
                "The Silence of the Lambs (1991)".to_string(),
                "Le silence des agneaux".to_string()
            ]
        );

        // A title that matches the search query or the saved title is not added.
        add_picked_title(
            &mut details,
            "Silence of the Lambs",
            "The Silence of the Lambs",
            "silence of the lambs",
        );
        add_picked_title(
            &mut details,
            "Silence of the Lambs",
            "The Silence of the Lambs",
            "The Silence of the Lambs",
        );

        // Case-insensitive duplicates of an existing alternative are not added.
        add_picked_title(
            &mut details,
            "Silence of the Lambs",
            "The Silence of the Lambs",
            "le silence des agneaux",
        );
        add_picked_title(
            &mut details,
            "Silence of the Lambs",
            "The Silence of the Lambs",
            "   ",
        );
        assert_eq!(
            details.alternative_titles,
            vec![
                "The Silence of the Lambs (1991)".to_string(),
                "Le silence des agneaux".to_string()
            ]
        );
    }

    #[test]
    fn draws_one_filled_mesh_per_full_star() {
        fn mesh_count(rating: f32) -> usize {
            let context = egui::Context::default();
            let mut output = context.run_ui(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw_rating_stars(ui, rating));
            });
            let count = output
                .shapes
                .iter()
                .filter(|clipped| matches!(clipped.shape, Shape::Mesh(_)))
                .count();
            output.textures_delta.clear();
            count
        }

        assert_eq!(mesh_count(0.0), 0);
        assert_eq!(mesh_count(2.5), 3);
        assert_eq!(mesh_count(5.0), 5);
    }
}
