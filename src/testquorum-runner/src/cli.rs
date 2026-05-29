use std::collections::HashMap;

use clap::Parser;
use clap::Subcommand;
use futures::StreamExt;
use rand::seq::SliceRandom;

use crate::CargoManager;
use crate::Environment;
use crate::ManagerRegistry;
use crate::NixManager;
use crate::RunContext;
use crate::Test;
use crate::TestEvent;
use crate::config::find_config_file;
use crate::detect_cargo;
use crate::detect_environment;
use crate::detect_nix;
use crate::uploader::Uploader;

pub(crate) enum RunResult {
    Success,
    TestsFailed,
    Error,
}

#[derive(Parser)]
#[command(name = "testquorum-runner")]
#[command(about = "Test runner for testquorum")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Discover available tests
    Discover,
    /// Run discovered tests
    Run,
}

pub(crate) async fn run_cli() -> Result<RunResult, anyhow::Error> {
    let cli = Cli::parse();
    let config = load_config()?;
    let registry = build_registry(&config)?;

    let env = detect_environment();

    match cli.command {
        Some(Commands::Discover) => {
            // Discovery doesn't upload — just identify the session.
            print_auth_banner(env.as_ref()).await;
            discover_only(&registry).await
        }
        Some(Commands::Run) | None => {
            let upload = prepare_upload(env.as_ref()).await;
            discover_and_run(&registry, upload).await
        }
    }
}

async fn print_auth_banner(env: &dyn Environment) {
    match env.authenticated_client().await {
        Ok(None) => {
            println!("unauthenticated (env: {})", env.name());
        }
        Ok(Some(client)) => match client.session_info().await {
            Ok(resp) => {
                println!(
                    "authed as {} (env: {})",
                    resp.into_inner().display_name,
                    env.name()
                );
            }
            Err(e) => {
                println!("auth failed (env: {}): session lookup: {}", env.name(), e);
            }
        },
        Err(e) => {
            println!("auth failed (env: {}): {}", env.name(), e);
        }
    }
}

/// Resolves auth + run context for the run path, printing a single
/// consolidated banner. Returns the spawned uploader when both pieces are
/// available; otherwise prints the reason upload is disabled and returns
/// `None`. Never returns an error: upload is best-effort.
async fn prepare_upload(env: &dyn Environment) -> Option<Uploader> {
    let client = match env.authenticated_client().await {
        Ok(Some(c)) => c,
        Ok(None) => {
            println!("unauthenticated (env: {})", env.name());
            println!("upload disabled: no authenticated client");
            return None;
        }
        Err(e) => {
            println!("auth failed (env: {}): {}", env.name(), e);
            println!("upload disabled: authentication error");
            return None;
        }
    };

    match client.session_info().await {
        Ok(resp) => println!(
            "authed as {} (env: {})",
            resp.into_inner().display_name,
            env.name()
        ),
        Err(e) => println!("auth banner (env: {}): session lookup: {}", env.name(), e),
    }

    let ctx: RunContext = match env.run_context().await {
        Ok(Some(c)) => c,
        Ok(None) => {
            println!("upload disabled: no run context for this invocation");
            return None;
        }
        Err(e) => {
            println!("upload disabled: run context error: {}", e);
            return None;
        }
    };

    println!("uploading test state to {}", ctx.repo_id);
    Some(Uploader::spawn(client, ctx))
}

fn load_config() -> Result<testquorum_config::Config, anyhow::Error> {
    let path = match find_config_file() {
        Some(p) => p,
        None => return Ok(default_config()),
    };
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {}", path.display(), e))?;
    let config = testquorum_config::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", path.display(), e))?;
    if let Some(nix) = &config.managers.nix {
        validate_attrset(&nix.attrset)?;
    }
    Ok(config)
}

fn default_config() -> testquorum_config::Config {
    testquorum_config::Config {
        managers: testquorum_config::Managers {
            autodetect: true,
            nix: None,
            cargo: None,
        },
    }
}

fn build_registry(config: &testquorum_config::Config) -> Result<ManagerRegistry, anyhow::Error> {
    let mut registry = ManagerRegistry::new();

    let nix_config = config.managers.nix.as_ref();
    let nix_enabled = nix_config.map(|n| n.enabled).unwrap_or(true);
    let nix_attrset = nix_config
        .map(|n| n.attrset.clone())
        .unwrap_or_else(|| "checks".to_string());

    if config.managers.autodetect && nix_enabled && std::path::Path::new("flake.nix").exists() {
        match detect_nix() {
            Ok(()) => registry.register(Box::new(NixManager::new(nix_attrset))),
            Err(e) => eprintln!("warning: nix detection failed: {}", e),
        }
    }

    let cargo_config = config.managers.cargo.as_ref();
    let cargo_enabled = cargo_config.map(|c| c.enabled).unwrap_or(true);

    if config.managers.autodetect && cargo_enabled {
        match detect_cargo() {
            Ok(()) => registry.register(Box::new(CargoManager::new())),
            Err(e) => eprintln!("warning: cargo detection failed: {}", e),
        }
    }

    Ok(registry)
}

async fn discover_only(registry: &ManagerRegistry) -> Result<RunResult, anyhow::Error> {
    let mut had_errors = false;
    let mut total = 0;

    for manager in registry.managers() {
        match manager.discover().await {
            Ok(mut tests) => {
                tests.sort_by(|a, b| a.name.cmp(&b.name));
                println!("{}: {} test(s)", manager.name(), tests.len());
                for test in &tests {
                    println!("  - {}", test.name);
                }
                total += tests.len();
            }
            Err(e) => {
                eprintln!("error discovering from {}: {}", manager.name(), e);
                had_errors = true;
            }
        }
    }

    println!("\nTotal: {} test(s)", total);

    if had_errors {
        Ok(RunResult::Error)
    } else {
        Ok(RunResult::Success)
    }
}

async fn discover_and_run(
    registry: &ManagerRegistry,
    upload: Option<Uploader>,
) -> Result<RunResult, anyhow::Error> {
    let mut had_errors = false;
    let mut total_passed = 0;
    let mut total_failed = 0;

    // Phase 1: discover all tests from all managers.
    let mut all_tests: Vec<Test> = Vec::new();
    for manager in registry.managers() {
        match manager.discover().await {
            Ok(tests) => all_tests.extend(tests),
            Err(e) => {
                eprintln!("error discovering from {}: {}", manager.name(), e);
                had_errors = true;
            }
        }
    }

    if all_tests.is_empty() && !had_errors {
        println!("no tests found");
        if let Some(u) = upload {
            u.shutdown().await;
        }
        return Ok(RunResult::Success);
    }

    // Synthesize a Discovered event per test so the uploader can mint the
    // UUIDv7 it'll reuse for the rest of the lifecycle. We do this in the CLI
    // layer so managers stay unmodified.
    if let Some(u) = upload.as_ref() {
        for test in &all_tests {
            u.send(TestEvent::Discovered {
                name: test.name.clone(),
            });
        }
    }

    // Phase 2: randomise order.
    let mut rng = rand::thread_rng();
    all_tests.shuffle(&mut rng);

    // Phase 3: group by manager so each manager receives only its own tests.
    let mut by_manager: HashMap<String, Vec<Test>> = HashMap::new();
    for test in all_tests {
        by_manager
            .entry(test.manager.clone())
            .or_default()
            .push(test);
    }

    // Phase 4: run each manager sequentially with its subset.
    for manager in registry.managers() {
        let tests = match by_manager.remove(manager.name()) {
            Some(t) if !t.is_empty() => t,
            _ => continue,
        };

        println!(
            "\nrunning {} test(s) from {}...",
            tests.len(),
            manager.name()
        );

        let mut stream = manager.run(tests).await;
        while let Some(event) = stream.next().await {
            match render_event(&event) {
                Transition::Discovered | Transition::Started => {}
                Transition::Passed => total_passed += 1,
                Transition::Failed => total_failed += 1,
            }
            if let Some(u) = upload.as_ref() {
                u.send(event);
            }
        }
    }

    if let Some(u) = upload {
        u.shutdown().await;
    }

    println!("\n{} passed, {} failed", total_passed, total_failed);

    if had_errors {
        Ok(RunResult::Error)
    } else if total_failed > 0 {
        Ok(RunResult::TestsFailed)
    } else {
        Ok(RunResult::Success)
    }
}

enum Transition {
    Discovered,
    Started,
    Passed,
    Failed,
}

fn render_event(event: &TestEvent) -> Transition {
    match event {
        TestEvent::Discovered { .. } => Transition::Discovered,
        TestEvent::Started { name } => {
            println!("  > {}", name);
            Transition::Started
        }
        TestEvent::Finished { name, outcome } if outcome.passed => {
            println!("  PASS {} ({}ms)", name, outcome.duration_ms);
            Transition::Passed
        }
        TestEvent::Finished { name, outcome } => {
            println!("  FAIL {} ({}ms)", name, outcome.duration_ms);
            if !outcome.stderr.is_empty() {
                for line in outcome.stderr.lines() {
                    println!("    {}", line);
                }
            }
            Transition::Failed
        }
    }
}

fn validate_attrset(s: &str) -> Result<(), anyhow::Error> {
    let mut chars = s.chars();
    let first = chars
        .next()
        .ok_or_else(|| anyhow::anyhow!("attrset is empty"))?;
    if !first.is_ascii_alphabetic() && first != '_' {
        anyhow::bail!(
            "attrset must start with an ASCII letter or underscore (got {:?})",
            s
        );
    }
    for c in chars {
        if !c.is_ascii_alphanumeric() && c != '_' && c != '-' {
            anyhow::bail!(
                "attrset must contain only ASCII alphanumerics, '_' or '-' (got {:?})",
                s
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_attrset_accepts_common_names() {
        assert!(validate_attrset("checks").is_ok());
        assert!(validate_attrset("ci").is_ok());
        assert!(validate_attrset("_my-checks").is_ok());
        assert!(validate_attrset("foo_bar-baz123").is_ok());
    }

    #[test]
    fn validate_attrset_rejects_empty() {
        assert!(validate_attrset("").is_err());
    }

    #[test]
    fn validate_attrset_rejects_leading_digit() {
        assert!(validate_attrset("1checks").is_err());
    }

    #[test]
    fn validate_attrset_rejects_metacharacters() {
        assert!(validate_attrset("checks\"; abort \"x").is_err());
        assert!(validate_attrset("checks.x86_64-linux").is_err());
        assert!(validate_attrset("checks ").is_err());
        assert!(validate_attrset("..").is_err());
        assert!(validate_attrset("a/b").is_err());
    }
}
