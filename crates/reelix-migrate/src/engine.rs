//! Turns a [`Snapshot`] into Reelix objects.
//!
//! One walk serves both the preview and the run: in [`Mode::Plan`] nothing is written and
//! objects that would be created get placeholder ids so dependants (a movie needing its root
//! folder and profile) are still planned; in [`Mode::Run`] each item is created as it's reached.
//! Items are matched to what Reelix already has (by path, name, external id or URL), so running
//! a migration twice doesn't duplicate anything.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use reelix_api_types::books::{AuthorListQuery, AuthorMonitor, BookImportItem, BookLibraryImportRequest};
use reelix_api_types::content::CreateContentRootFolderRequest;
use reelix_api_types::library::{
    AddMovieRequest, AddSeriesRequest, MinimumAvailability, MovieListQuery, MovieMonitor, NewItemMonitor,
    SeriesListQuery, SeriesMonitor, SeriesType,
};
use reelix_api_types::migrate::{MigrateAction, MigrateCategory, MigrateConnection, MigrateItem, SourceKind};
use reelix_api_types::music::{ArtistListQuery, ArtistMonitor, MusicImportItem, MusicLibraryImportRequest};
use reelix_api_types::providers::{ProviderInput, ProviderKind};
use reelix_api_types::requests::RequestStatus;
use reelix_core::{AuthSource, ContentKind, MediaKind, Permissions};
use reelix_db::NewUser;
use reelix_parser::QualityFamily;
use reelix_requests::repo::{NewRequest, RequestFilter, RequestRepo, UserAdminRepo, UserEdit};
use serde_json::{Map, Value, json};

use crate::source::{Provider, Snapshot};
use crate::{Targets, quality};

/// Page size used when listing what Reelix already has.
const PAGE: u32 = 500;

#[derive(Debug, Clone)]
pub(crate) enum Mode {
    Plan,
    Run {
        categories: Vec<MigrateCategory>,
        search_missing: bool,
    },
}

/// Reports progress: items done so far and what's being worked on.
pub(crate) type Progress<'a> = &'a (dyn Fn(usize, &str) + Send + Sync);

pub(crate) struct Engine<'a> {
    t: &'a Targets,
    snap: &'a Snapshot,
    conn: &'a MigrateConnection,
    mode: Mode,
    progress: Progress<'a>,
    pub items: Vec<MigrateItem>,
    pub notes: Vec<String>,
    done: usize,
    next_placeholder: i64,
    tags: HashMap<i64, i64>,
    /// Source root folder path (normalised, rewritten) → Reelix root folder id.
    roots: HashMap<String, i64>,
    profiles: HashMap<i64, i64>,
    /// Readarr: source profile id → family (ebook/audiobook).
    profile_family: HashMap<i64, QualityFamily>,
    users: HashMap<i64, i64>,
}

/// Normalises a path for comparison (separators, trailing slash).
fn norm(p: &str) -> String {
    let p = p.trim().replace('\\', "/");
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.to_owned()
    }
}

fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    if cfg!(windows) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn conflict(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("already") || e.contains("conflict") || e.contains("exists")
}

impl<'a> Engine<'a> {
    pub fn new(
        t: &'a Targets,
        snap: &'a Snapshot,
        conn: &'a MigrateConnection,
        mode: Mode,
        progress: Progress<'a>,
    ) -> Self {
        Self {
            t,
            snap,
            conn,
            mode,
            progress,
            items: Vec::new(),
            notes: Vec::new(),
            done: 0,
            next_placeholder: -1,
            tags: HashMap::new(),
            roots: HashMap::new(),
            profiles: HashMap::new(),
            profile_family: HashMap::new(),
            users: HashMap::new(),
        }
    }

    /// Number of items the walk will visit (for progress).
    pub fn total(snap: &Snapshot) -> usize {
        snap.tags.len()
            + snap.root_folders.len()
            + snap.quality_profiles.len()
            + snap.indexers.len()
            + snap.download_clients.len()
            + snap.remote_path_mappings.len()
            + snap.movies.len()
            + snap.series.len()
            + snap.authors.len()
            + snap.artists.len()
            + snap.users.len()
            + snap.requests.len()
    }

    fn running(&self) -> bool {
        matches!(self.mode, Mode::Run { .. })
    }

    /// Whether items of `c` are created (always true for a plan).
    fn creates(&self, c: MigrateCategory) -> bool {
        match &self.mode {
            Mode::Plan => true,
            Mode::Run { categories, .. } => categories.is_empty() || categories.contains(&c),
        }
    }

    fn search_missing(&self) -> bool {
        matches!(
            self.mode,
            Mode::Run {
                search_missing: true,
                ..
            }
        )
    }

    fn placeholder(&mut self) -> i64 {
        let id = self.next_placeholder;
        self.next_placeholder -= 1;
        id
    }

    fn tick(&mut self, what: &str) {
        self.done += 1;
        (self.progress)(self.done, what);
    }

    fn push(
        &mut self,
        key: String,
        category: MigrateCategory,
        name: &str,
        action: MigrateAction,
        detail: Option<String>,
    ) {
        if !self.creates(category) && action != MigrateAction::Existing {
            return;
        }
        self.items.push(MigrateItem {
            key,
            category,
            name: name.to_owned(),
            action,
            detail,
            missing_secrets: Vec::new(),
        });
    }

    fn skip(&mut self, key: String, c: MigrateCategory, name: &str, why: impl Into<String>) {
        self.push(key, c, name, MigrateAction::Skip, Some(why.into()));
    }

    /// Applies the user's path rewrites to a path from the source.
    fn rewrite(&self, path: &str) -> String {
        let p = norm(path);
        for r in &self.conn.path_rewrites {
            let from = norm(&r.from);
            if from.is_empty() {
                continue;
            }
            let matches = p == from || p.starts_with(&format!("{}/", from.trim_end_matches('/')));
            if matches {
                return format!(
                    "{}{}",
                    norm(&r.to).trim_end_matches('/'),
                    &p[from.trim_end_matches('/').len()..]
                );
            }
        }
        p
    }

    fn root_for(&self, root_folder_path: &str, item_path: &str) -> Option<i64> {
        let wanted = if root_folder_path.trim().is_empty() {
            // Older versions omit rootFolderPath: use the parent of the item's folder.
            Path::new(&norm(item_path))
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            root_folder_path.to_owned()
        };
        let wanted = self.rewrite(&wanted);
        self.roots
            .iter()
            .find(|(k, _)| same_path(k, &wanted))
            .map(|(_, v)| *v)
    }

    fn tag_ids(&self, src: &[i64]) -> Vec<i64> {
        src.iter()
            .filter_map(|t| self.tags.get(t))
            .copied()
            .filter(|t| *t > 0)
            .collect()
    }

    pub async fn run(&mut self) -> Result<(), String> {
        self.tags().await?;
        match self.snap.kind {
            SourceKind::Jellyseerr => {
                self.users_and_requests().await?;
                return Ok(());
            }
            SourceKind::Prowlarr => {
                self.indexers().await?;
                return Ok(());
            }
            _ => {}
        }
        self.quality_profiles().await?;
        self.root_folders().await?;
        self.indexers().await?;
        self.download_clients().await?;
        self.remote_path_mappings().await?;
        match self.snap.kind {
            SourceKind::Radarr => self.movies().await?,
            SourceKind::Sonarr => self.series().await?,
            SourceKind::Readarr => self.authors().await?,
            SourceKind::Lidarr => self.artists().await?,
            SourceKind::Prowlarr | SourceKind::Jellyseerr => {}
        }
        Ok(())
    }

    // ---------------------------------------------------------------- tags

    async fn tags(&mut self) -> Result<(), String> {
        let existing = self.t.providers.tags();
        for tag in &self.snap.tags {
            self.tick(&tag.label);
            let key = format!("tag:{}", tag.id);
            if let Some(e) = existing.iter().find(|e| e.label.eq_ignore_ascii_case(&tag.label)) {
                self.tags.insert(tag.id, e.id);
                self.push(
                    key,
                    MigrateCategory::Tag,
                    &tag.label,
                    MigrateAction::Existing,
                    None,
                );
                continue;
            }
            if !self.creates(MigrateCategory::Tag) {
                continue;
            }
            if self.running() {
                match self.t.providers.create_tag(&tag.label).await {
                    Ok(t) => {
                        self.tags.insert(tag.id, t.id);
                        self.push(key, MigrateCategory::Tag, &tag.label, MigrateAction::Create, None);
                    }
                    Err(e) => self.push(
                        key,
                        MigrateCategory::Tag,
                        &tag.label,
                        MigrateAction::Failed,
                        Some(err(e)),
                    ),
                }
            } else {
                let id = self.placeholder();
                self.tags.insert(tag.id, id);
                self.push(key, MigrateCategory::Tag, &tag.label, MigrateAction::Create, None);
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- quality profiles

    async fn quality_profiles(&mut self) -> Result<(), String> {
        let kind = self.snap.kind;
        let repo = self.t.db.quality_profiles();
        let mut existing_by_family: HashMap<QualityFamily, Vec<reelix_decision::QualityProfile>> =
            HashMap::new();
        for p in &self.snap.quality_profiles {
            self.tick(&p.name);
            let key = format!("quality_profile:{}", p.id);
            let family = quality::family(kind, p);
            self.profile_family.insert(p.id, family);
            if let std::collections::hash_map::Entry::Vacant(slot) = existing_by_family.entry(family) {
                slot.insert(repo.list_family(family).await.map_err(err)?);
            }
            let existing = &existing_by_family[&family];
            // The profile itself, or the one an earlier run made under the app's name because
            // another kind had the name: "Any (Lidarr)", or "Any (Lidarr) 2" when that was taken too.
            let renamed = renamed_profile(&p.name, kind.label());
            let found = existing
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(&p.name))
                .or_else(|| existing.iter().find(|e| is_renamed(&e.name, &renamed)));
            if let Some(e) = found {
                self.profiles.insert(p.id, e.id);
                self.push(
                    key,
                    MigrateCategory::QualityProfile,
                    &p.name,
                    MigrateAction::Existing,
                    Some(format!(
                        "Reelix's profile of this name is used; changes made to it in {} aren't copied",
                        kind.label()
                    )),
                );
                continue;
            }
            if !self.creates(MigrateCategory::QualityProfile) {
                continue;
            }
            let (mut profile, unmapped) = match quality::convert(kind, p) {
                Ok(v) => v,
                Err(why) => {
                    self.skip(key, MigrateCategory::QualityProfile, &p.name, why);
                    continue;
                }
            };
            let mut detail = Vec::new();
            // Profile names are unique across every kind (films and series, music, books): a
            // music profile "Any" from Lidarr can't share the name of Radarr's "Any".
            let names: Vec<String> = repo
                .list_all()
                .await
                .map_err(err)?
                .into_iter()
                .map(|e| e.name)
                .collect();
            let taken = |name: &str| names.iter().any(|n| n.eq_ignore_ascii_case(name));
            if taken(&profile.name) {
                // The new name may be taken as well: count up until one is free.
                profile.name = renamed.clone();
                let mut n = 2;
                while taken(&profile.name) {
                    profile.name = format!("{renamed} {n}");
                    n += 1;
                }
                detail.push(format!(
                    "named \"{}\": a profile of another kind already uses \"{}\"",
                    profile.name, p.name
                ));
            }
            if !unmapped.is_empty() {
                detail.push(format!("no Reelix equivalent for {}", unmapped.join(", ")));
            }
            if p.min_format_score != 0 || p.cutoff_format_score != 0 {
                detail.push("custom format scores aren't migrated".into());
            }
            let detail = (!detail.is_empty()).then(|| detail.join("; "));
            if self.running() {
                match repo.create(&profile).await {
                    Ok(created) => {
                        self.profiles.insert(p.id, created.id);
                        self.push(
                            key,
                            MigrateCategory::QualityProfile,
                            &p.name,
                            MigrateAction::Create,
                            detail,
                        );
                    }
                    Err(e) => self.push(
                        key,
                        MigrateCategory::QualityProfile,
                        &p.name,
                        MigrateAction::Failed,
                        Some(err(e)),
                    ),
                }
            } else {
                let id = self.placeholder();
                self.profiles.insert(p.id, id);
                self.push(
                    key,
                    MigrateCategory::QualityProfile,
                    &p.name,
                    MigrateAction::Create,
                    detail,
                );
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- root folders

    fn root_kind(&self, rf: &crate::source::RootFolder) -> ContentKind {
        match self.snap.kind {
            SourceKind::Radarr => ContentKind::Movie,
            SourceKind::Sonarr => ContentKind::Series,
            SourceKind::Lidarr => ContentKind::Music,
            _ => {
                let audio = rf
                    .default_quality_profile_id
                    .and_then(|id| self.profile_family.get(&id))
                    .is_some_and(|f| *f == QualityFamily::Audiobook);
                if audio {
                    ContentKind::Audiobook
                } else {
                    ContentKind::Ebook
                }
            }
        }
    }

    /// Every Reelix root folder as (id, path, kind).
    async fn reelix_roots(&self) -> Result<Vec<(i64, String, ContentKind)>, String> {
        let mut out = Vec::new();
        if let Some(lib) = &self.t.library {
            for r in lib.root_folders().await.map_err(err)? {
                let kind = match r.media_kind {
                    MediaKind::Movie => ContentKind::Movie,
                    MediaKind::Series => ContentKind::Series,
                };
                out.push((r.id, r.path, kind));
            }
        }
        if let Some(b) = &self.t.books {
            out.extend(
                b.root_folders()
                    .await
                    .map_err(err)?
                    .into_iter()
                    .map(|r| (r.id, r.path, r.kind)),
            );
        }
        if let Some(m) = &self.t.music {
            out.extend(
                m.root_folders()
                    .await
                    .map_err(err)?
                    .into_iter()
                    .map(|r| (r.id, r.path, r.kind)),
            );
        }
        Ok(out)
    }

    async fn root_folders(&mut self) -> Result<(), String> {
        let existing = self.reelix_roots().await?;
        for rf in &self.snap.root_folders {
            self.tick(&rf.path);
            let key = format!("root_folder:{}", rf.id);
            let path = self.rewrite(&rf.path);
            let kind = self.root_kind(rf);
            let label = if path == norm(&rf.path) {
                path.clone()
            } else {
                format!("{} → {path}", rf.path)
            };
            if let Some((id, _, k)) = existing.iter().find(|(_, p, _)| same_path(p, &path)) {
                if *k == kind {
                    self.roots.insert(path, *id);
                    self.push(
                        key,
                        MigrateCategory::RootFolder,
                        &label,
                        MigrateAction::Existing,
                        None,
                    );
                } else {
                    self.skip(
                        key,
                        MigrateCategory::RootFolder,
                        &label,
                        format!("Reelix already uses this folder for {}", k.as_str()),
                    );
                }
                continue;
            }
            if !self.creates(MigrateCategory::RootFolder) {
                continue;
            }
            if !Path::new(&path).is_dir() {
                self.skip(
                    key,
                    MigrateCategory::RootFolder,
                    &label,
                    "the folder doesn't exist on this server; add a path rewrite if Reelix sees it under another path",
                );
                continue;
            }
            if !self.running() {
                let id = self.placeholder();
                self.roots.insert(path, id);
                self.push(
                    key,
                    MigrateCategory::RootFolder,
                    &label,
                    MigrateAction::Create,
                    None,
                );
                continue;
            }
            let created = match kind {
                ContentKind::Movie | ContentKind::Series => match &self.t.library {
                    Some(lib) => lib
                        .add_root_folder(
                            &path,
                            if kind == ContentKind::Movie {
                                MediaKind::Movie
                            } else {
                                MediaKind::Series
                            },
                        )
                        .await
                        .map(|r| r.id)
                        .map_err(err),
                    None => Err("movies and series aren't available".into()),
                },
                ContentKind::Ebook | ContentKind::Audiobook => match &self.t.books {
                    Some(b) => b
                        .add_root_folder(CreateContentRootFolderRequest {
                            path: path.clone(),
                            kind,
                        })
                        .await
                        .map(|r| r.id)
                        .map_err(err),
                    None => Err("books aren't available".into()),
                },
                ContentKind::Music => match &self.t.music {
                    Some(m) => m
                        .add_root_folder(CreateContentRootFolderRequest {
                            path: path.clone(),
                            kind,
                        })
                        .await
                        .map(|r| r.id)
                        .map_err(err),
                    None => Err("music isn't available".into()),
                },
                // No *arr app has radio root folders.
                ContentKind::Radio => Err("radio root folders aren't migrated".into()),
            };
            match created {
                Ok(id) => {
                    self.roots.insert(path, id);
                    self.push(
                        key,
                        MigrateCategory::RootFolder,
                        &label,
                        MigrateAction::Create,
                        None,
                    );
                }
                Err(e) => self.push(
                    key,
                    MigrateCategory::RootFolder,
                    &label,
                    MigrateAction::Failed,
                    Some(e),
                ),
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- providers

    /// A secret field's value: the source's, unless hidden, then the user-supplied one.
    fn secret(&self, key: &str, field: &str, value: Option<String>) -> Result<Option<String>, ()> {
        match value {
            Some(v) if Provider::is_masked(&v) => self
                .conn
                .secrets
                .get(&format!("{key}:{field}"))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .map(Some)
                .ok_or(()),
            other => Ok(other),
        }
    }

    /// Creates, or plans, a provider; `missing` are hidden secrets without a value.
    #[allow(clippy::too_many_arguments)]
    async fn create_provider(
        &mut self,
        key: String,
        category: MigrateCategory,
        kind: ProviderKind,
        name: &str,
        input: ProviderInput,
        missing: Vec<String>,
        detail: Option<String>,
    ) {
        if !missing.is_empty() {
            self.items.push(MigrateItem {
                key,
                category,
                name: name.to_owned(),
                action: MigrateAction::Skip,
                detail: Some(format!(
                    "{} hides {}; enter {} to migrate it",
                    self.snap.kind.label(),
                    missing.join(" and "),
                    if missing.len() == 1 { "it" } else { "them" }
                )),
                missing_secrets: missing,
            });
            return;
        }
        if !self.running() {
            self.push(key, category, name, MigrateAction::Create, detail);
            return;
        }
        match self.t.providers.create(kind, &input).await {
            Ok(_) => self.push(key, category, name, MigrateAction::Create, detail),
            Err(e) => self.push(key, category, name, MigrateAction::Failed, Some(err(e))),
        }
    }

    /// Field names of a Reelix provider implementation (`None` if it doesn't exist).
    fn schema_fields(&self, kind: ProviderKind, implementation: &str) -> Option<BTreeSet<String>> {
        self.t
            .providers
            .schemas(kind)
            .into_iter()
            .find(|s| s.schema.implementation == implementation)
            .map(|s| s.schema.fields.into_iter().map(|f| f.name).collect())
    }

    async fn indexers(&mut self) -> Result<(), String> {
        let existing = self.t.providers.list(ProviderKind::Indexer).await.map_err(err)?;
        for ix in &self.snap.indexers {
            self.tick(&ix.name);
            let key = format!("indexer:{}", ix.id);
            let c = MigrateCategory::Indexer;
            if !self.creates(c) {
                continue;
            }
            let implementation = ix.implementation.to_ascii_lowercase();
            let mut settings = Map::new();
            let mut missing = Vec::new();
            let target = if implementation == "newznab" || implementation == "torznab" {
                let Some(base) = ix.str_field("baseUrl") else {
                    self.skip(key, c, &ix.name, "it has no URL");
                    continue;
                };
                // Indexers synced from Prowlarr point at Prowlarr (`…:9696/12/`); migrate
                // Prowlarr itself instead.
                if self.snap.kind != SourceKind::Prowlarr && looks_like_prowlarr(&base) {
                    self.skip(
                        key,
                        c,
                        &ix.name,
                        "it points at Prowlarr; migrate Prowlarr instead",
                    );
                    continue;
                }
                settings.insert("base_url".into(), json!(base.trim_end_matches('/')));
                if let Some(p) = ix.str_field("apiPath") {
                    settings.insert("api_path".into(), json!(p));
                }
                match self.secret(&key, "apiKey", ix.str_field("apiKey")) {
                    Ok(Some(k)) => {
                        settings.insert("api_key".into(), json!(k));
                    }
                    Ok(None) => {}
                    Err(()) => missing.push("apiKey".to_owned()),
                }
                if let Some(p) = ix.str_field("additionalParameters") {
                    settings.insert("additional_parameters".into(), json!(p));
                }
                let mut cats = ix.int_list_field("categories");
                for c in ix.int_list_field("animeCategories") {
                    if !cats.contains(&c) {
                        cats.push(c);
                    }
                }
                if !cats.is_empty() {
                    let field = match self.snap.kind {
                        SourceKind::Lidarr => "music_categories",
                        SourceKind::Readarr => "book_categories",
                        _ => "categories",
                    };
                    settings.insert(field.into(), json!(cats));
                }
                implementation.clone()
            } else if let Some(def) = ix.definition_name.as_deref().filter(|d| !d.is_empty()) {
                let implementation = format!(
                    "{}{}",
                    reelix_indexers::definition::IMPLEMENTATION_PREFIX,
                    def.to_ascii_lowercase()
                );
                let Some(fields) = self.schema_fields(ProviderKind::Indexer, &implementation) else {
                    self.skip(
                        key,
                        c,
                        &ix.name,
                        format!("Reelix has no definition for {def}; sync indexer definitions and try again"),
                    );
                    continue;
                };
                for f in &ix.fields {
                    let name = if f.name.eq_ignore_ascii_case("baseUrl") {
                        "base_url".to_owned()
                    } else {
                        f.name.clone()
                    };
                    if !fields.contains(&name) || f.value.is_null() {
                        continue;
                    }
                    if let Value::String(s) = &f.value
                        && Provider::is_masked(s)
                    {
                        match self.secret(&key, &f.name, Some(s.clone())) {
                            Ok(Some(v)) => {
                                settings.insert(name, json!(v));
                            }
                            _ => missing.push(f.name.clone()),
                        }
                        continue;
                    }
                    settings.insert(name, f.value.clone());
                }
                implementation
            } else {
                self.skip(
                    key,
                    c,
                    &ix.name,
                    format!("{} indexers aren't supported by Reelix yet", ix.implementation),
                );
                continue;
            };
            if ix.protocol.eq_ignore_ascii_case("torrent") {
                if let Some(v) = ix.int_field("minimumSeeders") {
                    settings.insert("minimum_seeders".into(), json!(v));
                }
                if let Some(Value::Number(n)) = ix.field("seedCriteria.seedRatio") {
                    settings.insert("seed_ratio".into(), json!(n));
                }
                if let Some(v) = ix.int_field("seedCriteria.seedTime") {
                    settings.insert("seed_time_minutes".into(), json!(v));
                }
            }
            // Radarr/Sonarr have per-indexer search toggles; Prowlarr only `enable`.
            if self.snap.kind != SourceKind::Prowlarr {
                settings.insert("enable_rss".into(), json!(ix.enable_rss));
                settings.insert(
                    "enable_automatic_search".into(),
                    json!(ix.enable_automatic_search),
                );
                settings.insert(
                    "enable_interactive_search".into(),
                    json!(ix.enable_interactive_search),
                );
            }
            // Same implementation and URL already in Reelix (e.g. migrated from another app).
            let same = existing.iter().find(|row| {
                row.implementation == target
                    && match (row.settings.get("base_url"), settings.get("base_url")) {
                        (Some(Value::String(a)), Some(Value::String(b))) => a
                            .trim_end_matches('/')
                            .eq_ignore_ascii_case(b.trim_end_matches('/')),
                        _ => row.name.eq_ignore_ascii_case(&ix.name),
                    }
            });
            if let Some(row) = same {
                let detail = self.merge_categories(row, &settings).await;
                self.push(key, c, &ix.name, MigrateAction::Existing, detail);
                continue;
            }
            let enabled = ix.enable
                || (self.snap.kind != SourceKind::Prowlarr
                    && (ix.enable_rss || ix.enable_automatic_search || ix.enable_interactive_search));
            let input = ProviderInput {
                implementation: target,
                name: ix.name.clone(),
                enabled,
                priority: if ix.priority > 0 { ix.priority } else { 25 },
                tags: self.tag_ids(&ix.tags),
                settings: Value::Object(settings),
            };
            self.create_provider(key, c, ProviderKind::Indexer, &ix.name, input, missing, None)
                .await;
        }
        Ok(())
    }

    /// Adds this app's categories to an indexer that already exists (e.g. the TV categories
    /// when migrating Sonarr after Radarr).
    async fn merge_categories(
        &self,
        row: &reelix_providers::ProviderRow,
        from: &Map<String, Value>,
    ) -> Option<String> {
        let mut settings = row.settings.clone();
        let mut changed = Vec::new();
        for field in [
            "categories",
            "book_categories",
            "audiobook_categories",
            "music_categories",
        ] {
            let Some(Value::Array(new)) = from.get(field) else {
                continue;
            };
            let mut current: Vec<Value> = match settings.get(field) {
                Some(Value::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            let before = current.len();
            for v in new {
                if !current.contains(v) {
                    current.push(v.clone());
                }
            }
            if current.len() != before {
                settings.insert(field.into(), Value::Array(current));
                changed.push(field.replace('_', " "));
            }
        }
        if changed.is_empty() {
            return None;
        }
        let detail = format!("adds {}", changed.join(", "));
        if !self.running() {
            return Some(detail);
        }
        let input = ProviderInput {
            implementation: row.implementation.clone(),
            name: row.name.clone(),
            enabled: row.enabled,
            priority: row.priority,
            tags: row.tags.clone(),
            settings: Value::Object(settings),
        };
        match self
            .t
            .providers
            .update(ProviderKind::Indexer, row.id, &input)
            .await
        {
            Ok(_) => Some(detail),
            Err(e) => Some(format!("couldn't add categories: {e}")),
        }
    }

    async fn download_clients(&mut self) -> Result<(), String> {
        let existing = self
            .t
            .providers
            .list(ProviderKind::DownloadClient)
            .await
            .map_err(err)?;
        let category_field = match self.snap.kind {
            SourceKind::Radarr => "movie_category",
            SourceKind::Sonarr => "tv_category",
            SourceKind::Readarr => "book_category",
            _ => "music_category",
        };
        for dc in &self.snap.download_clients {
            self.tick(&dc.name);
            let key = format!("download_client:{}", dc.id);
            let c = MigrateCategory::DownloadClient;
            if !self.creates(c) {
                continue;
            }
            let Some(implementation) = client_implementation(&dc.implementation) else {
                self.skip(
                    key,
                    c,
                    &dc.name,
                    format!("{} isn't supported by Reelix yet", dc.implementation),
                );
                continue;
            };
            let Some(fields) = self.schema_fields(ProviderKind::DownloadClient, implementation) else {
                self.skip(
                    key,
                    c,
                    &dc.name,
                    format!("{} isn't available in this build", dc.implementation),
                );
                continue;
            };
            let mut settings = Map::new();
            let mut missing = Vec::new();
            let mut set = |k: &str, v: Value| {
                if fields.contains(k) && !v.is_null() {
                    settings.insert(k.to_owned(), v);
                }
            };
            if let Some(h) = dc.str_field("host") {
                set("host", json!(h));
            }
            if let Some(p) = dc.int_field("port") {
                set("port", json!(p));
            }
            if let Some(b) = dc.bool_field("useSsl") {
                set("use_ssl", json!(b));
            }
            if let Some(u) = dc.str_field("urlBase") {
                set("url_base", json!(u));
            }
            if let Some(u) = dc.str_field("username") {
                set("username", json!(u));
            }
            for (src, dst) in [("password", "password"), ("apiKey", "api_key")] {
                match self.secret(&key, src, dc.str_field(src)) {
                    Ok(Some(v)) => set(dst, json!(v)),
                    Ok(None) => {}
                    Err(()) => missing.push(src.to_owned()),
                }
            }
            let category = [
                "movieCategory",
                "tvCategory",
                "bookCategory",
                "authorCategory",
                "musicCategory",
                "category",
            ]
            .iter()
            .find_map(|f| dc.str_field(f));
            if let Some(cat) = &category {
                set(category_field, json!(cat));
            }
            for (src, dst) in [
                ("movieDirectory", "directory"),
                ("tvDirectory", "directory"),
                ("musicDirectory", "directory"),
                ("bookDirectory", "directory"),
                ("torrentFolder", "drop_folder"),
                ("nzbFolder", "drop_folder"),
                ("watchFolder", "watch_folder"),
                ("magnetFileExtension", "magnet_extension"),
            ] {
                if let Some(v) = dc.str_field(src) {
                    let v = if dst.ends_with("folder") || dst == "directory" {
                        self.rewrite(&v)
                    } else {
                        v
                    };
                    set(dst, json!(v));
                }
            }
            for (src, dst) in [
                ("saveMagnetFiles", "save_magnet_files"),
                ("readOnly", "read_only"),
                ("sequentialOrder", "sequential"),
                ("firstAndLast", "first_last_piece"),
                ("addPaused", "add_paused"),
            ] {
                if let Some(v) = dc.bool_field(src) {
                    set(dst, json!(v));
                }
            }
            // qBittorrent/Transmission/Deluge `initialState` 1 = paused.
            if dc.int_field("initialState") == Some(1) {
                set("add_paused", json!(true));
            }
            let same = existing.iter().find(|row| {
                row.implementation == implementation
                    && ["host", "port", "drop_folder"].iter().all(|k| {
                        match (row.settings.get(*k), settings.get(*k)) {
                            (Some(a), Some(b)) => value_eq(a, b),
                            (None, None) => true,
                            _ => false,
                        }
                    })
            });
            if let Some(row) = same {
                // One Reelix client serves every content type: set this app's category on it.
                let detail = match &category {
                    Some(cat)
                        if row.settings.get(category_field).and_then(Value::as_str) != Some(cat.as_str()) =>
                    {
                        let mut s = row.settings.clone();
                        s.insert(category_field.into(), json!(cat));
                        let input = ProviderInput {
                            implementation: row.implementation.clone(),
                            name: row.name.clone(),
                            enabled: row.enabled,
                            priority: row.priority,
                            tags: row.tags.clone(),
                            settings: Value::Object(s),
                        };
                        let what = format!("sets its {} to {cat:?}", category_field.replace('_', " "));
                        if self.running() {
                            match self
                                .t
                                .providers
                                .update(ProviderKind::DownloadClient, row.id, &input)
                                .await
                            {
                                Ok(_) => Some(what),
                                Err(e) => Some(format!("couldn't set the category: {e}")),
                            }
                        } else {
                            Some(what)
                        }
                    }
                    _ => None,
                };
                self.push(key, c, &dc.name, MigrateAction::Existing, detail);
                continue;
            }
            let input = ProviderInput {
                implementation: implementation.to_owned(),
                name: dc.name.clone(),
                enabled: dc.enable,
                priority: if dc.priority > 0 { dc.priority } else { 1 },
                tags: self.tag_ids(&dc.tags),
                settings: Value::Object(settings),
            };
            self.create_provider(
                key,
                c,
                ProviderKind::DownloadClient,
                &dc.name,
                input,
                missing,
                None,
            )
            .await;
        }
        Ok(())
    }

    async fn remote_path_mappings(&mut self) -> Result<(), String> {
        let existing = self.t.providers.remote_path_mappings();
        for m in &self.snap.remote_path_mappings {
            let name = format!("{}: {} → {}", m.host, m.remote_path, m.local_path);
            self.tick(&name);
            let key = format!("remote_path_mapping:{}:{}", m.host, m.remote_path);
            let c = MigrateCategory::RemotePathMapping;
            let local = self.rewrite(&m.local_path);
            if existing
                .iter()
                .any(|e| e.host.eq_ignore_ascii_case(&m.host) && same_path(&e.remote_path, &m.remote_path))
            {
                self.push(key, c, &name, MigrateAction::Existing, None);
                continue;
            }
            if !self.creates(c) {
                continue;
            }
            if !self.running() {
                self.push(key, c, &name, MigrateAction::Create, None);
                continue;
            }
            match self
                .t
                .providers
                .create_mapping(&m.host, &m.remote_path, &local)
                .await
            {
                Ok(_) => self.push(key, c, &name, MigrateAction::Create, None),
                Err(e) => self.push(key, c, &name, MigrateAction::Failed, Some(err(e))),
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- library

    fn needs_tmdb(&mut self) -> bool {
        if (self.t.metadata_ready)() {
            return false;
        }
        let note =
            "TMDB isn't configured (Settings → Metadata), so movies, series and requests can't be added yet.";
        if !self.notes.iter().any(|n| n == note) {
            self.notes.push(note.into());
        }
        true
    }

    async fn movies(&mut self) -> Result<(), String> {
        let c = MigrateCategory::Movie;
        let Some(lib) = self.t.library.clone() else {
            return Ok(());
        };
        let mut have = BTreeSet::new();
        let mut page = 1;
        loop {
            let p = lib
                .list_movies(&MovieListQuery {
                    page: Some(page),
                    page_size: Some(PAGE),
                    ..Default::default()
                })
                .await
                .map_err(err)?;
            let n = p.items.len();
            have.extend(p.items.into_iter().map(|m| m.tmdb_id));
            if n < PAGE as usize || u64::from(page) * u64::from(PAGE) >= p.total {
                break;
            }
            page += 1;
        }
        let no_tmdb = self.needs_tmdb();
        for m in &self.snap.movies {
            let name = if m.year > 0 {
                format!("{} ({})", m.title, m.year)
            } else {
                m.title.clone()
            };
            self.tick(&name);
            let key = format!("movie:{}", m.id);
            if have.contains(&m.tmdb_id) {
                self.push(key, c, &name, MigrateAction::Existing, None);
                continue;
            }
            if !self.creates(c) {
                continue;
            }
            if m.tmdb_id == 0 {
                self.skip(key, c, &name, "it has no TMDB id");
                continue;
            }
            if no_tmdb {
                self.skip(key, c, &name, "TMDB isn't configured");
                continue;
            }
            let Some(root) = self.root_for(&m.root_folder_path, &m.path) else {
                self.skip(key, c, &name, "its root folder wasn't migrated");
                continue;
            };
            let Some(&profile) = self.profiles.get(&m.quality_profile_id) else {
                self.skip(key, c, &name, "its quality profile wasn't migrated");
                continue;
            };
            if !self.running() {
                self.push(key, c, &name, MigrateAction::Create, None);
                continue;
            }
            let req = AddMovieRequest {
                tmdb_id: m.tmdb_id,
                quality_profile_id: profile,
                root_folder_id: root,
                monitor: if m.monitored {
                    MovieMonitor::MovieOnly
                } else {
                    MovieMonitor::None
                },
                minimum_availability: match m.minimum_availability.to_ascii_lowercase().as_str() {
                    "tba" | "announced" => MinimumAvailability::Announced,
                    "incinemas" => MinimumAvailability::InCinemas,
                    _ => MinimumAvailability::Released,
                },
                tags: self.tag_ids(&m.tags),
                search_on_add: self.search_missing() && m.monitored && !m.has_file,
                path: Some(self.rewrite(&m.path)).filter(|p| p != "/"),
                added_by_request_id: None,
            };
            match lib.add_movie(req).await {
                Ok(_) => self.push(key, c, &name, MigrateAction::Create, None),
                Err(e) if conflict(&e.to_string()) => self.push(key, c, &name, MigrateAction::Existing, None),
                Err(e) => self.push(key, c, &name, MigrateAction::Failed, Some(err(e))),
            }
        }
        Ok(())
    }

    async fn series(&mut self) -> Result<(), String> {
        let c = MigrateCategory::Series;
        let Some(lib) = self.t.library.clone() else {
            return Ok(());
        };
        let (mut have_tmdb, mut have_tvdb) = (BTreeSet::new(), BTreeSet::new());
        let mut page = 1;
        loop {
            let p = lib
                .list_series(&SeriesListQuery {
                    page: Some(page),
                    page_size: Some(PAGE),
                    ..Default::default()
                })
                .await
                .map_err(err)?;
            let n = p.items.len();
            for s in p.items {
                if let Some(t) = s.tmdb_id {
                    have_tmdb.insert(t);
                }
                if let Some(t) = s.tvdb_id {
                    have_tvdb.insert(t);
                }
            }
            if n < PAGE as usize || u64::from(page) * u64::from(PAGE) >= p.total {
                break;
            }
            page += 1;
        }
        let no_tmdb = self.needs_tmdb();
        for s in &self.snap.series {
            self.tick(&s.title);
            let key = format!("series:{}", s.id);
            let name = s.title.clone();
            if (s.tmdb_id > 0 && have_tmdb.contains(&s.tmdb_id))
                || (s.tvdb_id > 0 && have_tvdb.contains(&s.tvdb_id))
            {
                self.push(key, c, &name, MigrateAction::Existing, None);
                continue;
            }
            if !self.creates(c) {
                continue;
            }
            if s.tmdb_id == 0 && s.tvdb_id == 0 {
                self.skip(key, c, &name, "it has no TMDB or TVDB id");
                continue;
            }
            if no_tmdb {
                self.skip(key, c, &name, "TMDB isn't configured");
                continue;
            }
            let Some(root) = self.root_for(&s.root_folder_path, &s.path) else {
                self.skip(key, c, &name, "its root folder wasn't migrated");
                continue;
            };
            let Some(&profile) = self.profiles.get(&s.quality_profile_id) else {
                self.skip(key, c, &name, "its quality profile wasn't migrated");
                continue;
            };
            if !self.running() {
                self.push(key, c, &name, MigrateAction::Create, None);
                continue;
            }
            let seasons: Vec<u32> = s
                .seasons
                .iter()
                .filter(|x| x.monitored)
                .map(|x| x.season_number)
                .collect();
            let req = AddSeriesRequest {
                tmdb_id: (s.tmdb_id > 0).then_some(s.tmdb_id),
                tvdb_id: (s.tvdb_id > 0).then_some(s.tvdb_id),
                quality_profile_id: profile,
                root_folder_id: root,
                monitor: if s.monitored && !seasons.is_empty() {
                    SeriesMonitor::All
                } else {
                    SeriesMonitor::None
                },
                seasons: (s.monitored && !seasons.is_empty()).then_some(seasons),
                series_type: match s.series_type.to_ascii_lowercase().as_str() {
                    "anime" => SeriesType::Anime,
                    "daily" => SeriesType::Daily,
                    _ => SeriesType::Standard,
                },
                season_folder: s.season_folder,
                monitor_new_items: match s.monitor_new_items.as_deref() {
                    Some("none") => NewItemMonitor::None,
                    _ => NewItemMonitor::All,
                },
                tags: self.tag_ids(&s.tags),
                search_on_add: self.search_missing() && s.monitored,
                search_cutoff_unmet: false,
                path: Some(self.rewrite(&s.path)).filter(|p| p != "/"),
                added_by_request_id: None,
            };
            match lib.add_series(req).await {
                Ok(_) => self.push(key, c, &name, MigrateAction::Create, None),
                Err(e) if conflict(&e.to_string()) => self.push(key, c, &name, MigrateAction::Existing, None),
                Err(e) => self.push(key, c, &name, MigrateAction::Failed, Some(err(e))),
            }
        }
        Ok(())
    }

    async fn authors(&mut self) -> Result<(), String> {
        let c = MigrateCategory::Author;
        let Some(books) = self.t.books.clone() else {
            return Ok(());
        };
        let mut have = BTreeSet::new();
        let mut page = 1;
        loop {
            let p = books
                .list_authors(&AuthorListQuery {
                    page: Some(page),
                    page_size: Some(PAGE),
                    ..Default::default()
                })
                .await
                .map_err(err)?;
            let n = p.items.len();
            have.extend(p.items.into_iter().map(|a| simplify(&a.name)));
            if n < PAGE as usize || u64::from(page) * u64::from(PAGE) >= p.total {
                break;
            }
            page += 1;
        }
        for a in &self.snap.authors {
            self.tick(&a.author_name);
            let key = format!("author:{}", a.id);
            let name = a.author_name.clone();
            if have.contains(&simplify(&name)) {
                self.push(key, c, &name, MigrateAction::Existing, None);
                continue;
            }
            if !self.creates(c) {
                continue;
            }
            let Some(root) = self.root_for(&a.root_folder_path, &a.path) else {
                self.skip(key, c, &name, "its root folder wasn't migrated");
                continue;
            };
            let Some(&profile) = self.profiles.get(&a.quality_profile_id) else {
                self.skip(key, c, &name, "its quality profile wasn't migrated");
                continue;
            };
            if !self.running() {
                self.push(
                    key,
                    c,
                    &name,
                    MigrateAction::Create,
                    Some("matched on Open Library by name when migrating".into()),
                );
                continue;
            }
            // Readarr ids are Goodreads ids; find the author on Open Library by name.
            let found = match books.lookup_authors(&name).await {
                Ok(list) => list.into_iter().find(|l| simplify(&l.name) == simplify(&name)),
                Err(e) => {
                    self.push(
                        key,
                        c,
                        &name,
                        MigrateAction::Failed,
                        Some(format!("Open Library lookup failed: {e}")),
                    );
                    continue;
                }
            };
            let Some(found) = found else {
                self.skip(key, c, &name, "no author with this name on Open Library");
                continue;
            };
            let audio = self.profile_family.get(&a.quality_profile_id) == Some(&QualityFamily::Audiobook);
            let item = BookImportItem {
                path: self.rewrite(&a.path),
                openlibrary_id: found.openlibrary_id,
                quality_profile_id: (!audio).then_some(profile),
                audio_quality_profile_id: audio.then_some(profile),
                monitor: if a.monitored {
                    AuthorMonitor::All
                } else {
                    AuthorMonitor::None
                },
                tags: self.tag_ids(&a.tags),
            };
            let res = books
                .library_import(BookLibraryImportRequest {
                    root_folder_id: root,
                    items: vec![item],
                })
                .await;
            self.import_outcome(key, c, &name, res.map_err(err));
        }
        Ok(())
    }

    async fn artists(&mut self) -> Result<(), String> {
        let c = MigrateCategory::Artist;
        let Some(music) = self.t.music.clone() else {
            return Ok(());
        };
        let mut have = BTreeSet::new();
        let mut page = 1;
        loop {
            let p = music
                .list_artists(&ArtistListQuery {
                    page: Some(page),
                    page_size: Some(PAGE),
                    ..Default::default()
                })
                .await
                .map_err(err)?;
            let n = p.items.len();
            have.extend(p.items.into_iter().map(|a| a.musicbrainz_id.to_ascii_lowercase()));
            if n < PAGE as usize || u64::from(page) * u64::from(PAGE) >= p.total {
                break;
            }
            page += 1;
        }
        let meta_profiles = music.metadata_profiles().await.map_err(err)?;
        let default_meta = meta_profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("standard"))
            .or(meta_profiles.first())
            .map(|p| p.id);
        for a in &self.snap.artists {
            self.tick(&a.artist_name);
            let key = format!("artist:{}", a.id);
            let name = a.artist_name.clone();
            if have.contains(&a.foreign_artist_id.to_ascii_lowercase()) {
                self.push(key, c, &name, MigrateAction::Existing, None);
                continue;
            }
            if !self.creates(c) {
                continue;
            }
            if a.foreign_artist_id.is_empty() {
                self.skip(key, c, &name, "it has no MusicBrainz id");
                continue;
            }
            let Some(root) = self.root_for(&a.root_folder_path, &a.path) else {
                self.skip(key, c, &name, "its root folder wasn't migrated");
                continue;
            };
            let Some(&profile) = self.profiles.get(&a.quality_profile_id) else {
                self.skip(key, c, &name, "its quality profile wasn't migrated");
                continue;
            };
            // Metadata profiles match by name; otherwise Reelix's default.
            let source_meta = self
                .snap
                .metadata_profiles
                .iter()
                .find(|m| m.id == a.metadata_profile_id)
                .map(|m| m.name.clone());
            let meta = source_meta
                .and_then(|n| {
                    meta_profiles
                        .iter()
                        .find(|p| p.name.eq_ignore_ascii_case(&n))
                        .map(|p| p.id)
                })
                .or(default_meta);
            let Some(meta) = meta else {
                self.skip(key, c, &name, "Reelix has no metadata profile");
                continue;
            };
            if !self.running() {
                self.push(key, c, &name, MigrateAction::Create, None);
                continue;
            }
            let item = MusicImportItem {
                path: self.rewrite(&a.path),
                musicbrainz_id: a.foreign_artist_id.clone(),
                quality_profile_id: profile,
                metadata_profile_id: meta,
                monitor: if a.monitored {
                    ArtistMonitor::All
                } else {
                    ArtistMonitor::None
                },
                tags: self.tag_ids(&a.tags),
            };
            let res = music
                .library_import(MusicLibraryImportRequest {
                    root_folder_id: root,
                    items: vec![item],
                })
                .await;
            self.import_outcome(key, c, &name, res.map_err(err));
        }
        Ok(())
    }

    fn import_outcome(
        &mut self,
        key: String,
        c: MigrateCategory,
        name: &str,
        res: Result<reelix_api_types::library::LibraryImportResult, String>,
    ) {
        match res {
            Ok(r) if !r.added.is_empty() => self.push(key, c, name, MigrateAction::Create, None),
            Ok(r) => {
                let why = r
                    .errors
                    .into_iter()
                    .map(|e| e.message)
                    .collect::<Vec<_>>()
                    .join("; ");
                if conflict(&why) {
                    self.push(key, c, name, MigrateAction::Existing, None);
                } else {
                    self.push(key, c, name, MigrateAction::Failed, Some(why));
                }
            }
            Err(e) => self.push(key, c, name, MigrateAction::Failed, Some(e)),
        }
    }

    // ---------------------------------------------------------------- Jellyseerr

    async fn users_and_requests(&mut self) -> Result<(), String> {
        let admin_repo = UserAdminRepo(&self.t.db);
        let users = admin_repo.list().await.map_err(err)?;
        for u in &self.snap.users {
            let Some(login) = u.login_name() else {
                self.tick("user");
                continue;
            };
            self.tick(&login);
            let key = format!("user:{}", u.id);
            let c = MigrateCategory::User;
            let (source, external) = match u.user_type {
                1 => (AuthSource::Plex, u.plex_id.map(|i| i.to_string())),
                3 => (AuthSource::Jellyfin, u.jellyfin_user_id.clone()),
                4 => (AuthSource::Emby, u.jellyfin_user_id.clone()),
                _ => (AuthSource::Local, None),
            };
            let found = match &external {
                Some(ext) => admin_repo
                    .by_external(source.as_str(), ext)
                    .await
                    .map_err(err)?
                    .map(|r| r.id),
                None => None,
            }
            .or_else(|| {
                users
                    .iter()
                    .find(|r| r.username.eq_ignore_ascii_case(&login))
                    .map(|r| r.id)
            });
            if let Some(id) = found {
                self.users.insert(u.id, id);
                self.push(key, c, &login, MigrateAction::Existing, None);
                continue;
            }
            if !self.creates(c) {
                continue;
            }
            let detail = (source == AuthSource::Local)
                .then(|| "local account: set a password for it under Settings → Users".to_owned());
            if !self.running() {
                let id = self.placeholder();
                self.users.insert(u.id, id);
                self.push(key, c, &login, MigrateAction::Create, detail);
                continue;
            }
            let permissions = seerr_permissions(u.permissions);
            let created = self
                .t
                .db
                .users()
                .create(NewUser {
                    username: login.clone(),
                    email: u.email.clone().filter(|e| !e.trim().is_empty()),
                    display_name: u.display_name.clone().filter(|d| !d.trim().is_empty()),
                    password_hash: None,
                    auth_source: source,
                    permissions,
                })
                .await;
            let user = match created {
                Ok(user) => user,
                Err(e) => {
                    self.push(key, c, &login, MigrateAction::Failed, Some(err(e)));
                    continue;
                }
            };
            if let Some(ext) = &external
                && let Err(e) = admin_repo
                    .link_external(user.id, ext, u.email.as_deref(), None)
                    .await
            {
                self.push(
                    key,
                    c,
                    &login,
                    MigrateAction::Failed,
                    Some(format!("created, but linking failed: {e}")),
                );
                continue;
            }
            let quota = |v: Option<i64>| v.filter(|n| *n > 0).and_then(|n| u32::try_from(n).ok());
            if [u.movie_quota_limit, u.tv_quota_limit]
                .iter()
                .any(|q| quota(*q).is_some())
            {
                let edit = UserEdit {
                    display_name: u.display_name.clone(),
                    email: u.email.clone(),
                    avatar: None,
                    locale: None,
                    permissions,
                    movie_quota_limit: quota(u.movie_quota_limit),
                    movie_quota_days: quota(u.movie_quota_days),
                    tv_quota_limit: quota(u.tv_quota_limit),
                    tv_quota_days: quota(u.tv_quota_days),
                    book_quota_limit: None,
                    book_quota_days: None,
                    music_quota_limit: None,
                    music_quota_days: None,
                };
                let _ = admin_repo.update(user.id, &edit).await;
            }
            self.users.insert(u.id, user.id);
            self.push(key, c, &login, MigrateAction::Create, detail);
        }
        self.requests().await
    }

    async fn requests(&mut self) -> Result<(), String> {
        let c = MigrateCategory::Request;
        let no_tmdb = self.needs_tmdb();
        let repo = RequestRepo(&self.t.db);
        for r in &self.snap.requests {
            let kind = if r.kind.eq_ignore_ascii_case("tv") || r.media.media_type.eq_ignore_ascii_case("tv") {
                MediaKind::Series
            } else {
                MediaKind::Movie
            };
            let Some(tmdb_id) = r.media.tmdb_id.filter(|t| *t > 0) else {
                self.tick("request");
                continue;
            };
            let who = r
                .requested_by
                .as_ref()
                .and_then(|u| u.login_name())
                .unwrap_or_else(|| "unknown user".into());
            let name = format!(
                "{} {tmdb_id} by {who}{}",
                if kind == MediaKind::Movie {
                    "Movie"
                } else {
                    "Series"
                },
                if r.is4k { " (4K)" } else { "" }
            );
            self.tick(&name);
            let key = format!("request:{}", r.id);
            if !self.creates(c) {
                continue;
            }
            let Some(&user_id) = r.requested_by.as_ref().and_then(|u| self.users.get(&u.id)) else {
                self.skip(key, c, &name, "its user wasn't migrated");
                continue;
            };
            if user_id > 0 {
                let existing = repo
                    .find(&RequestFilter {
                        kind: Some(kind),
                        tmdb_ids: vec![tmdb_id],
                        is_4k: Some(r.is4k),
                        user_id: Some(user_id),
                        ..Default::default()
                    })
                    .await
                    .map_err(err)?;
                if !existing.is_empty() {
                    self.push(key, c, &name, MigrateAction::Existing, None);
                    continue;
                }
            }
            if no_tmdb {
                self.skip(key, c, &name, "TMDB isn't configured");
                continue;
            }
            if !self.running() {
                self.push(key, c, &name, MigrateAction::Create, None);
                continue;
            }
            let meta = match kind {
                MediaKind::Movie => self.t.metadata.movie(tmdb_id).await.map(|m| {
                    (
                        m.title,
                        m.year,
                        m.imdb_id,
                        None,
                        m.images.poster,
                        m.images.backdrop,
                    )
                }),
                MediaKind::Series => self
                    .t
                    .metadata
                    .series(reelix_metadata::SeriesExternalId::Tmdb(tmdb_id))
                    .await
                    .map(|s| {
                        (
                            s.title,
                            s.year,
                            s.imdb_id,
                            s.tvdb_id,
                            s.images.poster,
                            s.images.backdrop,
                        )
                    }),
            };
            let (title, year, imdb_id, tvdb_id, poster, backdrop) = match meta {
                Ok(m) => m,
                Err(e) => {
                    self.push(key, c, &name, MigrateAction::Failed, Some(format!("TMDB: {e}")));
                    continue;
                }
            };
            let label = format!("{title} by {who}{}", if r.is4k { " (4K)" } else { "" });
            let created = repo
                .create(NewRequest {
                    user_id,
                    kind,
                    tmdb_id,
                    tvdb_id: tvdb_id.or(r.media.tvdb_id),
                    imdb_id,
                    title,
                    year,
                    poster,
                    backdrop,
                    seasons: r.seasons.iter().map(|s| s.season_number).collect(),
                    is_4k: r.is4k,
                    profile_id: None,
                    root_folder_id: None,
                    tags: Vec::new(),
                    source: "manual",
                })
                .await;
            let id = match created {
                Ok(id) => id,
                Err(e) => {
                    self.push(key, c, &label, MigrateAction::Failed, Some(err(e)));
                    continue;
                }
            };
            if let Err(e) = self.finish_request(id, r, kind, tmdb_id).await {
                self.push(
                    key,
                    c,
                    &label,
                    MigrateAction::Failed,
                    Some(format!("created, but {e}")),
                );
                continue;
            }
            self.push(key, c, &label, MigrateAction::Create, None);
        }
        Ok(())
    }

    /// Copies the status, dates and library link of a migrated request.
    async fn finish_request(
        &self,
        id: i64,
        r: &crate::source::SeerrRequest,
        kind: MediaKind,
        tmdb_id: u32,
    ) -> Result<(), String> {
        let repo = RequestRepo(&self.t.db);
        let Some(mut row) = repo.get(id).await.map_err(err)? else {
            return Err("the request disappeared".into());
        };
        row.status = match (r.status, r.media.status) {
            (1, _) => RequestStatus::Pending,
            (3, _) => RequestStatus::Declined,
            (4, _) => RequestStatus::Failed,
            (5, _) | (_, 5) => RequestStatus::Available,
            (_, 4) => RequestStatus::PartiallyAvailable,
            (_, 3) => RequestStatus::Processing,
            _ => RequestStatus::Approved,
        };
        let created = r
            .created_at
            .as_deref()
            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
            .map(|d| d.timestamp_millis());
        if !matches!(row.status, RequestStatus::Pending | RequestStatus::Declined) {
            row.approved_at = Some(created.unwrap_or(row.created_at));
        }
        if let Some(lib) = &self.t.library {
            match kind {
                MediaKind::Movie => {
                    row.movie_id = lib.movie_by_tmdb(tmdb_id).await.ok().flatten().map(|m| m.id)
                }
                MediaKind::Series => {
                    row.series_id = lib.series_by_tmdb(tmdb_id).await.ok().flatten().map(|s| s.id)
                }
            }
        }
        repo.save(&row).await.map_err(err)?;
        if let Some(ms) = created {
            sqlx::query("UPDATE requests SET created_at = ? WHERE id = ?")
                .bind(ms)
                .bind(id)
                .execute(self.t.db.pool())
                .await
                .map_err(err)?;
        }
        Ok(())
    }
}

/// Lowercased letters and digits only, for matching names across sources.
fn simplify(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.trim().eq_ignore_ascii_case(y.trim()),
        (Value::Number(x), Value::String(y)) | (Value::String(y), Value::Number(x)) => {
            x.to_string() == y.trim()
        }
        _ => a == b,
    }
}

/// `http://host:9696/12/` or `…/prowlarr/12/api`: a per-indexer Prowlarr proxy.
fn looks_like_prowlarr(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let segments: Vec<&str> = u
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let numeric_last = segments
        .iter()
        .rev()
        .find(|s| **s != "api")
        .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()));
    numeric_last && (u.port() == Some(9696) || segments.iter().any(|s| s.eq_ignore_ascii_case("prowlarr")))
}

fn client_implementation(arr: &str) -> Option<&'static str> {
    Some(match arr.to_ascii_lowercase().as_str() {
        "sabnzbd" => "sabnzbd",
        "nzbget" => "nzbget",
        "qbittorrent" => "qbittorrent",
        "transmission" => "transmission",
        "deluge" => "deluge",
        "rtorrent" => "rtorrent",
        "torrentblackhole" => "torrent_blackhole",
        "usenetblackhole" => "usenet_blackhole",
        _ => return None,
    })
}

/// Overseerr/Jellyseerr permission bits → Reelix permissions.
pub(crate) fn seerr_permissions(bits: u64) -> Permissions {
    const MAP: &[(u64, Permissions)] = &[
        (2, Permissions::ADMIN),
        (8, Permissions::MANAGE_USERS),
        (16, Permissions::MANAGE_REQUESTS.union(Permissions::VIEW_REQUESTS)),
        (32, Permissions::REQUEST_MOVIE.union(Permissions::REQUEST_TV)),
        (
            128,
            Permissions::AUTO_APPROVE_MOVIE.union(Permissions::AUTO_APPROVE_TV),
        ),
        (256, Permissions::AUTO_APPROVE_MOVIE),
        (512, Permissions::AUTO_APPROVE_TV),
        (1024, Permissions::REQUEST_4K),
        (2048, Permissions::REQUEST_4K),
        (4096, Permissions::REQUEST_4K),
        (8192, Permissions::REQUEST_ADVANCED),
        (16384, Permissions::VIEW_REQUESTS),
        (32768, Permissions::AUTO_APPROVE_4K),
        (65536, Permissions::AUTO_APPROVE_4K),
        (131072, Permissions::AUTO_APPROVE_4K),
        (262144, Permissions::REQUEST_MOVIE),
        (524288, Permissions::REQUEST_TV),
        (1048576, Permissions::MANAGE_ISSUES),
        (4194304, Permissions::CREATE_ISSUES),
    ];
    let mut p = Permissions::empty();
    for (bit, perm) in MAP {
        if bits & bit != 0 {
            p |= *perm;
        }
    }
    if p.is_empty() { Permissions::ROLE_USER } else { p }
}

/// The name a profile gets when another kind already uses its own: "Any (Lidarr)".
fn renamed_profile(name: &str, app: &str) -> String {
    format!("{name} ({app})")
}

/// Whether `name` is `renamed`, or `renamed` with a number after it ("Any (Lidarr) 2").
fn is_renamed(name: &str, renamed: &str) -> bool {
    if name.eq_ignore_ascii_case(renamed) {
        return true;
    }
    name.get(..renamed.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(renamed))
        && name[renamed.len()..]
            .strip_prefix(' ')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_profiles_are_recognised() {
        let r = renamed_profile("Any", "Lidarr");
        assert_eq!(r, "Any (Lidarr)");
        assert!(is_renamed("Any (Lidarr)", &r));
        assert!(is_renamed("any (lidarr) 3", &r));
        assert!(!is_renamed("Any (Lidarr) x", &r));
        assert!(!is_renamed("Any (Lidarr) ", &r));
        assert!(!is_renamed("Any", &r));
    }

    #[test]
    fn detects_prowlarr_proxies() {
        assert!(looks_like_prowlarr("http://prowlarr:9696/12/"));
        assert!(looks_like_prowlarr("http://10.0.0.2:9696/3/api"));
        assert!(looks_like_prowlarr("https://example.com/prowlarr/7"));
        assert!(!looks_like_prowlarr("https://api.nzbgeek.info"));
        assert!(!looks_like_prowlarr(
            "http://jackett:9117/api/v2.0/indexers/x/results/torznab/"
        ));
    }

    #[test]
    fn maps_seerr_permissions() {
        assert_eq!(seerr_permissions(2), Permissions::ADMIN);
        let p = seerr_permissions(32 | 1024);
        assert!(p.contains(Permissions::REQUEST_MOVIE | Permissions::REQUEST_TV | Permissions::REQUEST_4K));
        assert!(!p.contains(Permissions::ADMIN));
        assert_eq!(seerr_permissions(0), Permissions::ROLE_USER);
    }

    #[test]
    fn normalises_paths() {
        assert_eq!(norm("/movies/"), "/movies");
        assert_eq!(norm("C:\\Media\\Movies\\"), "C:/Media/Movies");
        assert!(same_path("/tv", "/tv/"));
    }
}
