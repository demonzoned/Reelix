//! The first-run install wizard: every step, question, answer and help text, in one place.
//!
//! The browser wizard (`/setup`) and the terminal wizard (`reelix setup`) both read this module,
//! so they ask the same questions, in the same order, with the same answers. Only the drawing
//! differs. Each answer maps onto a setting Reelix already has (see the `*_for` helpers), so a
//! choice in either wizard does the same thing.
//!
//! Words follow one style: plain English, sentence case, no contractions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How the person starts: the first question, which picks the workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// A new media server, one part at a time.
    Fresh,
    /// Everything comes back from a Reelix backup file.
    Restore,
    /// Bring over Radarr, Sonarr, Prowlarr, Readarr, Lidarr or Jellyseerr.
    Migrate,
}

impl Start {
    pub const ALL: [Start; 3] = [Start::Fresh, Start::Restore, Start::Migrate];

    pub fn key(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Restore => "restore",
            Self::Migrate => "migrate",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    /// The workflow's name, for the progress line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Fresh => "New install",
            Self::Restore => "Restore",
            Self::Migrate => "Migration",
        }
    }
}

/// Every step either wizard can show. The order of [`flow`] is the order they are asked in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    Start,
    Code,
    Restore,
    Account,
    Follow,
    Migrate,
    Tmdb,
    Folders,
    Torrent,
    Usenet,
    DownloadOptions,
    Sites,
    Quality,
    Monitor,
    MediaServers,
    Requests,
    Finish,
}

/// The steps of each workflow, in order. [`applies`] then leaves out the ones that do not fit.
pub fn flow(start: Start) -> &'static [StepId] {
    use StepId::*;
    match start {
        self::Start::Fresh => &[
            Start,
            Code,
            Account,
            // What you want, where it lives, finding it, downloading it, watching it.
            Follow,
            Quality,
            Monitor,
            Folders,
            Tmdb,
            Sites,
            Torrent,
            Usenet,
            DownloadOptions,
            MediaServers,
            Requests,
            Finish,
        ],
        self::Start::Restore => &[Start, Code, Restore, Finish],
        self::Start::Migrate => &[
            Start,
            Code,
            Account,
            Migrate,
            Quality,
            Monitor,
            Folders,
            Tmdb,
            Sites,
            Torrent,
            Usenet,
            DownloadOptions,
            MediaServers,
            Requests,
            Finish,
        ],
    }
}

/// Facts about this server that decide whether a step is shown, beside the answers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context {
    /// The browser is not on the server itself, so the set-up code is needed.
    pub code_required: bool,
    /// An admin is signed in already: the wizard runs again (Settings, then Set-up wizard), so
    /// there is no code, no account to make and no backup to restore over.
    pub signed_in: bool,
}

/// Whether `step` is shown, given the answers so far.
pub fn applies(step: StepId, answers: &Answers, ctx: Context) -> bool {
    let start = answers.start();
    if !flow(start).contains(&step) {
        return false;
    }
    if ctx.signed_in && matches!(step, StepId::Code | StepId::Account | StepId::Restore) {
        return false;
    }
    match step {
        StepId::Code => ctx.code_required,
        StepId::Tmdb => answers.follows(Kind::Movies) || answers.follows(Kind::Series),
        StepId::DownloadOptions => answers.get("torrent") != "none" && answers.get("usenet") != "none",
        StepId::Monitor => answers.follows(Kind::Movies),
        _ => true,
    }
}

/// Whether an answer is offered. Running the wizard again offers no restore: a backup is
/// restored in System, then Backup, once Reelix is set up.
pub fn offered(question: &Question, value: &str, ctx: Context) -> bool {
    !(ctx.signed_in && question.key == "start" && value == "restore")
}

/// A library folder the Library folders step suggests. The browser and the terminal both offer
/// these, for the kinds the person follows, so the two wizards ask for the same folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LibraryFolder {
    /// The folder's kind, as the folder lists name it: `movie`, `series`, `ebook`, `audiobook`
    /// or `music`.
    pub key: &'static str,
    pub label: &'static str,
    /// Where it is suggested, as Reelix sees it: inside `/data`, the one folder the Docker
    /// compose file mounts for downloads and media.
    pub path: &'static str,
}

const LIBRARY_FOLDERS: [(Kind, LibraryFolder); 5] = [
    (
        Kind::Movies,
        LibraryFolder {
            key: "movie",
            label: "Movies",
            path: "/data/media/movies",
        },
    ),
    (
        Kind::Series,
        LibraryFolder {
            key: "series",
            label: "Series",
            path: "/data/media/series",
        },
    ),
    (
        Kind::Books,
        LibraryFolder {
            key: "ebook",
            label: "Ebooks",
            path: "/data/media/books",
        },
    ),
    (
        Kind::Books,
        LibraryFolder {
            key: "audiobook",
            label: "Audiobooks",
            path: "/data/media/audiobooks",
        },
    ),
    (
        Kind::Music,
        LibraryFolder {
            key: "music",
            label: "Music",
            path: "/data/media/music",
        },
    ),
];

/// The folders to suggest, in order, for what the person follows.
pub fn library_folders(answers: &Answers) -> Vec<LibraryFolder> {
    LIBRARY_FOLDERS
        .iter()
        .filter(|(kind, _)| answers.follows(*kind))
        .map(|(_, f)| *f)
        .collect()
}

/// The steps that will be shown, in order.
pub fn steps_shown(answers: &Answers, ctx: Context) -> Vec<StepId> {
    flow(answers.start())
        .iter()
        .copied()
        .filter(|s| applies(*s, answers, ctx))
        .collect()
}

/// What a person can choose to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Movies,
    Series,
    Books,
    Music,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Movies, Kind::Series, Kind::Books, Kind::Music];

    pub fn key(self) -> &'static str {
        match self {
            Self::Movies => "movies",
            Self::Series => "series",
            Self::Books => "books",
            Self::Music => "music",
        }
    }
}

/// The answers so far, keyed by [`Question::key`]. Several answers to one question are kept
/// comma-separated, in the order of the choices.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Answers(pub BTreeMap<String, String>);

impl Answers {
    /// Every question answered with its recommended (or first) choice.
    pub fn with_defaults() -> Self {
        let mut a = Self::default();
        for step in STEPS {
            for q in step.questions {
                a.0.insert(q.key.to_owned(), q.default_value());
            }
        }
        a
    }

    pub fn get(&self, key: &str) -> &str {
        self.0.get(key).map(String::as_str).unwrap_or("")
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        self.0.insert(key.to_owned(), value.into());
    }

    pub fn start(&self) -> Start {
        Start::from_key(self.get("start")).unwrap_or(Start::Fresh)
    }

    /// A migration follows whatever the old apps followed, so every kind counts.
    pub fn follows(&self, kind: Kind) -> bool {
        self.start() == Start::Migrate || self.get("follow").split(',').any(|k| k == kind.key())
    }

    /// Whether `value` is among the answers to `key`.
    pub fn has(&self, key: &str, value: &str) -> bool {
        self.get(key).split(',').any(|v| v == value)
    }
}

/// One answer to a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    /// Shown first, with "(recommended)". At most one per question.
    pub recommended: bool,
}

const fn choice(value: &'static str, label: &'static str, hint: &'static str) -> Choice {
    Choice {
        value,
        label,
        hint,
        recommended: false,
    }
}

const fn recommended(value: &'static str, label: &'static str, hint: &'static str) -> Choice {
    Choice {
        value,
        label,
        hint,
        recommended: true,
    }
}

/// A question with fixed answers. Free-text fields (the account, keys, addresses) are drawn by
/// each wizard from Reelix's own forms and provider schemas instead.
#[derive(Debug, Clone, Copy)]
pub struct Question {
    pub key: &'static str,
    pub legend: &'static str,
    pub choices: &'static [Choice],
    /// Several answers at once (check boxes, or numbers with commas in a terminal).
    pub multi: bool,
    /// Asked only when this holds.
    pub when: Option<fn(&Answers) -> bool>,
}

impl Question {
    pub fn asked(&self, answers: &Answers) -> bool {
        self.when.is_none_or(|w| w(answers))
    }

    /// The recommended choice, or the first. For several answers: every choice but "none".
    pub fn default_value(&self) -> String {
        if self.multi {
            return self
                .choices
                .iter()
                .filter(|c| c.value != "none")
                .map(|c| c.value)
                .collect::<Vec<_>>()
                .join(",");
        }
        self.choices
            .iter()
            .find(|c| c.recommended)
            .or(self.choices.first())
            .map(|c| c.value.to_owned())
            .unwrap_or_default()
    }

    pub fn choice(&self, value: &str) -> Option<&'static Choice> {
        self.choices.iter().find(|c| c.value == value)
    }
}

/// "What is…" help: a term and a few short paragraphs.
#[derive(Debug, Clone, Copy)]
pub struct Help {
    pub term: &'static str,
    pub text: &'static [&'static str],
}

/// One step of the wizard.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub id: StepId,
    /// Stable id: named in feedback reports and in the address (`/setup#tmdb`).
    pub key: &'static str,
    /// The part of the set-up this step belongs to.
    pub stage: &'static str,
    /// Short name, for the progress line and the terminal's heading box.
    pub title: &'static str,
    /// The step's heading.
    pub question: &'static str,
    pub lead: &'static str,
    /// The step can be skipped and done later in Settings.
    pub optional: bool,
    pub questions: &'static [Question],
    pub help: &'static [Help],
}

pub fn step(id: StepId) -> &'static Step {
    STEPS
        .iter()
        .find(|s| s.id == id)
        .expect("every StepId has a Step")
}

pub fn step_by_key(key: &str) -> Option<&'static Step> {
    STEPS.iter().find(|s| s.key == key)
}

/// The kinds of feedback, as Snaglist offers them, for the terminal (the browser uses Snaglist).
pub const FEEDBACK_KINDS: &[Choice] = &[
    choice(
        "bug",
        "Something is broken",
        "An error, a step that does nothing, wrong data",
    ),
    choice(
        "ux",
        "Hard to use",
        "Confusing, too many steps, not where you expected",
    ),
    choice(
        "content",
        "Words are wrong",
        "Spelling, unclear or out-of-date text",
    ),
    choice(
        "accessibility",
        "Hard to use with my tools or needs",
        "Keyboard, screen reader, zoom, contrast",
    ),
    choice("feature", "A new feature", "Something Reelix cannot do yet"),
    choice(
        "idea",
        "Make something better",
        "An improvement to something that already works",
    ),
    choice("other", "Something else", "A question, a thank you, anything"),
];

/// Where feedback goes: new issues on Reelix's GitHub.
pub const FEEDBACK_REPO: &str = "demonzoned/Reelix";

/// A provider the wizard suggests but Reelix does not set up itself.
#[derive(Debug, Clone, Copy)]
pub struct Provider {
    pub name: &'static str,
    pub note: &'static str,
    /// The sign-up page. `None` until the links are chosen: see [`LINKS_ARE_PLACEHOLDERS`].
    pub link: Option<&'static str>,
}

/// Every provider and help centre link in the wizard is a placeholder for now: the VPN and Usenet
/// provider programmes are not chosen yet, and the help centre does not exist yet. Both wizards
/// say so where a link would be.
pub const LINKS_ARE_PLACEHOLDERS: bool = true;

/// The help centre's address, when there is one. Until then "What is…" help is all in the wizard.
pub const HELP_CENTRE: Option<&str> = None;

/// VPN providers suggested for torrents. Placeholders until real providers are chosen.
pub const VPN_PROVIDERS: &[Provider] = &[
    Provider {
        name: "Example VPN One",
        note: "WireGuard, port forwarding",
        link: None,
    },
    Provider {
        name: "Example VPN Two",
        note: "WireGuard",
        link: None,
    },
    Provider {
        name: "Example VPN Three",
        note: "OpenVPN, port forwarding",
        link: None,
    },
];

/// Usenet providers suggested for a Usenet app. Placeholders until real providers are chosen.
pub const USENET_PROVIDERS: &[Provider] = &[
    Provider {
        name: "Example Usenet One",
        note: "Unlimited plan",
        link: None,
    },
    Provider {
        name: "Example Usenet Two",
        note: "Block account, pay once",
        link: None,
    },
];

/// The words shown where a provider or help centre link will go.
pub const PLACEHOLDER_NOTE: &str = "Placeholder: the real providers and their links are not chosen yet, so nothing here opens a sign-up page.";

fn torrent_chosen(a: &Answers) -> bool {
    a.get("torrent") != "none"
}
fn usenet_chosen(a: &Answers) -> bool {
    a.get("usenet") != "none"
}
fn watching(a: &Answers) -> bool {
    a.follows(Kind::Movies) || a.follows(Kind::Series)
}

/// The quality profile (one of Reelix's built-in profiles) for the `quality` answer.
pub fn quality_profile_for(answer: &str) -> &'static str {
    match answer {
        "uhd" => "Ultra-HD",
        "small" => "HD-720p",
        _ => "HD-1080p",
    }
}

/// The movie `minimum_availability` request default for the `movie_when` answer.
pub fn minimum_availability_for(answer: &str) -> &'static str {
    match answer {
        "announced" => "announced",
        "cinemas" => "in_cinemas",
        _ => "released",
    }
}

const HELP_SETUP_CODE: Help = Help {
    term: "What is the set-up code?",
    text: &[
        "Reelix prints a set-up code when it starts with no accounts. It proves you are the person who installed Reelix, so nobody else on your network can make the first account.",
        "Find it in the output of the terminal where Reelix started, or run docker logs reelix on the server. On the server itself, no code is needed.",
    ],
};

const HELP_ARR: Help = Help {
    term: "What are the *arr apps?",
    text: &[
        "Radarr (movies), Sonarr (series), Prowlarr (sites to search), Readarr (books), Lidarr (music) and Jellyseerr (requests) each do one job. Reelix does all of them in one app.",
        "Reelix reads their settings through each app's address and API key. Your old apps and their files are not changed.",
    ],
};

const HELP_API_KEY: Help = Help {
    term: "What is an API key?",
    text: &[
        "A long password one app uses to talk to another. In each *arr app it is under Settings, then General, then Security.",
        "Reelix keeps keys on this server and never shows them again.",
    ],
};

const HELP_BACKUP: Help = Help {
    term: "What is in a Reelix backup?",
    text: &[
        "The database, the settings file (config.toml) and the secret key. Your movies, series, books and music are not in it: they stay in your folders.",
        "Reelix makes backups in System, then Backup. Their names start with reelix-backup.",
    ],
};

const HELP_TMDB: Help = Help {
    term: "What is TMDB?",
    text: &[
        "The Movie Database: a free site with the title, poster, cast and release dates of almost every movie and series. Reelix asks it about each title you add.",
        "Make a free account, go to Settings, then API, and request a key for personal use. Copy the API Read Access Token, the longer of the two.",
    ],
};

const HELP_DATA: Help = Help {
    term: "What is /data?",
    text: &[
        "The installer shares one folder on your server with Reelix, and Reelix sees it as /data. Every path inside Reelix starts with /data.",
        "Downloads and the library are in the same folder, so a finished download moves into the library in an instant, with no second copy.",
    ],
};

const HELP_TORRENTS: Help = Help {
    term: "What do I need for torrents?",
    text: &[
        "A torrent app, such as qBittorrent, and sites that list torrents. Torrents download from other people and upload to them at the same time (seeding).",
        "Some sites ask you to upload as much as you download (ratio). Your address is seen by other people in the same torrent, which is why many people use a VPN.",
    ],
};

const HELP_VPN: Help = Help {
    term: "What is a VPN, and do I need one?",
    text: &[
        "A VPN sends your torrent app's traffic through another company's server, so other people in a torrent see that server's address and not yours.",
        "Reelix does not set up a VPN. Run your torrent app through one with a VPN container that has a kill switch, so torrents stop if the VPN drops. Usenet needs no VPN.",
    ],
};

const HELP_USENET: Help = Help {
    term: "What do I need for Usenet?",
    text: &[
        "A Usenet provider (a paid account that stores the files), an indexer (a site that lists them as NZB files) and a Usenet app, such as SABnzbd.",
        "Your provider's details go into the Usenet app. Reelix needs only the app's address and API key.",
    ],
};

const HELP_INDEXERS: Help = Help {
    term: "What are sites to search (indexers)?",
    text: &[
        "Sites that list releases: torrent sites, and Usenet indexers. Reelix searches them for what you follow and sends the best release to your download app.",
        "Reelix has a built-in list of sites. A private site asks for your account details or key.",
    ],
};

const HELP_QUALITY: Help = Help {
    term: "What does quality change?",
    text: &[
        "Which releases Reelix downloads, and how much space each takes. Reelix keeps looking until it has a copy at the quality you choose.",
        "Each movie or series can have its own quality later, on its page.",
    ],
};

const HELP_MEDIA_SERVER: Help = Help {
    term: "What is a media server?",
    text: &[
        "The app you watch or listen with, such as Jellyfin, Emby or Plex. Reelix tells it about each import, so new titles appear straight away.",
        "Your household can sign in to Reelix with their Jellyfin, Emby or Plex accounts.",
    ],
};

const HELP_REQUESTS: Help = Help {
    term: "What are requests?",
    text: &[
        "People in your home ask for a movie or series, and Reelix adds it and searches for it. You choose whether each request waits for your approval.",
    ],
};

const HELP_PLAIN: Help = Help {
    term: "How do I use the terminal wizard with a screen reader?",
    text: &[
        "Start it with reelix setup --plain, or set REELIX_PLAIN=1. Plain mode asks the same questions as numbered lines that scroll, with nothing cleared or redrawn, and no boxes.",
    ],
};

/// Every step, in no particular order: [`flow`] gives the order.
pub const STEPS: &[Step] = &[
    Step {
        id: StepId::Start,
        key: "start",
        stage: "Choose how to start",
        title: "How to start",
        question: "How do you want to start?",
        lead: "Each answer has its own steps. You can change anything later in Settings.",
        optional: false,
        questions: &[Question {
            key: "start",
            legend: "Choose one",
            choices: &[
                recommended(
                    "fresh",
                    "New install",
                    "A new media server. Reelix asks for each part, one step at a time.",
                ),
                choice(
                    "restore",
                    "Restore a Reelix backup",
                    "You used Reelix before, on this server or another. Everything comes back from the backup file.",
                ),
                choice(
                    "migrate",
                    "Migrate from the *arr apps",
                    "You use Radarr, Sonarr, Prowlarr, Readarr, Lidarr or Jellyseerr now. Your old apps are not changed.",
                ),
            ],
            multi: false,
            when: None,
        }],
        help: &[HELP_ARR, HELP_BACKUP, HELP_PLAIN],
    },
    Step {
        id: StepId::Code,
        key: "code",
        stage: "Sign in",
        title: "Set-up code",
        question: "Enter the set-up code",
        lead: "The code proves you are the person who installed Reelix. It is in the installer's output. If you closed the terminal, run docker logs reelix on the server.",
        optional: false,
        questions: &[],
        help: &[HELP_SETUP_CODE],
    },
    Step {
        id: StepId::Restore,
        key: "restore",
        stage: "Restore your backup",
        title: "Restore a backup",
        question: "Restore a Reelix backup",
        lead: "Choose a backup file. Reelix checks it, then restores it the next time Reelix starts. If the database does not open, Reelix goes back to this empty set-up.",
        optional: false,
        questions: &[],
        help: &[HELP_BACKUP],
    },
    Step {
        id: StepId::Account,
        key: "account",
        stage: "Your account",
        title: "Admin account",
        question: "Create the admin account",
        lead: "This account can change everything in Reelix. You can add accounts for your household later.",
        optional: false,
        questions: &[],
        help: &[HELP_SETUP_CODE],
    },
    Step {
        id: StepId::Follow,
        key: "follow",
        stage: "What you want",
        title: "What to follow",
        question: "What do you want Reelix to follow?",
        lead: "Reelix leaves out the steps for anything you do not choose. You can add it later in Settings.",
        optional: false,
        questions: &[Question {
            key: "follow",
            legend: "Choose everything you want",
            choices: &[
                choice("movies", "Movies", "Information from TMDB, with a free key."),
                choice("series", "Series", "Information from TMDB, with a free key."),
                choice(
                    "books",
                    "Books",
                    "Ebooks and audiobooks. Information from Open Library, no key.",
                ),
                choice("music", "Music", "Information from MusicBrainz, no key."),
            ],
            multi: true,
            when: None,
        }],
        help: &[HELP_TMDB],
    },
    Step {
        id: StepId::Migrate,
        key: "migrate",
        stage: "Migration plan",
        title: "Bring over your apps",
        question: "Bring over your apps",
        lead: "For each app, give its address and API key. Reelix shows what it would bring over, and nothing changes until you agree. Your old apps and their files are not changed.",
        optional: false,
        questions: &[],
        help: &[HELP_ARR, HELP_API_KEY],
    },
    Step {
        id: StepId::Tmdb,
        key: "tmdb",
        stage: "Finding it",
        title: "Movie and series information",
        question: "Connect TMDB for movie and series information",
        lead: "Reelix gets titles, posters and release dates from TMDB (The Movie Database). Without a key, Reelix cannot find movies or series. A free key takes about 5 minutes.",
        optional: true,
        questions: &[],
        help: &[HELP_TMDB],
    },
    Step {
        id: StepId::Folders,
        key: "folders",
        stage: "Where it lives",
        title: "Library folders",
        question: "Where does your library go?",
        lead: "Add a folder for each kind of media you follow, such as /data/media/movies. Reelix checks it can write to each one.",
        optional: true,
        questions: &[],
        help: &[HELP_DATA],
    },
    Step {
        id: StepId::Torrent,
        key: "torrent",
        stage: "Downloading it",
        title: "Download apps: torrent",
        question: "Which torrent app downloads for you?",
        lead: "Reelix sends torrent releases to it and imports each one when it finishes.",
        optional: true,
        questions: &[Question {
            key: "torrent",
            legend: "Choose one",
            choices: &[
                recommended(
                    "qbittorrent",
                    "qBittorrent",
                    "The one most guides use. A web page with the fewest settings to change.",
                ),
                choice(
                    "transmission",
                    "Transmission",
                    "Small and light, with fewer options.",
                ),
                choice("deluge", "Deluge", "Adds features through plug-ins."),
                choice(
                    "rtorrent",
                    "rTorrent",
                    "For people who run it already, usually with ruTorrent.",
                ),
                choice("none", "No torrent app", "Usenet only, or neither yet."),
            ],
            multi: false,
            when: None,
        }],
        help: &[HELP_TORRENTS, HELP_VPN],
    },
    Step {
        id: StepId::Usenet,
        key: "usenet",
        stage: "Downloading it",
        title: "Download apps: Usenet",
        question: "Which Usenet app downloads for you?",
        lead: "Reelix sends Usenet releases to it and imports each one when it finishes. Your Usenet provider's details go into the app.",
        optional: true,
        questions: &[Question {
            key: "usenet",
            legend: "Choose one",
            choices: &[
                recommended(
                    "sabnzbd",
                    "SABnzbd",
                    "The one most guides use, with a set-up guide of its own. Needs a Usenet provider.",
                ),
                choice(
                    "nzbget",
                    "NZBGet",
                    "Uses very little memory, for small servers. Needs a Usenet provider.",
                ),
                choice("none", "No Usenet app", "Torrents only, or neither yet."),
            ],
            multi: false,
            when: None,
        }],
        help: &[HELP_USENET],
    },
    Step {
        id: StepId::DownloadOptions,
        key: "download-options",
        stage: "Downloading it",
        title: "Download apps: options",
        question: "Which should Reelix prefer?",
        lead: "When both your torrent app and your Usenet app could download a release, Reelix uses the one you prefer, and the other when it has nothing.",
        optional: true,
        questions: &[Question {
            key: "prefer",
            legend: "Choose one",
            choices: &[
                recommended(
                    "usenet",
                    "Prefer Usenet",
                    "Usenet downloads are usually faster and need no seeding. Torrents are used when Usenet has nothing.",
                ),
                choice(
                    "torrent",
                    "Prefer torrents",
                    "Usenet is used when no torrent is found.",
                ),
            ],
            multi: false,
            when: None,
        }],
        help: &[HELP_TORRENTS, HELP_USENET],
    },
    Step {
        id: StepId::Sites,
        key: "sites",
        stage: "Finding it",
        title: "Sites to search (Indexers)",
        question: "Choose the sites Reelix searches",
        lead: "Reelix tests each site. A site that needs an account asks for it.",
        optional: true,
        questions: &[],
        help: &[HELP_INDEXERS],
    },
    Step {
        id: StepId::Quality,
        key: "quality",
        stage: "What you want",
        title: "Quality",
        question: "What quality do you want?",
        lead: "New titles start with this. Each one can have its own quality later, on its page.",
        optional: true,
        questions: &[Question {
            key: "quality",
            legend: "Movies and series",
            choices: &[
                recommended(
                    "hd",
                    "HD 1080p",
                    "About 2 to 8 GB a movie. Plays well on most screens.",
                ),
                choice(
                    "uhd",
                    "4K",
                    "About 15 to 60 GB a movie. Needs a 4K screen and more space.",
                ),
                choice(
                    "small",
                    "Smaller files, 720p",
                    "About 1 to 3 GB a movie. For a small disk or slow internet.",
                ),
            ],
            multi: false,
            when: Some(watching),
        }],
        help: &[HELP_QUALITY],
    },
    Step {
        id: StepId::Monitor,
        key: "monitor",
        stage: "What you want",
        title: "What to get",
        question: "When should Reelix get a new movie?",
        lead: "Anything not found yet waits in Wanted, and Reelix searches again by itself.",
        optional: true,
        questions: &[Question {
            key: "movie_when",
            legend: "Movies",
            choices: &[
                recommended(
                    "released",
                    "When it is released to watch at home",
                    "The usual choice: releases before then are mostly poor copies.",
                ),
                choice(
                    "cinemas",
                    "When it is in cinemas",
                    "Reelix starts looking from the cinema date.",
                ),
                choice(
                    "announced",
                    "As soon as it is announced",
                    "Reelix looks from the start, and takes what it finds.",
                ),
            ],
            multi: false,
            when: None,
        }],
        help: &[],
    },
    Step {
        id: StepId::MediaServers,
        key: "media-servers",
        stage: "Watching it",
        title: "Media servers",
        question: "Where do you watch and listen?",
        lead: "After each import, Reelix tells each app, so new titles appear straight away. For Plex, use Sign in with Plex in the form.",
        optional: true,
        questions: &[],
        help: &[HELP_MEDIA_SERVER],
    },
    Step {
        id: StepId::Requests,
        key: "requests",
        stage: "Watching it",
        title: "Requests",
        question: "Who can ask for movies and series?",
        lead: "People sign in with their own accounts, or with their Jellyfin, Emby or Plex accounts.",
        optional: true,
        questions: &[Question {
            key: "requests",
            legend: "Choose one",
            choices: &[
                recommended(
                    "approve",
                    "People in my home, with my approval",
                    "Each request waits for you in Requests.",
                ),
                choice(
                    "auto",
                    "People in my home, approved automatically",
                    "Reelix searches straight away, within the limits in Settings.",
                ),
                choice("me", "Only me", "Nobody else can sign in yet."),
            ],
            multi: false,
            when: Some(watching),
        }],
        help: &[HELP_REQUESTS],
    },
    Step {
        id: StepId::Finish,
        key: "finish",
        stage: "Finish",
        title: "Reelix is ready",
        question: "Reelix is ready",
        lead: "Anything you skipped is in Settings, and the dashboard checklist shows what is still missing.",
        optional: false,
        questions: &[],
        help: &[],
    },
];

/// Asked on the torrent and Usenet steps when an app is chosen.
pub fn download_needs(step: StepId, answers: &Answers) -> Option<(&'static str, &'static [Provider])> {
    match step {
        StepId::Torrent if torrent_chosen(answers) => Some((
            "Privacy for torrents: other people in a torrent see your address. Many people run their torrent app through a VPN with a kill switch. Reelix does not set up the VPN.",
            VPN_PROVIDERS,
        )),
        StepId::Usenet if usenet_chosen(answers) => Some((
            "A Usenet app needs a Usenet provider: a paid account that stores the files. Its details go into the Usenet app, not into Reelix.",
            USENET_PROVIDERS,
        )),
        _ => None,
    }
}

/// The provider `implementation` ids (see the download client schemas) for an app answer.
pub fn download_client_for(answer: &str) -> Option<&'static str> {
    [
        "qbittorrent",
        "transmission",
        "deluge",
        "rtorrent",
        "sabnzbd",
        "nzbget",
    ]
    .into_iter()
    .find(|id| *id == answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_text() -> Vec<&'static str> {
        let mut out = vec![PLACEHOLDER_NOTE];
        for s in STEPS {
            out.extend([s.stage, s.title, s.question, s.lead]);
            for q in s.questions {
                out.push(q.legend);
                for c in q.choices {
                    out.extend([c.label, c.hint]);
                }
            }
            for h in s.help {
                out.push(h.term);
                out.extend(h.text);
            }
        }
        for c in FEEDBACK_KINDS {
            out.extend([c.label, c.hint]);
        }
        out
    }

    #[test]
    fn library_folders_follow_the_answers() {
        let mut a = Answers::with_defaults();
        a.set("follow", "movies,books");
        let keys: Vec<_> = library_folders(&a).iter().map(|f| f.key).collect();
        assert_eq!(keys, ["movie", "ebook", "audiobook"]);
        assert!(
            library_folders(&a)
                .iter()
                .all(|f| f.path.starts_with("/data/media/"))
        );
    }

    #[test]
    fn every_step_in_a_flow_has_one_definition() {
        for start in Start::ALL {
            let f = flow(start);
            assert_eq!(f.first(), Some(&StepId::Start));
            assert_eq!(f.last(), Some(&StepId::Finish));
            for id in f {
                assert_eq!(STEPS.iter().filter(|s| s.id == *id).count(), 1, "{id:?}");
            }
        }
        let mut keys: Vec<_> = STEPS.iter().map(|s| s.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), STEPS.len(), "step keys are unique");
    }

    #[test]
    fn recommended_answer_comes_first_and_only_once() {
        for q in STEPS.iter().flat_map(|s| s.questions) {
            let n = q.choices.iter().filter(|c| c.recommended).count();
            assert!(n <= 1, "{}", q.key);
            if n == 1 {
                assert!(q.choices[0].recommended, "{}: recommended first", q.key);
            }
        }
    }

    #[test]
    fn words_follow_the_style() {
        for t in all_text() {
            assert!(!t.contains('\u{2014}'), "no em dashes: {t}");
            for c in ["n't", "'re", "'ll", "'ve", "you'd", "it's", "\u{2019}t "] {
                assert!(!t.to_lowercase().contains(c), "no contractions: {t}");
            }
            assert!(!t.trim().is_empty());
        }
    }

    #[test]
    fn steps_follow_the_answers() {
        let ctx = Context::default();
        let mut a = Answers::with_defaults();
        assert!(!steps_shown(&a, ctx).contains(&StepId::Code));
        assert!(
            steps_shown(
                &a,
                Context {
                    code_required: true,
                    signed_in: false
                }
            )
            .contains(&StepId::Code)
        );

        a.set("follow", "books,music");
        let shown = steps_shown(&a, ctx);
        assert!(!shown.contains(&StepId::Tmdb), "no TMDB without movies or series");
        assert!(!shown.contains(&StepId::Monitor));

        a.set("usenet", "none");
        assert!(!steps_shown(&a, ctx).contains(&StepId::DownloadOptions));

        a.set("start", "restore");
        assert_eq!(
            steps_shown(&a, ctx),
            vec![StepId::Start, StepId::Restore, StepId::Finish]
        );

        a.set("start", "migrate");
        let shown = steps_shown(&a, ctx);
        assert!(shown.contains(&StepId::Migrate) && shown.contains(&StepId::Tmdb));
        assert!(!shown.contains(&StepId::Follow));
    }

    #[test]
    fn running_again_leaves_out_the_account_and_restore() {
        let again = Context {
            code_required: true,
            signed_in: true,
        };
        let shown = steps_shown(&Answers::with_defaults(), again);
        assert_eq!(shown[0], StepId::Start);
        assert!(!shown.contains(&StepId::Code) && !shown.contains(&StepId::Account));
        let start = &step(StepId::Start).questions[0];
        assert!(!offered(start, "restore", again));
        assert!(offered(start, "restore", Context::default()));
    }

    #[test]
    fn links_are_placeholders_until_chosen() {
        assert!(HELP_CENTRE.is_none());
        assert!(
            VPN_PROVIDERS
                .iter()
                .chain(USENET_PROVIDERS)
                .all(|p| p.link.is_none())
        );
    }

    #[test]
    fn answers_map_onto_reelix_settings() {
        assert_eq!(quality_profile_for("hd"), "HD-1080p");
        assert_eq!(quality_profile_for("uhd"), "Ultra-HD");
        assert_eq!(minimum_availability_for("cinemas"), "in_cinemas");
        assert_eq!(download_client_for("none"), None);
    }
}
