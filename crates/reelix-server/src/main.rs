use std::process::ExitCode;

use clap::Parser;
use reelix_server::cli::{Cli, Command};
use reelix_server::commands::{self, Maintenance};
use reelix_server::config::Config;
use reelix_server::lifecycle;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match Config::load(&cli.overrides()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("reelix: invalid configuration: {e}");
            return ExitCode::from(2);
        }
    };

    match &cli.command {
        Some(Command::Healthcheck) => ExitCode::from(reelix_server::healthcheck::run(&config)),
        Some(Command::SpotifyLogin {
            credentials,
            client_id,
            redirect_uri,
        }) => {
            let runtime = tokio::runtime::Runtime::new().expect("create Spotify login runtime");
            match runtime.block_on(reelix_direct::spotify::login(
                credentials,
                client_id.as_deref(),
                redirect_uri,
            )) {
                Ok(()) => {
                    println!("Spotify session saved to {}", credentials.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("Spotify login failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(Command::Setup { plain, url }) => reelix_server::setup_wizard::run(
            &config,
            reelix_server::setup_wizard::Options {
                plain: *plain,
                url: url.clone(),
            },
        ),
        Some(Command::Service { action }) => reelix_server::service::main(action, config, &cli),
        Some(Command::Backup { action }) => commands::main(Maintenance::Backup(action), &config),
        Some(Command::Restore { action }) => commands::main(Maintenance::Restore(action), &config),
        Some(Command::Migrate { action }) => commands::main(Maintenance::Migrate(action), &config),
        Some(Command::Config { action }) => commands::main(Maintenance::Config(action), &config),
        None => {
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("reelix: failed to start async runtime: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let result = runtime.block_on(reelix_server::server::run(config));
            // Background tasks are done by now; don't wait long for stragglers.
            runtime.shutdown_timeout(std::time::Duration::from_secs(5));
            match result {
                // A self-update replaced the files: start the new binary (see `lifecycle`).
                Ok(()) => match lifecycle::take_restart() {
                    Some(exe) => lifecycle::restart(&exe),
                    None => ExitCode::SUCCESS,
                },
                Err(e) => {
                    // Logging may not be initialised yet, so always print to stderr too.
                    tracing::error!("fatal: {e:#}");
                    eprintln!("reelix: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
