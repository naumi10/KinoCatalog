# User Guide

KinoCatalog is a desktop app for keeping track of a DVD, Blu-ray and 4K Ultra HD
collection. Titles appear in the list on the left; select one to see its details
on the right.

## Adding and editing movies

Choose **Add movie** or press `Ctrl+N` to add a title. The title is the only
required field. You can edit a movie's details, including its locality (release
country), region or Blu-ray area code, original title and international title,
in the movie form. Locality, original title and international title are also
imported from DVD Profiler exports when present; region and area code are
imported from DVD Profiler exports. UPCs are stored as digits only, so hyphens
and spaces are removed when a movie is edited, imported or loaded. The editor
includes a 3D checkbox for manual and barcode-added titles.

### Add a movie with a barcode

Open **Add movie** (`Ctrl+N`). The barcode field is focused automatically, so a
USB barcode scanner can scan a disc and send Enter without any clicking. You can
also type a UPC by hand.

The app looks up the profile in Invelos in the background and fills in the title,
format, year, runtime, rating, director, cast, synopsis and UPC. The details
remain editable before saving. Any high-quality front and back covers returned
by the server are saved to the cover folder and shown for the new movie.

The lookup is built into the app; Python and a separate helper script are not
needed. A UPC can exist in several Invelos localities (for example, the same
Nordic Blu-ray may have Swedish, Norwegian, Danish and Finnish profiles). If so,
the Add dialog lists each matching profile with its country, title and edition,
preselects the preferred locality, and waits for you to load one.

The preferred locality can be configured with the `KINOCATALOG_LOCALITY` environment
variable. Its default is `16` (Finland). This preference only chooses the initial
selection; you can choose another country for any barcode. Barcodes with a
single profile are loaded automatically.

If a barcode is not found or the network is unavailable, the dialog shows the
error and leaves the form untouched so you can enter the movie manually.

## Importing a collection

In the app, click **Import XML…** and choose the DVD Profiler `Collection.xml`
file, or drop the file onto the window. For scripted or headless imports, run:

```sh
KinoCatalog --import /path/to/Collection.xml
```

This merges the export into the current collection and exits. The import summary
includes the number of titles added, updated or skipped and the destination data
file.

Imports are matched by a stable id: the DVD Profiler profile id when available,
otherwise the UPC, otherwise title and year. Personal notes on an existing entry
are kept when the same profile is imported again. XML exports may be Windows-1252
or UTF-8 encoded.

## Importing Letterboxd ratings

Click **Import Letterboxd…** and choose a Letterboxd `.zip` export. The app reads
`ratings.csv` from the archive and matches each rating to a movie by title and
year; original titles are considered too. Matches are shown as gold stars beside
the movie title, including half stars. Ratings range from 0.5 to 5 stars. A
rating that matches several titles without a disambiguating year is skipped
rather than guessed.

For scripted imports, run:

```sh
KinoCatalog --import-letterboxd /path/to/letterboxd-export.zip
```

## Updating details from OMDb

Select a movie and click **Update from OMDB** in the details pane. The app looks
up the title and year in the [Open Movie Database](https://www.omdbapi.com/).

OMDb requires a free API key. The first update asks for it and stores it beside
the collection as `omdb_api_key` (for example,
`~/.local/share/KinoCatalog/omdb_api_key`) with owner-only permissions. The key is
reused on later updates. If the key is rejected, the dialog offers **Change API
key**.

When the response arrives, a comparison window lists the mapped fields:

| OMDb field | Movie field |
| --- | --- |
| `Year` | Year (only when the movie has no usable year) |
| `Genre` | Genres |
| `Director` | Directors |
| `Writer` | Writers |
| `Actors` | Actors |
| `Plot` | Synopsis |
| `Country` | Country |
| `imdbID` | IMDb ID |

Country names are stored in a single form: `United States of America` is saved as
`United States`, so it does not appear as a change against an existing entry.

Each field is a clickable row showing the OMDb value and the current detail.
Changed values are selected by default; fields whose current detail is empty are
selected when OMDb has a value, and empty OMDb values are left unselected. Click
a row to toggle selection. **Select all** and **Select none** adjust the whole
list. **Update movie** saves only the selected fields. Existing cast roles are
kept when an actor appears in both lists.

A stored IMDb ID appears in the details pane as a clickable IMDb link. It is also
kept when a DVD Profiler XML export is re-imported.

## Updating details from TMDB

Select a movie and click **Update from TMDB** in the details pane. The app
searches [The Movie Database](https://www.themoviedb.org/) by the movie title.
Because several films can share a title, the search results are listed with their
year, original title and a short synopsis. Pick the correct entry and click
**Load selected title**; the app then fetches full details using that TMDB id.
The search field in the results window can be edited and searched again. The
detail request also includes TMDB's alternative titles, which are offered in the
comparison window like any other field and appear in the movie details once
saved. If the title you pick differs from the title that was searched for and
from the movie's saved title, the picked title is added to the alternative titles
as well.

TMDB requires an API key or read access token. The first update asks for one and
stores it beside the collection as `tmdb_api_key` (for example,
`~/.local/share/KinoCatalog/tmdb_api_key`) with owner-only permissions. Either a v3
API key or a v4 read access token from the TMDB settings page works. If the
credential is rejected, the dialog offers **Change API key**.

The comparison window works the same way as the OMDb window:

| TMDB field | Movie field |
| --- | --- |
| Release year | Year (only when the movie has no usable year) |
| Alternative titles | Alternative titles |
| Genres | Genres |
| Crew (Directing) | Directors |
| Crew (Writing) | Writers |
| Top-billed cast | Actors |
| Overview | Synopsis |
| Production countries | Country |
| IMDb id | IMDb ID |

Production countries use the same canonical spelling, so `United States of
America` is stored as `United States`.

Changed values are selected by default, and **Update movie** saves only the
selected fields. **Choose another title** returns to the search results if the
selected match was not the right film.

## Updating details from Invelos

Select a movie and click **Update from Invelos** in the details pane. The app
looks up the movie's barcode (UPC) in the Invelos profile service and downloads
the matching profile. If Invelos has profiles for the barcode in several
localities, a list is shown first and the release matching the movie's locality
is preselected; choose the right release and click **Load selected profile**.

The comparison window offers the fields stored in the Invelos profile:

| Invelos profile | Movie field |
| --- | --- |
| Title | Title |
| Original title | Original title |
| Sort title | Sort title |
| Media type | Format |
| Production year | Year |
| Runtime | Runtime |
| Rating | Rating |
| Directed by | Directors |
| Cast and roles | Actors |
| Synopsis | Synopsis |
| UPC | UPC |
| Locality | Locality |

Changed values are selected by default; fields that are empty in the profile are
left unselected. Click a row to toggle it, or use **Select all** and **Select
none**. **Update movie** saves only the selected fields. **Choose another
profile** returns to the locality list after a profile has been downloaded. Any
front or back cover images available for the profile are saved to the cover
folder, just like the barcode lookup when adding a movie.

A movie without a barcode cannot be looked up; add the UPC in the movie editor
first.

## Updating details from DVDCompare

Select a movie and click **Update from DVDCompare** in the details pane. The app
searches [DVDCompare](https://www.dvdcompare.net/) for the movie title and lists
the matching film pages with their year and disc format. The result matching the
movie's year and format is preselected, and the search field can be edited and
searched again. Click **Load selected film**, then choose one of the regional
releases on the film page.

DVDCompare support is built into the app and reads pages directly from
dvdcompare.net. No Python installation or separate scraper project is needed.

The release comparison window offers these fields:

| DVDCompare release | Movie field |
| --- | --- |
| Soundtrack(s) | Audio |
| Subtitles | Subtitles |
| Case type | Case type |
| UPC Number | UPC |
| Cut information | Cuts |

Soundtrack lines are converted to the collection's usual
`Language · Format · Channels` style, for example
`English · Dolby Digital · 5.1`. Values that DVDCompare reports as `None` are
treated as empty. UPCs are stored as digits only.

Cut information is read from the film page's cut table and matched to the
chosen release by its region, distributor and edition. The **Cuts** value keeps
the cut status, runtime and any detailed notes, for example
`No cuts (91:12 NTSC)` or `Yes - Missing a few seconds (91:00 NTSC)` followed by
the affected scenes. The film-level cut summary is stored per movie, so it can
also be edited by hand in the movie editor.

Changed values are selected by default; fields that are empty in the release are
left unselected. Click a row to toggle it, or use **Select all** and
**Select none**. **Update movie** saves only the selected fields. **Choose
another release** returns to the release list, and **Choose another film**
returns to the search results.

DVDCompare data is read directly from dvdcompare.net. The app is not affiliated
with dvdcompare.net.

## Checking for updates

Choose **Update to new version…** from the menu in the top-right corner. The app
checks the latest GitHub release for `naumi10/KinoCatalog`. If the release is newer
than the running version, it downloads `KinoCatalog-linux-x64.tar.gz` and installs
the new executable and icon alongside the running app. Restart the app to use the
new version. The current version is the package version in `Cargo.toml` and is
also shown in the **About** window.

Automatic updates use the Linux x64 release archive. Set `KINOCATALOG_INSTALL_DIR`
to install into a directory other than the running executable's directory.

## Browsing and searching

Search across title, cast, director, rating, year and UPC. Use the format filters
to narrow the collection; the **3D Blu-ray** filter shows only 3D titles. The
**Filter by** menu currently offers **Missing covers**, which shows only titles
without a front or back cover image, **Missing year**, which shows titles
without a release year, and **Missing original title**, which shows titles
without an original title. The title list can be sorted, and deleting a movie
requires confirmation.

Each title in the collection list carries a coloured text badge for its format:
**DVD**, **BD**, **3D BD** or **4K UHD**. Hover over a badge to see the full
format name; the details pane shows the full label as well.

Front and back cover thumbnails appear in the details pane when matching images
are available. Click a thumbnail to enlarge it; press `Esc` or click **Close** to
dismiss the enlarged view.

| Shortcut | Action |
| --- | --- |
| `Ctrl+N` | Add a movie |
| `Ctrl+F` | Focus the search field |

## Cover images

Cover images are looked up by UPC in `~/covers`, or in the folder chosen in
**Settings**. The `KINOCATALOG_COVERS` environment variable is used only until a
folder is saved in Settings. The dotted number in a file name is the Invelos
locality ID (`16` = Finland, `11` = Sweden, `4` = United Kingdom), not a disc
number:

```
~/covers/012236047605f.jpg       front, no locality recorded
~/covers/012236047605b.jpg       back, no locality recorded
~/covers/044007867020.16f.jpg    Finnish front for this UPC
~/covers/044007867020.16b.jpg    Finnish back for this UPC
```

When several releases share a UPC, the movie's locality is used to pick its own
front and back files. Covers downloaded by the barcode lookup are named
`044007867020.16_front.jpg` / `_back.jpg` and are matched the same way. UPCs are
stored in the collection without hyphens or spaces, and separators in existing
cover file names are ignored when matching. `.jpeg` is accepted as well as
`.jpg`. If a UPC has no matching images, the covers section is omitted.

### Scanning covers with SANE

Cover scanning requires SANE's `scanimage` command. It is not bundled with
KinoCatalog, and the Windows installer does not install a scanner driver. In
practice this feature is intended for **Linux** with SANE and a supported
scanner installed. On Windows it can only work if you separately install a SANE
frontend with `scanimage.exe` on `PATH`; TWAIN/WIA scanners are not supported.

The details pane has a **Scan cover images** button for movies with a UPC. Place
the whole, opened cover face down on the scanner and press the button. KinoCatalog
runs SANE's `scanimage` command with PNG output, `Color` mode, 300 dpi and a
184 × 274 mm area — tall enough for the front and back panels one above the other.

The scan is split horizontally: the **top** half is the front and the **bottom**
half is the back. Each half is rotated **90° clockwise** so it appears upright
in the cover viewer.

The front half is shown first with a rectangular selection. The starting
selection comes from the saved default for the movie's format when one exists;
otherwise it excludes about **10 mm** of spine and limits the height to **150 mm**,
matching a Blu-ray cover. Drag the corner handles to resize, drag inside the
rectangle to move it, or drag on an empty area to draw a new selection. Choose
**Save front** to crop and save `<UPC>.<locality>f.jpg` when the movie has a
locality, or `<UPC>f.jpg` otherwise. The back half is then shown with
its own selection; **Save back** writes the matching `b.jpg` file. **Reset selection**
restores the starting rectangle, and **Discard** stops without changing the
remaining cover. Files are written to the covers folder, replacing any files
with those names.

Check **Remember as `<format>` default** before saving to store both selections
for that disc format (DVD, Blu-ray or 4K Ultra HD). The checkbox is pre-selected
when a default for that format already exists. Defaults are kept in
`settings.json` beside `movies.json` and are used as the starting rectangles for
later scans of the same format.

Saved covers are scaled to at most **500 × 700 px** and the JPEG quality is
lowered as needed so each file stays at or below **200 kB**, matching the size
of covers already in the collection.

KinoCatalog finds the device with `scanimage -L` and prefers a `snapscan:`
device when one is available. If auto-detection picks the wrong scanner, run
`scanimage -L` and set `KINOCATALOG_SANE_DEVICE` to the full name it prints:

```sh
export KINOCATALOG_SANE_DEVICE='snapscan:libusb:007:003'
```

A bare `snapscan` is a backend name, not a device name. `scanimage` must be
installed and available on `PATH`, and the scanner must be powered on.

## Settings

Choose **Settings** from the menu in the top-right corner. The window has a list
of setting groups on the left and the values for the selected group on the right:

- **Collection** — the folder containing `movies.json`. You can also type the
  path to the JSON file directly.
- **Covers** — the folder containing the front and back cover JPEGs.
- **Title list** — rules deciding which title is shown in the left-hand movie
  list.

The folder fields have a **Browse…** button, and `~` is expanded to your home
folder. The folder choices are stored in `KinoCatalog/locations.json` under your
config folder (`~/.config` on Linux, `%APPDATA%` on Windows). Set
`KINOCATALOG_CONFIG` to use a different file.

### Title list rules

Each rule says: if the movie's country is one of the listed countries, show a
chosen title instead of the normal one. Separate countries with commas. Rules
are checked in order and the first match wins. If the selected title is empty,
the normal title is used instead.

The built-in default shows the **Original title** for United States, United
Kingdom, Australia and South Africa movies. You can edit, remove or add rules in
Settings; the rules are saved in `settings.json` beside `movies.json`. For
example, to show the international title for Finnish and Swedish releases, add:

```
If country is: Finland, Sweden
Show:          International title
```

Nothing is moved or deleted. The collection at the new location is loaded at
once, along with its own API keys and settings (cover-crop defaults and title
list rules); those files are copied from the old location only when the new
folder does not already have them.
Existing covers and API keys in the old folder are left untouched. `--data` still
overrides the saved collection folder for a single command.

## Collection data

By default, the collection is stored in:

```
~/.local/share/KinoCatalog/movies.json
```

On the first start after the rename, an existing
`~/.local/share/myMovies` collection and its saved API keys are copied to the new
folder. The old folder is left untouched, so no data is lost.

Set the `KINOCATALOG_DATA` environment variable to choose a different file before
Settings has been used, or pass `--data <file.json>` to override Settings for a
single command:

```sh
KINOCATALOG_DATA=/tmp/test.json cargo run
KinoCatalog --import Collection.xml --data /tmp/test.json
```

The old `MYMOVIES_DATA`, `MYMOVIES_COVERS`, `MYMOVIES_LOCALITY` and
`MYMOVIES_INSTALL_DIR` variable names are still accepted as fallbacks.

The JSON file is written atomically. If it cannot be decoded at startup, it is
renamed to `movies.json.invalid-<timestamp>` instead of being overwritten, and
the app starts with an empty collection.
