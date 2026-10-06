use clap::{CommandFactory, Parser, Subcommand};
use tracel_client::console::Env;

use crate::commands;
use crate::context::CliContext;
use crate::tools::terminal::Terminal;

#[derive(Parser, Debug)]
#[clap(name = "tracel", author, version, about, long_about = None)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Use development environment (localhost:9001) with separate dev credentials
    #[arg(long, global = true, action = clap::ArgAction::SetTrue, hide = true, conflicts_with = "staging")]
    pub dev: bool,

    /// Use staging environment (specify version: 1, 2, etc.)
    #[arg(
        long,
        global = true,
        value_name = "VERSION",
        hide = true,
        conflicts_with = "dev"
    )]
    pub staging: Option<u8>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run your project locally via `cargo run` (forwards args after `--`).
    Train(commands::training::TrainingArgs),

    /// Package your project for running on a remote machine.
    Package(commands::package::PackageArgs),
    /// Log in to the Tracel server.
    Login,
    /// Log out of the Tracel server.
    Logout,
    /// Show which credential commands use, or print an access token for scripts.
    Auth(commands::auth::AuthArgs),
    /// Initialize a new project or reinitialize an existing one.
    Init(commands::init::InitArgs),
    /// Unlink the Tracel Console project from this repository.
    Unlink(commands::unlink::UnlinkArgs),
    /// Display current user information.
    Me,
    /// Display current project information.
    Project,
    /// Upload local files as a model version in the model registry.
    Model(commands::model::ModelArgs),
}

pub fn cli_main() {
    let args = CliArgs::parse();

    let environment = if args.dev {
        Env::Development
    } else if let Some(version) = args.staging {
        Env::Staging(version)
    } else {
        Env::Production
    };

    let terminal = Terminal::default();

    if args.dev {
        terminal
            .print_warning("Running in development mode - using local server and dev credentials");
    }

    let Some(command) = args.command else {
        // Bare `tracel` only explains itself: it never logs in, writes files or runs anything.
        // A reader that closes early (`tracel | head`) is not an error.
        let _ = CliArgs::command().print_help();
        return;
    };

    let context = CliContext::new(terminal.clone(), environment);

    if let Err(e) = handle_command(command, context) {
        terminal.cancel_finalize(&format!("{e:#}"));
        std::process::exit(1);
    }
}

fn handle_command(command: Commands, context: CliContext) -> anyhow::Result<()> {
    match command {
        Commands::Train(run_args) => commands::training::handle_command(run_args, context),
        Commands::Package(package_args) => commands::package::handle_command(package_args, context),
        Commands::Login => commands::login::handle_command(context),
        Commands::Logout => commands::logout::handle_command(context),
        Commands::Auth(auth_args) => commands::auth::handle_command(auth_args, context),
        Commands::Init(init_args) => commands::init::handle_command(init_args, context),
        Commands::Unlink(unlink_args) => commands::unlink::handle_command(unlink_args, context),
        Commands::Me => commands::me::handle_command(context),
        Commands::Project => commands::project::handle_command(context),
        Commands::Model(model_args) => commands::model::handle_command(model_args, context),
    }
}
