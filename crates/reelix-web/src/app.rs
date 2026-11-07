//! Root component: contexts, boot sequence and routes.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{ParentRoute, Redirect, Route, Router, Routes};
use leptos_router::path;

use crate::api;
use crate::components::layout::GlobalShortcuts;
use crate::components::{Icon, Logo, Shell, Spinner, ToastHost};
use crate::pages;
use crate::state::{AuthState, Boot, Theme, Toasts};
use crate::ws::LiveEvents;

#[component]
pub fn App() -> impl IntoView {
    let auth = AuthState::new();
    let toasts = Toasts::new();
    let theme = Theme::init();
    let live = LiveEvents::new();
    provide_context(auth);
    provide_context(toasts);
    provide_context(theme);
    provide_context(live);

    // A 401 anywhere means the session is gone; the shell then redirects to /login.
    api::set_unauthorized_handler(move || auth.clear());

    // Only hold a WebSocket open while someone is signed in.
    Effect::new(move |_| {
        if auth.is_logged_in() {
            live.start();
        } else {
            live.stop();
        }
    });

    spawn_local(auth.boot());

    view! {
        <Router base=api::base_path()>
            <GlobalShortcuts />
            <Show when=move || auth.boot.get() != Boot::Pending fallback=Splash>
                <Show when=move || auth.boot.get() != Boot::Offline fallback=Offline>
                    <Routes fallback=pages::NotFound>
                        <Route path=path!("/setup") view=pages::SetupPage />
                        <Route path=path!("/login") view=pages::LoginPage />
                        <ParentRoute path=path!("") view=Shell>
                            <Route path=path!("") view=pages::DashboardPage />
                            <Route path=path!("downloads") view=pages::downloads::DownloadsPage />
                            <Route path=path!("system/status") view=pages::system::StatusPage />
                            <Route path=path!("system/health") view=pages::system::HealthPage />
                            <Route path=path!("system/tasks") view=pages::system::TasksPage />
                            <Route path=path!("system/events") view=pages::system::EventsPage />
                            <Route path=path!("system/backup") view=pages::system::BackupPage />
                            <Route path=path!("system/updates") view=pages::system::UpdatesPage />
                            <Route path=path!("system/logs") view=pages::system::LogsPage />
                            <Route path=path!("system/migrate") view=pages::system::MigratePage />
                            <Route path=path!("settings/general") view=pages::settings::GeneralPage />
                            <Route path=path!("settings/ui") view=pages::settings::UiSettingsPage />
                            <Route path=path!("settings/profiles") view=pages::settings::ProfilesPage />
                            <Route path=path!("settings/quality") view=pages::settings::QualityPage />
                            <Route path=path!("settings/customformats") view=pages::settings::CustomFormatsPage />
                            <Route path=path!("settings/parse") view=pages::settings::ParsePage />
                            <Route path=path!("movies") view=pages::movies::MoviesPage />
                            <Route path=path!("movies/add") view=pages::movies::AddMoviePage />
                            <Route path=path!("movies/collections") view=pages::movies::CollectionsPage />
                            <Route path=path!("movies/import") view=pages::movies::ImportMoviesPage />
                            <Route path=path!("movies/:id") view=pages::movies::MovieDetailPage />
                            <Route path=path!("series") view=pages::series::SeriesPage />
                            <Route path=path!("series/add") view=pages::series::AddSeriesPage />
                            <Route path=path!("series/import") view=pages::series::ImportSeriesPage />
                            <Route path=path!("series/:id") view=pages::series::SeriesDetailPage />
                            <Route path=path!("calendar") view=pages::calendar::CalendarPage />
                            <Route path=path!("activity/queue") view=pages::activity::QueuePage />
                            <Route path=path!("activity/history") view=pages::activity::HistoryPage />
                            <Route path=path!("activity/blocklist") view=pages::activity::BlocklistPage />
                            <Route path=path!("wanted/missing") view=pages::wanted::MissingPage />
                            <Route path=path!("wanted/cutoffunmet") view=pages::wanted::CutoffUnmetPage />
                            <Route path=path!("settings/mediamanagement") view=pages::settings::MediaManagementPage />
                            <Route path=path!("settings/naming") view=pages::settings::NamingPage />
                            <Route path=path!("discover") view=pages::discover::DiscoverPage />
                            <Route path=path!("discover/*feed") view=pages::discover::DiscoverFeedPage />
                            <Route path=path!("search") view=pages::search::SearchPage />
                            <Route path=path!("media/:kind/:id") view=pages::media::MediaDetailPage />
                            <Route path=path!("requests") view=pages::requests::RequestsPage />
                            <Route path=path!("issues") view=pages::issues::IssuesPage />
                            <Route path=path!("issues/:id") view=pages::issues::IssueDetailPage />
                            <Route path=path!("settings/users") view=pages::settings::UsersPage />
                            <Route path=path!("settings/requests") view=pages::settings::RequestSettingsPage />
                            <Route path=path!("settings/security") view=pages::settings::SecuritySettingsPage />
                            <Route path=path!("profile") view=pages::ProfilePage />
                            <Route path=path!("indexers") view=pages::indexers::IndexersPage />
                            <Route path=path!("indexers/search") view=pages::indexers::IndexerSearchPage />
                            <Route path=path!("indexers/stats") view=pages::indexers::IndexerStatsPage />
                            <Route path=path!("settings/indexers") view=pages::settings::IndexerSettingsPage />
                            <Route path=path!("settings/downloadclients") view=pages::settings::DownloadClientsPage />
                            <Route path=path!("settings/notifications") view=pages::settings::NotificationsPage />
                            <Route path=path!("settings/mediaservers") view=pages::settings::MediaServersPage />
                            <Route path=path!("settings/importlists") view=pages::settings::ImportListsPage />
                            <Route path=path!("settings/metadata") view=pages::settings::MetadataPage />
                            <Route path=path!("settings/tags") view=pages::settings::TagsPage />
                            <Route path=path!("settings/wizard") view=pages::settings::WizardSettingsPage />
                            <Route path=path!("books") view=pages::books::AuthorsPage />
                            <Route path=path!("books/add") view=pages::books::AddAuthorPage />
                            <Route path=path!("books/import") view=pages::books::ImportAuthorsPage />
                            <Route path=path!("books/authors/:id") view=pages::books::AuthorDetailPage />
                            <Route path=path!("books/book/:id") view=pages::books::BookDetailPage />
                            <Route path=path!("settings/books") view=|| view! { <Redirect path="/settings/mediamanagement" /> } />
                            <Route path=path!("music") view=pages::music::ArtistsPage />
                            <Route path=path!("music/add") view=pages::music::AddArtistPage />
                            <Route path=path!("music/import") view=pages::music::ImportArtistsPage />
                            <Route path=path!("music/artists/:id") view=pages::music::ArtistDetailPage />
                            <Route path=path!("music/albums/:id") view=pages::music::AlbumDetailPage />
                            <Route path=path!("settings/music") view=|| view! { <Redirect path="/settings/metadata" /> } />
                            <Route path=path!("settings/metadataprofiles") view=pages::music::MetadataProfilesPage />
                            <Route path=path!("radio") view=pages::radio::RadioShowsPage />
                            <Route path=path!("radio/add") view=pages::radio::AddRadioShowPage />
                            <Route path=path!("radio/stations") view=pages::radio::RadioStationsPage />
                            <Route path=path!("radio/review") view=pages::radio::RadioReviewPage />
                            <Route path=path!("radio/shows/:id") view=pages::radio::RadioShowPage />
                            <Route path=path!("settings/radio") view=pages::radio::RadioSettingsPage />
                            <Route path=path!("*any") view=pages::NotFound />
                        </ParentRoute>
                    </Routes>
                </Show>
            </Show>
            <ToastHost />
        </Router>
    }
}

#[component]
fn Splash() -> impl IntoView {
    view! {
        <div class="splash">
            <div class="splash-inner">
                <Logo />
                <Spinner label="Starting Reelix…" />
            </div>
        </div>
    }
}

#[component]
fn Offline() -> impl IntoView {
    let auth = AuthState::expect();
    view! {
        <main class="auth-page">
            <div class="auth-card">
                <div class="auth-brand">
                    <Logo />
                    <span class="brand-name">"Reelix"</span>
                </div>
                <div class="alert alert-danger" role="alert">
                    <Icon name="error" />
                    <div class="alert-body">
                        <strong>"Can't reach the server"</strong>
                        <span>"The Reelix API didn't respond. Check that the server is running, then try again."</span>
                    </div>
                </div>
                <button type="button" class="btn btn-primary btn-block" on:click=move |_| spawn_local(auth.boot())>
                    <Icon name="refresh" />
                    "Try again"
                </button>
            </div>
        </main>
    }
}
