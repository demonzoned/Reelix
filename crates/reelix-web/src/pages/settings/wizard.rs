//! `/settings/wizard`: run the set-up wizard again, in the browser or in a terminal.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

use crate::components::{Card, Icon, PageHeader};
use crate::pages::system::require_admin;

/// Which onboarding this build has: the set-up wizard, or the classic onboarding when Reelix is
/// built with the `classic-setup` feature.
const CLASSIC: bool = cfg!(feature = "classic-setup");

#[component]
pub fn WizardSettingsPage() -> impl IntoView {
    require_admin();
    let navigate = use_navigate();
    let launch = move |_| navigate("/setup", Default::default());
    let (what, steps) = if CLASSIC {
        (
            "This Reelix is built with the classic onboarding: media server, root folders, download client, indexers and request defaults.",
            "It starts after the admin account, and every step can be skipped.",
        )
    } else {
        (
            "The set-up wizard asks what you follow, then for TMDB, library folders, download apps, sites to search, quality, media servers and requests. You can also bring over the *arr apps.",
            "It starts after the admin account. Everything you have set stays as it is until you save a step, and every step can be skipped.",
        )
    };
    view! {
        <PageHeader title="Set-up wizard" subtitle="Ask the set-up questions again, one step at a time." />
        <Card title="Launch wizard" subtitle=what>
            <div class="form">
                <p class="field-hint">{steps}</p>
                <div class="form-actions">
                    <button type="button" class="btn btn-primary" on:click=launch>
                        "Launch wizard"
                        <Icon name="chevron" />
                    </button>
                </div>
            </div>
        </Card>
        <Card title="In a terminal" subtitle="For a server with no desktop. It asks the same questions as the browser wizard.">
            <div class="form">
                <p>"With Docker, on the server:"</p>
                <pre class="mono"><code>"docker exec -it reelix /app/reelix setup"</code></pre>
                <p>"Without Docker (the Linux service):"</p>
                <pre class="mono"><code>"sudo -u reelix /opt/reelix/reelix setup --data-dir /var/lib/reelix"</code></pre>
                <p class="field-hint">"Add " <code>"--plain"</code> " for lines that a screen reader can follow, with nothing redrawn. Every command, from installing Reelix, is in docs/terminal-setup.md."</p>
            </div>
        </Card>
    }
}
