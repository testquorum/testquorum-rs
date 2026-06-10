//! Background uploader that streams `TestEvent`s to the TestQuorum API.
//!
//! Spawned when the runner has both an authenticated `Client` and a
//! `RunContext`. The lifetime is bounded by the run: callers `send` events as
//! they happen and `shutdown` once the run is over. Uploads are best-effort —
//! a definitive 4xx disables uploads for the rest of the process, transient
//! errors are retried, and nothing here is allowed to escalate to a non-zero
//! exit code.

use std::collections::HashMap;
use std::time::Duration;
use std::time::SystemTime;

use reqwest::StatusCode;
use testquorum_api::Client;
use testquorum_api::types::EpochSecs;
use testquorum_api::types::Run;
use testquorum_api::types::SubmitTestResultsRequest;
use testquorum_api::types::TestManager;
use testquorum_api::types::TestResultDoc;
use testquorum_api::types::TestState;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::TestEvent;
use crate::environments::RunContext;

/// Hard ceiling per request from the spec is 1024; flush at a quarter of that
/// to keep latency reasonable.
const FLUSH_BATCH_SIZE: usize = 256;

/// Periodic flush so a slow trickle of events still reaches the dashboard.
const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// Retry schedule for transient failures (5xx, network). Picked to absorb
/// brief blips without making upload-disabled feel hung.
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(4),
];

/// Identifiers minted by the ranker for the explicit Discovered submission.
/// Reused by the uploader so the rest of the lifecycle (`Running`, terminal)
/// reuses the same UUID and carries the same `test_group`.
pub(crate) struct GroupContext {
    pub(crate) group_id: Uuid,
    /// `(test_name, test_manager)` → `(uuid, discovered_at)`.
    pub(crate) instances: HashMap<(String, String), (Uuid, EpochSecs)>,
}

pub(crate) struct Uploader {
    tx: mpsc::UnboundedSender<TestEvent>,
    handle: JoinHandle<()>,
}

impl Uploader {
    pub(crate) fn spawn(client: Client, ctx: RunContext) -> Self {
        Self::spawn_inner(client, ctx, None)
    }

    /// Spawn an uploader that reuses the UUIDs the ranker already submitted
    /// and stamps `test_group` on every later transition.
    pub(crate) fn spawn_with_group(client: Client, ctx: RunContext, group: GroupContext) -> Self {
        Self::spawn_inner(client, ctx, Some(group))
    }

    fn spawn_inner(client: Client, ctx: RunContext, group: Option<GroupContext>) -> Self {
        // Unbounded: we never want to drop test events for purely local
        // backpressure reasons. The buffer is bounded in practice by the
        // total number of test transitions in a single run, which is small
        // (a few thousand `TestResultDoc`s at worst), so the memory cost
        // is negligible even when uploads are stalled.
        let (tx, rx) = mpsc::unbounded_channel::<TestEvent>();
        let handle = tokio::spawn(run(client, ctx, rx, group));
        Self { tx, handle }
    }

    /// Non-blocking. Only drops the event if the background task has exited
    /// (upload disabled by a definitive 4xx); a healthy uploader always
    /// accepts.
    pub(crate) fn send(&self, event: TestEvent) {
        let _ = self.tx.send(event);
    }

    /// Drops the sender and awaits the background task so any in-flight batch
    /// gets a chance to flush. Logs but swallows panics.
    pub(crate) async fn shutdown(self) {
        drop(self.tx);
        if let Err(e) = self.handle.await {
            eprintln!("uploader task ended abnormally: {}", e);
        }
    }
}

/// Per-test-name lifecycle bookkeeping. We mint the UUIDv7 the first time a
/// test name shows up and reuse it across transitions so the server can stitch
/// `Discovered → Running → terminal` into one record.
struct Instance {
    id: Uuid,
    discovered_at: EpochSecs,
    started_at: Option<EpochSecs>,
}

async fn run(
    client: Client,
    ctx: RunContext,
    mut rx: mpsc::UnboundedReceiver<TestEvent>,
    group: Option<GroupContext>,
) {
    let (group_id, mut instances) = match group {
        Some(g) => {
            let pre_seeded = g
                .instances
                .into_iter()
                .map(|((name, _manager), (id, discovered_at))| {
                    (
                        name,
                        Instance {
                            id,
                            discovered_at,
                            started_at: None,
                        },
                    )
                })
                .collect();
            (Some(g.group_id), pre_seeded)
        }
        None => (None, HashMap::new()),
    };
    let mut buffer: Vec<TestResultDoc> = Vec::with_capacity(FLUSH_BATCH_SIZE);
    let mut interval = tokio::time::interval_at(Instant::now() + FLUSH_INTERVAL, FLUSH_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            event = rx.recv() => match event {
                Some(event) => {
                    if let Some(doc) = build_doc(&event, &mut instances, &ctx.run, group_id) {
                        buffer.push(doc);
                        if buffer.len() >= FLUSH_BATCH_SIZE
                            && !flush(&client, &ctx.repo_id, &mut buffer).await
                        {
                            return;
                        }
                    }
                }
                None => break,
            },
            _ = interval.tick() => {
                if !buffer.is_empty()
                    && !flush(&client, &ctx.repo_id, &mut buffer).await
                {
                    return;
                }
            }
        }
    }

    // Channel drained — final flush.
    if !buffer.is_empty() {
        let _ = flush(&client, &ctx.repo_id, &mut buffer).await;
    }
}

/// Returns false when uploads have been definitively disabled and the run
/// loop should exit.
async fn flush(client: &Client, repo_id: &str, buffer: &mut Vec<TestResultDoc>) -> bool {
    let results = std::mem::take(buffer);
    let req = SubmitTestResultsRequest { results };

    for delay in std::iter::once(None).chain(RETRY_DELAYS.iter().map(|d| Some(*d))) {
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        match client.submit_test_results(repo_id, &req).await {
            Ok(_) => return true,
            Err(e) => match classify(&e) {
                Disposition::Disable(reason) => {
                    eprintln!("upload disabled: {}", reason);
                    return false;
                }
                Disposition::Warn(reason) => {
                    eprintln!("upload warning: {}", reason);
                    return true;
                }
                Disposition::Retry(reason) => {
                    eprintln!("upload transient error, retrying: {}", reason);
                    continue;
                }
            },
        }
    }
    eprintln!("upload batch dropped after exhausting retries");
    true
}

enum Disposition {
    Disable(String),
    Warn(String),
    Retry(String),
}

fn classify(err: &testquorum_api::Error<()>) -> Disposition {
    match err.status() {
        Some(StatusCode::UNAUTHORIZED) => {
            Disposition::Disable("bearer token rejected (401)".to_string())
        }
        Some(StatusCode::PAYMENT_REQUIRED) => Disposition::Disable(
            "repository does not have an active TestQuorum subscription (402)".to_string(),
        ),
        Some(StatusCode::FORBIDDEN) => Disposition::Disable(format!(
            "API key lacks permission for this repo (403): {}",
            err
        )),
        Some(StatusCode::NOT_FOUND) => {
            Disposition::Disable(format!("repository not found by the API (404): {}", err))
        }
        Some(StatusCode::BAD_REQUEST) => {
            Disposition::Disable(format!("rejected as malformed (400): {}", err))
        }
        Some(StatusCode::CONFLICT) => Disposition::Warn(format!(
            "state transition rejected (409); this batch dropped: {}",
            err
        )),
        Some(s) if s.is_server_error() => {
            Disposition::Retry(format!("server error {}: {}", s, err))
        }
        Some(s) => Disposition::Disable(format!("unexpected status {}: {}", s, err)),
        None => Disposition::Retry(format!("network/transport error: {}", err)),
    }
}

fn build_doc(
    event: &TestEvent,
    instances: &mut HashMap<String, Instance>,
    run: &Run,
    group_id: Option<Uuid>,
) -> Option<TestResultDoc> {
    let now = SystemTime::now();
    match event {
        TestEvent::Discovered { name, manager } => {
            let inst = instances
                .entry(name.clone())
                .or_insert_with(|| new_instance(now));
            Some(TestResultDoc {
                id: inst.id,
                test_name: name.clone(),
                test_manager: Some(TestManager(manager.clone())),
                test_group: group_id,
                rank: None,
                run: run.clone(),
                state: TestState::Discovered {
                    discovered_at: inst.discovered_at.clone(),
                },
            })
        }
        TestEvent::Started { name, manager } => {
            let inst = instances
                .entry(name.clone())
                .or_insert_with(|| new_instance(now));
            inst.started_at = Some(now.into());
            Some(TestResultDoc {
                id: inst.id,
                test_name: name.clone(),
                test_manager: Some(TestManager(manager.clone())),
                test_group: group_id,
                rank: None,
                run: run.clone(),
                state: TestState::Running {
                    discovered_at: inst.discovered_at.clone(),
                    started_at: now.into(),
                },
            })
        }
        TestEvent::Finished {
            name,
            manager,
            outcome,
        } => {
            let inst = instances
                .entry(name.clone())
                .or_insert_with(|| new_instance(now));
            let started_at = inst.started_at.clone().unwrap_or_else(|| now.into());
            let id = inst.id;
            let discovered_at = inst.discovered_at.clone();
            let duration_ms = outcome.duration_ms as i64;
            let state = if outcome.passed {
                TestState::Passed {
                    discovered_at,
                    started_at,
                    finished_at: now.into(),
                    duration_ms,
                }
            } else {
                TestState::Failed {
                    discovered_at,
                    started_at,
                    finished_at: now.into(),
                    duration_ms,
                    failure_message: failure_message_from(&outcome.stderr),
                    stderr: outcome.stderr.clone(),
                    stdout: None,
                }
            };
            Some(TestResultDoc {
                id,
                test_name: name.clone(),
                test_manager: Some(TestManager(manager.clone())),
                test_group: group_id,
                rank: None,
                run: run.clone(),
                state,
            })
        }
    }
}

fn new_instance(now: SystemTime) -> Instance {
    Instance {
        id: Uuid::now_v7(),
        discovered_at: now.into(),
        started_at: None,
    }
}

fn failure_message_from(stderr: &str) -> String {
    for line in stderr.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "test failed".to_string()
}

#[cfg(test)]
mod tests {
    use testquorum_api::types::Commit;
    use testquorum_api::types::RunKind;

    use super::*;
    use crate::TestOutcome;

    fn fake_run() -> Run {
        Run {
            head: Commit {
                sha: "aaaa".to_string(),
                height: 10,
            },
            kind: RunKind::Diff {
                merge_base: Commit {
                    sha: "bbbb".to_string(),
                    height: 5,
                },
            },
        }
    }

    #[test]
    fn discovered_then_started_then_passed_shares_id() {
        let run = fake_run();
        let mut instances = HashMap::new();
        let d = build_doc(
            &TestEvent::Discovered {
                name: "t".to_string(),
                manager: "cargo".to_string(),
            },
            &mut instances,
            &run,
            None,
        )
        .unwrap();
        let s = build_doc(
            &TestEvent::Started {
                name: "t".to_string(),
                manager: "cargo".to_string(),
            },
            &mut instances,
            &run,
            None,
        )
        .unwrap();
        let f = build_doc(
            &TestEvent::Finished {
                name: "t".to_string(),
                manager: "cargo".to_string(),
                outcome: TestOutcome {
                    passed: true,
                    duration_ms: 12,
                    stderr: String::new(),
                },
            },
            &mut instances,
            &run,
            None,
        )
        .unwrap();
        assert_eq!(d.id, s.id);
        assert_eq!(s.id, f.id);
        assert_eq!(d.test_manager.as_ref().map(|m| m.0.as_str()), Some("cargo"));
        assert_eq!(s.test_manager.as_ref().map(|m| m.0.as_str()), Some("cargo"));
        assert_eq!(f.test_manager.as_ref().map(|m| m.0.as_str()), Some("cargo"));
        assert!(matches!(d.state, TestState::Discovered { .. }));
        assert!(matches!(s.state, TestState::Running { .. }));
        match f.state {
            TestState::Passed { duration_ms, .. } => assert_eq!(duration_ms, 12),
            _ => panic!("expected Passed"),
        }
    }

    #[test]
    fn failed_uses_first_nonempty_stderr_line_as_message() {
        let run = fake_run();
        let mut instances = HashMap::new();
        let f = build_doc(
            &TestEvent::Finished {
                name: "t".to_string(),
                manager: "cargo".to_string(),
                outcome: TestOutcome {
                    passed: false,
                    duration_ms: 7,
                    stderr: "\n  assertion failed: x == y  \nbacktrace...\n".to_string(),
                },
            },
            &mut instances,
            &run,
            None,
        )
        .unwrap();
        match f.state {
            TestState::Failed {
                failure_message,
                stderr,
                ..
            } => {
                assert_eq!(failure_message, "assertion failed: x == y");
                assert!(stderr.contains("assertion failed"));
            }
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn failed_with_empty_stderr_falls_back_to_default_message() {
        let run = fake_run();
        let mut instances = HashMap::new();
        let f = build_doc(
            &TestEvent::Finished {
                name: "t".to_string(),
                manager: "cargo".to_string(),
                outcome: TestOutcome {
                    passed: false,
                    duration_ms: 1,
                    stderr: String::new(),
                },
            },
            &mut instances,
            &run,
            None,
        )
        .unwrap();
        match f.state {
            TestState::Failed {
                failure_message, ..
            } => assert_eq!(failure_message, "test failed"),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn group_id_is_stamped_on_every_transition() {
        let run = fake_run();
        let group = Uuid::now_v7();
        let mut instances = HashMap::new();
        let d = build_doc(
            &TestEvent::Discovered {
                name: "t".to_string(),
                manager: "cargo".to_string(),
            },
            &mut instances,
            &run,
            Some(group),
        )
        .unwrap();
        let s = build_doc(
            &TestEvent::Started {
                name: "t".to_string(),
                manager: "cargo".to_string(),
            },
            &mut instances,
            &run,
            Some(group),
        )
        .unwrap();
        let f = build_doc(
            &TestEvent::Finished {
                name: "t".to_string(),
                manager: "cargo".to_string(),
                outcome: TestOutcome {
                    passed: true,
                    duration_ms: 1,
                    stderr: String::new(),
                },
            },
            &mut instances,
            &run,
            Some(group),
        )
        .unwrap();
        assert_eq!(d.test_group, Some(group));
        assert_eq!(s.test_group, Some(group));
        assert_eq!(f.test_group, Some(group));
    }

    #[test]
    fn started_without_prior_discovered_mints_id() {
        let run = fake_run();
        let mut instances = HashMap::new();
        let s = build_doc(
            &TestEvent::Started {
                name: "t".to_string(),
                manager: "cargo".to_string(),
            },
            &mut instances,
            &run,
            None,
        )
        .unwrap();
        assert!(matches!(s.state, TestState::Running { .. }));
        assert_eq!(instances.len(), 1);
    }
}
