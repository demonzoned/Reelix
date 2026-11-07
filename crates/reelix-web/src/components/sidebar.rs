//! Primary navigation, mirroring the README navigation map.

use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::{use_location, use_navigate};
use reelix_api_types::Permissions;

use super::{Icon, Logo};
use crate::api::base_path;
use crate::state::AuthState;

pub struct NavItem {
    pub label: &'static str,
    pub icon: &'static str,
    pub href: &'static str,
    /// Path prefix that marks this item (and its children) as active.
    pub prefix: &'static str,
    /// `None` = visible to every signed-in user; otherwise any one of these bits suffices.
    pub perm: Option<Permissions>,
    pub children: &'static [(&'static str, &'static str)],
}

const LIB: Option<Permissions> = Some(Permissions::MANAGE_LIBRARY);
const ADMIN: Option<Permissions> = Some(Permissions::ADMIN);
/// Children that need more than their parent: `VIEW_QUEUE` opens Activity for the queue only.
fn child_perm(href: &str) -> Option<Permissions> {
    matches!(href, "/activity/history" | "/activity/blocklist").then_some(Permissions::MANAGE_LIBRARY)
}

/// Path of the stand-alone "Users" entry shown to non-admin user managers.
const USERS_HREF: &str = "/settings/users";

/// Sections separated by dividers.
pub const NAV: &[&[NavItem]] = &[
    &[
        NavItem {
            label: "Dashboard",
            icon: "dashboard",
            href: "/",
            prefix: "",
            perm: ADMIN,
            children: &[],
        },
        NavItem {
            label: "Discover",
            icon: "discover",
            href: "/discover",
            prefix: "/discover",
            perm: None,
            children: &[],
        },
        NavItem {
            label: "Requests",
            icon: "requests",
            href: "/requests",
            prefix: "/requests",
            perm: None,
            children: &[],
        },
        NavItem {
            label: "Issues",
            icon: "issues",
            href: "/issues",
            prefix: "/issues",
            perm: None,
            children: &[],
        },
    ],
    &[
        NavItem {
            label: "Movies",
            icon: "movies",
            href: "/movies",
            prefix: "/movies",
            perm: LIB,
            children: &[
                ("Library", "/movies"),
                ("Add New", "/movies/add"),
                ("Collections", "/movies/collections"),
                ("Import Library", "/movies/import"),
            ],
        },
        NavItem {
            label: "Series",
            icon: "series",
            href: "/series",
            prefix: "/series",
            perm: LIB,
            children: &[
                ("Library", "/series"),
                ("Add New", "/series/add"),
                ("Import Library", "/series/import"),
            ],
        },
        NavItem {
            label: "Books",
            icon: "books",
            href: "/books",
            prefix: "/books",
            perm: LIB,
            children: &[
                ("Library", "/books"),
                ("Add New", "/books/add"),
                ("Import Library", "/books/import"),
            ],
        },
        NavItem {
            label: "Music",
            icon: "music",
            href: "/music",
            prefix: "/music",
            perm: LIB,
            children: &[
                ("Library", "/music"),
                ("Add New", "/music/add"),
                ("Import Library", "/music/import"),
            ],
        },
        NavItem {
            label: "Radio",
            icon: "radio",
            href: "/radio",
            prefix: "/radio",
            perm: LIB,
            children: &[
                ("Shows", "/radio"),
                ("Add New", "/radio/add"),
                ("Stations", "/radio/stations"),
                ("Review", "/radio/review"),
            ],
        },
        NavItem {
            label: "Downloads",
            icon: "download",
            href: "/downloads",
            prefix: "/downloads",
            perm: ADMIN,
            children: &[],
        },
        NavItem {
            label: "Calendar",
            icon: "calendar",
            href: "/calendar",
            prefix: "/calendar",
            perm: Some(Permissions::MANAGE_LIBRARY.union(Permissions::VIEW_CALENDAR)),
            children: &[],
        },
        NavItem {
            label: "Activity",
            icon: "activity",
            href: "/activity/queue",
            prefix: "/activity",
            perm: Some(Permissions::MANAGE_LIBRARY.union(Permissions::VIEW_QUEUE)),
            children: &[
                ("Queue", "/activity/queue"),
                ("History", "/activity/history"),
                ("Blocklist", "/activity/blocklist"),
            ],
        },
        NavItem {
            label: "Wanted",
            icon: "wanted",
            href: "/wanted/missing",
            prefix: "/wanted",
            perm: LIB,
            children: &[
                ("Missing", "/wanted/missing"),
                ("Cutoff Unmet", "/wanted/cutoffunmet"),
            ],
        },
        NavItem {
            label: "Indexers",
            icon: "indexers",
            href: "/indexers",
            prefix: "/indexers",
            perm: ADMIN,
            children: &[
                ("Indexers", "/indexers"),
                ("Search", "/indexers/search"),
                ("Stats", "/indexers/stats"),
            ],
        },
    ],
    &[
        NavItem {
            label: "Settings",
            icon: "settings",
            href: "/settings/general",
            prefix: "/settings",
            perm: ADMIN,
            children: &[
                ("General", "/settings/general"),
                ("UI", "/settings/ui"),
                ("Media Management", "/settings/mediamanagement"),
                ("Naming", "/settings/naming"),
                ("Metadata Profiles", "/settings/metadataprofiles"),
                ("Profiles", "/settings/profiles"),
                ("Quality", "/settings/quality"),
                ("Custom Formats", "/settings/customformats"),
                ("Parse Release", "/settings/parse"),
                ("Indexers", "/settings/indexers"),
                ("Download Clients", "/settings/downloadclients"),
                ("Import Lists", "/settings/importlists"),
                ("Media Servers", "/settings/mediaservers"),
                ("Notifications", "/settings/notifications"),
                ("Requests", "/settings/requests"),
                ("Users", "/settings/users"),
                ("Tags", "/settings/tags"),
                ("Metadata", "/settings/metadata"),
                ("Radio", "/settings/radio"),
                ("Security", "/settings/security"),
                ("Set-up Wizard", "/settings/wizard"),
            ],
        },
        // Admins reach Users through Settings; this entry is for non-admin user managers.
        NavItem {
            label: "Users",
            icon: "user",
            href: USERS_HREF,
            prefix: USERS_HREF,
            perm: Some(Permissions::MANAGE_USERS),
            children: &[],
        },
        NavItem {
            label: "System",
            icon: "system",
            href: "/system/status",
            prefix: "/system",
            perm: ADMIN,
            children: &[
                ("Status", "/system/status"),
                ("Health", "/system/health"),
                ("Tasks", "/system/tasks"),
                ("Backup", "/system/backup"),
                ("Migrate", "/system/migrate"),
                ("Updates", "/system/updates"),
                ("Events", "/system/events"),
                ("Logs", "/system/logs"),
            ],
        },
    ],
];

/// The current route path with the URL base stripped (always starts with `/`).
pub fn app_path(pathname: &str) -> String {
    let rest = pathname.strip_prefix(base_path()).unwrap_or(pathname);
    let trimmed = rest.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.to_string()
    }
}

fn is_under(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return path == "/";
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[component]
pub fn Sidebar(open: RwSignal<bool>) -> impl IntoView {
    let auth = AuthState::expect();
    let location = use_location();
    let path = Memo::new(move |_| app_path(&location.pathname.get()));
    let close = move |_| open.set(false);
    let navigate = use_navigate();

    let sections = move || {
        NAV.iter()
            .map(|section| {
                section
                    .iter()
                    .filter(|item| item.perm.is_none_or(|p| auth.has_any(p)))
                    .filter(|item| item.href != USERS_HREF || !auth.is_admin())
                    .collect::<Vec<_>>()
            })
            .filter(|section| !section.is_empty())
            .map(|section| {
                let items = section
                    .into_iter()
                    .map(|item| {
                        let prefix = item.prefix;
                        let navigate = navigate.clone();
                        let active = move || is_under(&path.get(), prefix);
                        let children = (!item.children.is_empty()).then(|| {
                            view! {
                                <Show when=active>
                                    <ul class="nav-children">
                                        {item
                                            .children
                                            .iter()
                                            .filter(|(_, href)| child_perm(href).is_none_or(|p| auth.has(p)))
                                            .map(|(label, href)| {
                                                view! {
                                                    <li>
                                                        <A href=*href exact=true attr:class="nav-child-link" on:click=close>
                                                            {*label}
                                                        </A>
                                                    </li>
                                                }
                                            })
                                            .collect_view()}
                                    </ul>
                                </Show>
                            }
                        });
                        view! {
                            <li class="nav-item" class:expanded=active>
                                <a
                                    href=format!("{}{}", base_path(), item.href)
                                    class="nav-link"
                                    class:active=active
                                    aria-current=move || active().then_some("page")
                                    on:click=move |ev| {
                                        ev.prevent_default();
                                        open.set(false);
                                        navigate(item.href, Default::default());
                                    }
                                >
                                    <Icon name=item.icon />
                                    <span>{item.label}</span>
                                </a>
                                {children}
                            </li>
                        }
                    })
                    .collect_view();
                view! { <ul class="nav-section">{items}</ul> }
            })
            .collect_view()
    };

    view! {
        <div class="sidebar-scrim" class:visible=move || open.get() on:click=close aria-hidden="true"></div>
        <aside id="app-sidebar" class="sidebar" class:open=move || open.get() aria-label="Primary"
            on:keydown=move |e: leptos::ev::KeyboardEvent| {
                if e.key() == "Escape" && open.get_untracked() {
                    open.set(false);
                    if let Some(t) = document()
                        .query_selector(".drawer-toggle")
                        .ok()
                        .flatten()
                        .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(e).ok())
                    {
                        let _ = t.focus();
                    }
                }
            }>
            <div class="sidebar-brand">
                <A href="/" attr:class="brand" attr:aria-label="Reelix home" on:click=close>
                    <Logo />
                    <span class="brand-name">"Reelix"</span>
                </A>
            </div>
            <nav class="sidebar-nav">{sections}</nav>
            <div class="sidebar-footer">
                <span class="version">"v" {reelix_core::VERSION}</span>
            </div>
        </aside>
    }
}
