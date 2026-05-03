use clap::Parser;
use clap::Subcommand;
use futures::StreamExt;

use crate::ManagerRegistry;
use crate::NixManager;
use crate::TestEvent;
use crate::config::find_config_file;
use crate::detect_nix;

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

    match cli.command {
        Some(Commands::Discover) => discover_only(&registry).await,
        Some(Commands::Run) | None => discover_and_run(&registry).await,
    }
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

    if config.managers.autodetect && nix_enabled {
        match detect_nix() {
            Ok(()) => registry.register(Box::new(NixManager::new(nix_attrset))),
            Err(e) => eprintln!("warning: nix detection failed: {}", e),
        }
    }

    Ok(registry)
}

async fn discover_only(registry: &ManagerRegistry) -> Result<RunResult, anyhow::Error> {
    let mut had_errors = false;
    let mut total = 0;

    for manager in registry.managers() {
        match manager.discover().await {
            Ok(tests) => {
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

async fn discover_and_run(registry: &ManagerRegistry) -> Result<RunResult, anyhow::Error> {
    let mut had_errors = false;
    let mut total_passed = 0;
    let mut total_failed = 0;
    let mut total_discovered = 0;

    for manager in registry.managers() {
        let tests = match manager.discover().await {
            Ok(tests) => tests,
            Err(e) => {
                eprintln!("error discovering from {}: {}", manager.name(), e);
                had_errors = true;
                continue;
            }
        };

        if tests.is_empty() {
            continue;
        }

        total_discovered += tests.len();
        println!(
            "\nrunning {} test(s) from {}...",
            tests.len(),
            manager.name()
        );

        let mut stream = manager.run(tests).await;
        while let Some(event) = stream.next().await {
            match render_event(&event) {
                Transition::Started => {}
                Transition::Passed => total_passed += 1,
                Transition::Failed => total_failed += 1,
            }
        }
    }

    if total_discovered == 0 && !had_errors {
        println!("no tests found");
        return Ok(RunResult::Success);
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
    Started,
    Passed,
    Failed,
}

fn render_event(event: &TestEvent) -> Transition {
    match event {
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
