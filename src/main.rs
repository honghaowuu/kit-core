use clap::{Parser, Subcommand};
use std::process::ExitCode;

mod contract;
mod envelope;
mod git;
mod lockfile;
mod plan_status;
mod plugin_status;
mod scenarios;

#[derive(Parser)]
#[command(name = "kit", version, about = "Universal binary used by *kit language plugins")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Report current jkit plan/run state as JSON.
    PlanStatus(plan_status::Args),
    /// Report install state of a Claude Code plugin.
    PluginStatus(plugin_status::Args),
    /// Test-scenarios subcommands.
    #[command(subcommand)]
    Scenarios(scenarios::ScenarioCmd),
    /// Contract subcommands.
    #[command(subcommand)]
    Contract(contract::ContractCmd),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::PlanStatus(args) => plan_status::run(args),
        Cmd::PluginStatus(args) => plugin_status::run(args),
        Cmd::Scenarios(cmd) => scenarios::run(cmd),
        Cmd::Contract(cmd) => contract::run(cmd),
    };
    match result {
        Ok(code) => code,
        Err(err) => envelope::print_err(&format!("{err:#}"), None),
    }
}
