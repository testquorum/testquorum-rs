mod cli;
mod config;
mod environments;
mod managers;
mod registry;

// Re-exports so submodules can refer to types via `crate::X`. These are
// genuine re-exports, not local imports — make that intent explicit.
use cli::RunResult;
use cli::run_cli;
pub(crate) use environments::Environment;
pub(crate) use environments::detect_environment;
pub(crate) use managers::CargoManager;
pub(crate) use managers::NixManager;
pub(crate) use managers::Test;
pub(crate) use managers::TestEvent;
pub(crate) use managers::TestManager;
pub(crate) use managers::TestOutcome;
pub(crate) use managers::detect_cargo;
pub(crate) use managers::detect_nix;
pub(crate) use registry::ManagerRegistry;

#[tokio::main]
async fn main() {
    match run_cli().await {
        Ok(RunResult::Success) => {}
        Ok(RunResult::TestsFailed) => std::process::exit(1),
        Ok(RunResult::Error) => std::process::exit(2),
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(2);
        }
    }
}
