use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use clap::Subcommand;
use futures::StreamExt;
use rand::seq::SliceRandom;
use testquorum_api::Client;
use testquorum_api::types::TestManager;

use crate::CargoManager;
use crate::Environment;
use crate::ManagerRegistry;
use crate::NixManager;
use crate::NpmManager;
use crate::RManager;
use crate::RunContext;
use crate::Test;
use crate::TestEvent;
use crate::TreefmtManager;
use crate::config::find_config_file;
use crate::detect_cargo;
use crate::detect_environment;
use crate::detect_nix;
use crate::detect_npm;
use crate::detect_r;
use crate::detect_treefmt;
use crate::managers::nix::NixBuilder;
use crate::managers::nix::PreFailed;
use crate::ranker;
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
    /// Skip environment detection, authentication, and upload — run entirely
    /// offline against the local registry.
    #[arg(long, global = true)]
    pub local: bool,

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
    let (registry, nix_builder) = build_registry(&config);

    match cli.command {
        Some(Commands::Discover) => {
            if !cli.local {
                let env = detect_environment();
                print_auth_banner(env.as_ref()).await;
            }
            discover_only(&registry).await
        }
        Some(Commands::Run) | None => {
            let inputs = if cli.local {
                println!("local mode: upload disabled");
                None
            } else {
                let env = detect_environment();
                prepare_upload(env.as_ref()).await
            };
            discover_and_run(&registry, &config, inputs, nix_builder).await
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

/// Resolves auth + run context, printing a consolidated banner. Returns the
/// raw `Client` and `RunContext` so the caller can decide whether to spawn a
/// plain uploader or first run the ranking flow against them. Never returns
/// an error: upload is best-effort.
async fn prepare_upload(env: &dyn Environment) -> Option<(Client, RunContext)> {
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
    Some((client, ctx))
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
    if let Some(cargo) = &config.managers.cargo {
        if cargo.nextest == Some(false) && cargo.nextest_archive.is_some() {
            anyhow::bail!(
                "`[managers.cargo] nextest_archive` requires the nextest backend and cannot be combined with `nextest = false`"
            );
        }
    }
    Ok(config)
}

fn default_config() -> testquorum_config::Config {
    testquorum_config::Config {
        managers: testquorum_config::Managers {
            autodetect: true,
            nix: None,
            cargo: None,
            npm: None,
            r: None,
            treefmt: None,
        },
        cloud: testquorum_config::Cloud::default(),
    }
}

/// Builds the manager registry and, when the nix manager is active, the shared
/// [`NixBuilder`] a `nix://` cargo archive resolves through. The builder is
/// returned so the caller can drain the build failures it accumulated during
/// discovery (see [`NixBuilder::take_pre_failed`]).
fn build_registry(
    config: &testquorum_config::Config,
) -> (ManagerRegistry, Option<Arc<NixBuilder>>) {
    let mut registry = ManagerRegistry::new();

    let nix_config = config.managers.nix.as_ref();
    let nix_enabled = nix_config.map(|n| n.enabled).unwrap_or(true);
    let nix_attrset = nix_config
        .map(|n| n.attrset.clone())
        .unwrap_or_else(|| "checks".to_string());

    // The builder exists only when the nix manager is registered: that manager
    // is what reports a `nix://` build's pass/fail, so cargo may only depend on
    // one when it's active.
    let mut nix_builder: Option<Arc<NixBuilder>> = None;
    if config.managers.autodetect && nix_enabled && std::path::Path::new("flake.nix").exists() {
        match detect_nix() {
            Ok(()) => {
                nix_builder = Some(Arc::new(NixBuilder::new(nix_attrset.clone())));
                registry.register(Box::new(NixManager::new(nix_attrset)));
            }
            Err(e) => eprintln!("warning: nix detection failed: {}", e),
        }
    }

    let cargo_config = config.managers.cargo.as_ref();
    let cargo_enabled = cargo_config.map(|c| c.enabled).unwrap_or(true);
    let cargo_nextest = cargo_config.and_then(|c| c.nextest);
    let cargo_archive = cargo_config.and_then(|c| c.nextest_archive.clone());

    if config.managers.autodetect && cargo_enabled {
        match detect_cargo(cargo_config.and_then(|c| c.manifest_path.as_deref())) {
            // Construction is infallible: an `nextest_archive` is staged lazily
            // in `discover`, so a broken archive surfaces there as a discovery
            // error the run loop reports (non-zero exit), not a construction
            // abort. Detection failure means "not a cargo project" — skip.
            Ok(manifest_path) => {
                registry.register(Box::new(CargoManager::new(
                    manifest_path,
                    cargo_nextest,
                    cargo_archive,
                    nix_builder.clone(),
                )));
            }
            Err(e) => eprintln!("warning: cargo detection failed: {}", e),
        }
    }

    let npm_config = config.managers.npm.as_ref();
    let npm_enabled = npm_config.map(|c| c.enabled).unwrap_or(true);
    let npm_path = npm_config.and_then(|c| c.package_json_path.as_deref());

    // Gate autodetect on package.json existing (like nix gates on flake.nix)
    // unless an explicit path is configured, in which case let detect_npm report the error.
    if config.managers.autodetect
        && npm_enabled
        && (npm_path.is_some() || std::path::Path::new("package.json").exists())
    {
        match detect_npm(npm_path) {
            Ok(package_json_path) => {
                registry.register(Box::new(NpmManager::new(package_json_path)));
            }
            Err(e) => eprintln!("warning: npm detection failed: {}", e),
        }
    }

    let r_config = config.managers.r.as_ref();
    let r_enabled = r_config.map(|c| c.enabled).unwrap_or(true);
    let r_description_path = r_config.and_then(|c| c.description_path.as_deref());

    // Gate autodetect on DESCRIPTION existing (like nix gates on flake.nix)
    // unless an explicit path is configured, in which case let detect_r report the error.
    if config.managers.autodetect
        && r_enabled
        && (r_description_path.is_some() || std::path::Path::new("DESCRIPTION").exists())
    {
        match detect_r(r_description_path) {
            Ok(description_path) => {
                registry.register(Box::new(RManager::new(description_path)));
            }
            Err(e) => eprintln!("warning: r detection failed: {}", e),
        }
    }

    let treefmt_config = config.managers.treefmt.as_ref();
    let treefmt_enabled = treefmt_config.map(|c| c.enabled).unwrap_or(None);

    if config.managers.autodetect && treefmt_enabled != Some(false) {
        match detect_treefmt() {
            Ok(()) => registry.register(Box::new(TreefmtManager::new(treefmt_enabled))),
            Err(e) => eprintln!("warning: treefmt detection failed: {}", e),
        }
    }

    (registry, nix_builder)
}

async fn discover_only(registry: &ManagerRegistry) -> Result<RunResult, anyhow::Error> {
    let mut discovery_errors: Vec<(TestManager, String)> = Vec::new();
    let mut total = 0;

    for manager in registry.managers() {
        match manager.discover().await {
            Ok(mut tests) => {
                tests.sort_by(|a, b| a.name.cmp(&b.name));
                println!("{}: {} test(s)", manager.identity(), tests.len());
                for test in &tests {
                    println!("  - {}", test.name);
                }
                total += tests.len();
            }
            Err(e) => {
                eprintln!("error discovering from {}: {}", manager.identity(), e);
                discovery_errors.push((manager.identity(), e.to_string()));
            }
        }
    }

    println!("\nTotal: {} test(s)", total);
    report_discovery_errors(&discovery_errors);

    if discovery_errors.is_empty() {
        Ok(RunResult::Success)
    } else {
        Ok(RunResult::Error)
    }
}

/// Re-states discovery failures after the run summary, since the inline
/// `error discovering …` line is otherwise buried above all the test output.
/// The runner still exits non-zero (`RunResult::Error`); this makes the *why*
/// visible at the end.
fn report_discovery_errors(errors: &[(TestManager, String)]) {
    if errors.is_empty() {
        return;
    }
    eprintln!("\n{} manager(s) failed to discover:", errors.len());
    for (manager, reason) in errors {
        eprintln!("  - {}: {}", manager, reason);
    }
}

async fn discover_and_run(
    registry: &ManagerRegistry,
    config: &testquorum_config::Config,
    upload_inputs: Option<(Client, RunContext)>,
    nix_builder: Option<Arc<NixBuilder>>,
) -> Result<RunResult, anyhow::Error> {
    let mut discovery_errors: Vec<(TestManager, String)> = Vec::new();
    let mut total_passed = 0;
    let mut total_failed = 0;

    // Phase 1: discover all tests from all managers.
    let mut all_tests: Vec<Test> = Vec::new();
    for manager in registry.managers() {
        match manager.discover().await {
            Ok(tests) => all_tests.extend(tests),
            Err(e) => {
                eprintln!("error discovering from {}: {}", manager.identity(), e);
                discovery_errors.push((manager.identity(), e.to_string()));
            }
        }
    }

    // A cargo `nix://` archive whose build failed during discovery leaves the
    // nix manager's corresponding test recorded here, to be submitted terminal
    // before ranking (which could otherwise skip it, dropping the failure).
    let pre_failed: Vec<PreFailed> = nix_builder.map(|b| b.take_pre_failed()).unwrap_or_default();

    if all_tests.is_empty() && discovery_errors.is_empty() && pre_failed.is_empty() {
        println!("no tests found");
        return Ok(RunResult::Success);
    }

    // Phase 2: decide the batch source. Server-ranked when the runner is
    // authed and ranking succeeds; otherwise local random — unless
    // `[cloud] required = true`, in which case the cloud failure is fatal.
    let (mut batches, upload, pre_submitted) =
        prepare_run(upload_inputs, &mut all_tests, config, &pre_failed).await?;
    total_failed += pre_submitted.failed;

    // Phase 3: drain batches, grouping each one by manager so a single
    // `manager.run(Vec<Test>)` call still gets a batch worth of work.
    while let Some(batch) = batches.next().await {
        if batch.is_empty() {
            continue;
        }
        let mut by_manager: HashMap<TestManager, Vec<Test>> = HashMap::new();
        for test in batch {
            by_manager
                .entry(test.manager.clone())
                .or_default()
                .push(test);
        }
        for manager in registry.managers() {
            let tests = match by_manager.remove(&manager.identity()) {
                Some(t) if !t.is_empty() => t,
                _ => continue,
            };
            // Drop anything already submitted terminal before ranking: it must
            // not run (and re-submitting would 409 against the stored Failed).
            let tests: Vec<Test> = tests
                .into_iter()
                .filter(|t| {
                    !pre_submitted
                        .skip
                        .contains(&(t.manager.clone(), t.name.clone()))
                })
                .collect();
            if tests.is_empty() {
                continue;
            }

            println!(
                "\nrunning {} test(s) from {}...",
                tests.len(),
                manager.identity()
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
    }

    batches.shutdown().await;
    if let Some(u) = upload {
        u.shutdown().await;
    }

    println!("\n{} passed, {} failed", total_passed, total_failed);
    report_discovery_errors(&discovery_errors);

    if !discovery_errors.is_empty() {
        Ok(RunResult::Error)
    } else if total_failed > 0 {
        Ok(RunResult::TestsFailed)
    } else {
        Ok(RunResult::Success)
    }
}

/// Either the page stream from a successful ranking attempt, or a single
/// locally shuffled batch.
enum Batches {
    Local(Option<Vec<Test>>),
    Ranked(ranker::PageStream),
}

impl Batches {
    async fn next(&mut self) -> Option<Vec<Test>> {
        match self {
            Self::Local(slot) => slot.take(),
            Self::Ranked(s) => s.next().await,
        }
    }

    async fn shutdown(self) {
        match self {
            Self::Local(_) => {}
            Self::Ranked(s) => s.shutdown().await,
        }
    }
}

/// Tries to set up server-side ranking when auth is available.
///
/// On a ranker failure, the `[cloud] required` flag decides the response:
/// `false` (default) → log and fall back to local random order; `true` →
/// propagate as a hard error so the runner exits non-zero.
///
/// `all_tests` is consumed in the local-fallback path and left empty in the
/// ranked path (since the page stream owns its own copy of the tests).
/// Tests submitted terminal-`Failed` before ranking (a tracked `nix://` build a
/// cargo dependency triggered, which failed). The run loop skips them — they
/// won't and mustn't run again — and counts them toward the total. Empty on the
/// local-fallback path, where those tests instead run (and fail) normally.
#[derive(Default)]
struct PreSubmitted {
    skip: std::collections::HashSet<(TestManager, String)>,
    failed: usize,
}

async fn prepare_run(
    upload_inputs: Option<(Client, RunContext)>,
    all_tests: &mut Vec<Test>,
    config: &testquorum_config::Config,
    pre_failed: &[PreFailed],
) -> Result<(Batches, Option<Uploader>, PreSubmitted), anyhow::Error> {
    if let Some((client, ctx)) = upload_inputs {
        let pre_failed_map: ranker::PreFailed = pre_failed
            .iter()
            .map(|p| ((p.name.clone(), p.manager.clone()), p.stderr.clone()))
            .collect();
        match ranker::attempt(
            client.clone(),
            ctx.repo_id.clone(),
            ctx.run.clone(),
            all_tests,
            &config.cloud,
            &pre_failed_map,
        )
        .await
        {
            Ok(ranked) => {
                println!(
                    "ranked run: group {} ({} tests submitted)",
                    ranked.group.group_id,
                    all_tests.len()
                );
                let uploader = Uploader::spawn_with_group(client, ctx, ranked.group);
                let pre_submitted = PreSubmitted {
                    skip: pre_failed
                        .iter()
                        .map(|p| (p.manager.clone(), p.name.clone()))
                        .collect(),
                    failed: pre_failed.len(),
                };
                return Ok((Batches::Ranked(ranked.pages), Some(uploader), pre_submitted));
            }
            Err(e) => {
                if config.cloud.required {
                    return Err(anyhow::anyhow!(
                        "cloud failed and `[cloud] required = true`: {}",
                        e
                    ));
                }
                println!("cloud unavailable ({}); using local random order", e);
                let uploader = Uploader::spawn(client, ctx);
                // The local-random path relies on the uploader minting a UUID
                // for each test the first time it sees a `Discovered` event,
                // and reusing it across later transitions.
                for test in all_tests.iter() {
                    uploader.send(TestEvent::Discovered {
                        name: test.name.clone(),
                        manager: test.manager.clone(),
                    });
                }
                let shuffled = shuffle(std::mem::take(all_tests));
                // Local order didn't pre-submit anything: the pre-failed tests
                // are still in the batch and run (and fail) like any other.
                return Ok((
                    Batches::Local(Some(shuffled)),
                    Some(uploader),
                    PreSubmitted::default(),
                ));
            }
        }
    }
    let shuffled = shuffle(std::mem::take(all_tests));
    Ok((
        Batches::Local(Some(shuffled)),
        None,
        PreSubmitted::default(),
    ))
}

fn shuffle(mut tests: Vec<Test>) -> Vec<Test> {
    let mut rng = rand::rng();
    tests.shuffle(&mut rng);
    tests
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
        TestEvent::Started { name, .. } => {
            println!("  > {}", name);
            Transition::Started
        }
        TestEvent::Finished { name, outcome, .. } if outcome.passed => {
            println!("  PASS {} ({}ms)", name, outcome.duration_ms);
            Transition::Passed
        }
        TestEvent::Finished { name, outcome, .. } => {
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
