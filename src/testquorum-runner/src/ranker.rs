//! Server-side test ranking: create a group, submit every discovered test
//! into it, call `/rank`, then stream the ranked queue back as batches.
//!
//! The runner falls back to local randomisation if anything here returns an
//! error. The one exception is `429`: when `ranking.wait_on_rate_limit` is
//! true we honor `Retry-After` (capped by `max_wait_seconds`) and retry. A
//! 429 with retry disabled, or a `Retry-After` over the cap, is treated like
//! any other failure and the caller falls back.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use testquorum_api::Client;
use testquorum_api::Error as ApiError;
use testquorum_api::types::CheckTarget;
use testquorum_api::types::CreateTestGroupRequest;
use testquorum_api::types::EpochSecs;
use testquorum_api::types::Run;
use testquorum_api::types::SubmitTestResultsRequest;
use testquorum_api::types::TestManager;
use testquorum_api::types::TestResultDoc;
use testquorum_api::types::TestState;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::Test;
use crate::uploader::GroupContext;

/// Server-side cap is 1024 entries per `submit_test_results` call.
const SUBMIT_CHUNK: usize = 1024;
/// Page size for `/queue`. Below the server cap of 1024 so a slow page fetch
/// doesn't stall the run loop for too long, but big enough that the cost of
/// network round-trips is amortised across many tests.
const PAGE_LIMIT: i32 = 256;
/// Prefetch depth: how many pages the background fetcher may stage ahead of
/// the runner. Two keeps a steady supply without holding the full queue in
/// memory.
const PAGE_BUFFER: usize = 2;

pub(crate) struct RankedRun {
    pub(crate) group: GroupContext,
    pub(crate) pages: PageStream,
}

/// A bounded stream of ranked test batches produced by a background page
/// fetcher. Dropping or `shutdown`ing tears the fetcher down.
pub(crate) struct PageStream {
    rx: mpsc::Receiver<Vec<Test>>,
    handle: JoinHandle<()>,
}

impl PageStream {
    pub(crate) async fn next(&mut self) -> Option<Vec<Test>> {
        self.rx.recv().await
    }

    pub(crate) async fn shutdown(self) {
        drop(self.rx);
        let _ = self.handle.await;
    }
}

#[derive(Debug)]
pub(crate) enum RankerError {
    /// Server suggested a `Retry-After` larger than `max_wait_seconds`.
    RetryAfterTooLong(u64),
    /// Any other API failure (4xx that isn't 429, 5xx, transport).
    Other(String),
}

impl fmt::Display for RankerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RetryAfterTooLong(secs) => write!(
                f,
                "rate limited; server suggested {}s wait exceeds max_wait_seconds",
                secs
            ),
            Self::Other(s) => write!(f, "{}", s),
        }
    }
}

/// Builds a GitHub Actions workflow-run check target from the standard
/// `GITHUB_RUN_ID` and `GITHUB_RUN_ATTEMPT` environment variables.
///
/// Returns `None` when the variables are missing or malformed, which lets the
/// group be created without publishing a commit check.
fn check_target_from_env() -> Option<CheckTarget> {
    let run_id = std::env::var("GITHUB_RUN_ID").ok()?;
    let run_attempt = std::env::var("GITHUB_RUN_ATTEMPT").ok()?;

    Some(CheckTarget::GithubWorkflowRun {
        run_id: run_id.parse().ok()?,
        run_attempt: run_attempt.parse().ok()?,
    })
}

/// Build outcomes already known at discovery time (a tracked `nix://` archive
/// build that a cargo dependency triggered and that failed), keyed by
/// `(test_name, manager)` with the failure's stderr. These are submitted as
/// terminal `Failed` in the same pre-rank batch as the `Discovered` docs, so
/// `/rank` — which may move an un-run test to `Skipped` — leaves them failed.
pub(crate) type PreFailed = HashMap<(String, TestManager), String>;

pub(crate) async fn attempt(
    client: Client,
    repo_id: String,
    run: Run,
    tests: &[Test],
    cfg: &testquorum_config::Cloud,
    pre_failed: &PreFailed,
) -> Result<RankedRun, RankerError> {
    // Step 1: create the group with a GitHub Actions workflow run check target
    // when running inside GitHub Actions so the test group result is published
    // as a commit check on the same pull request / merge queue entry.
    let create_request = CreateTestGroupRequest {
        check_target: check_target_from_env(),
    };
    let group_resp = loop {
        match client
            .create_test_group(&repo_id, Some(&create_request))
            .await
        {
            Ok(r) => break r,
            Err(e) => handle_429(cfg, "create_test_group", e).await?,
        }
    };
    let group_id = group_resp.into_inner().group_id;

    // Step 2: mint UUIDs locally and build Discovered docs. Generating IDs
    // here (rather than reading them back from the response) means the
    // uploader can stitch later transitions onto the same record without an
    // extra lookup.
    let now: EpochSecs = SystemTime::now().into();
    let (docs, instances) = build_submission_docs(tests, pre_failed, &run, group_id, &now);

    // Step 3: submit the Discovered batch in server-sized chunks.
    let upload_start = Instant::now();
    for chunk in docs.chunks(SUBMIT_CHUNK) {
        let req = SubmitTestResultsRequest {
            results: chunk.to_vec(),
        };
        loop {
            match client.submit_test_results(&repo_id, &req).await {
                Ok(_) => break,
                Err(e) => handle_429(cfg, "submit_test_results", e).await?,
            }
        }
    }
    println!(
        "uploaded {} test(s) in {}ms",
        docs.len(),
        upload_start.elapsed().as_millis()
    );

    // Step 4: ask the server to stamp ranks.
    let group_str = group_id.to_string();
    let rank_start = Instant::now();
    loop {
        match client.rank_test_group(&repo_id, &group_str).await {
            Ok(_) => break,
            Err(e) => handle_429(cfg, "rank_test_group", e).await?,
        }
    }
    println!("ranked in {}ms", rank_start.elapsed().as_millis());

    // Step 5: build the lookup the page fetcher needs to map ranked entries
    // back to runnable `Test`s, then spawn the prefetcher.
    let name_to_test: HashMap<(String, TestManager), Test> = tests
        .iter()
        .map(|t| ((t.name.clone(), t.manager.clone()), t.clone()))
        .collect();

    let pages = spawn_page_stream(client, repo_id, group_id, name_to_test, cfg.clone());

    Ok(RankedRun {
        group: GroupContext {
            group_id,
            instances,
        },
        pages,
    })
}

/// The id and mint time assigned to each test, so the uploader can stitch later
/// transitions onto the same record without re-reading it.
type Instances = HashMap<(String, TestManager), (Uuid, EpochSecs)>;

/// Builds the pre-rank submission batch. Every test is `Discovered`, except
/// those in `pre_failed`, which are created straight as terminal `Failed` so
/// `/rank` — which may move an un-run test to `Skipped` — leaves them failed.
/// UUIDs are minted here (rather than read back) so the uploader can stitch
/// later transitions onto the same record.
fn build_submission_docs(
    tests: &[Test],
    pre_failed: &PreFailed,
    run: &Run,
    group_id: Uuid,
    now: &EpochSecs,
) -> (Vec<TestResultDoc>, Instances) {
    let mut instances = Instances::with_capacity(tests.len());
    let mut docs = Vec::with_capacity(tests.len());
    for test in tests {
        let id = Uuid::now_v7();
        let key = (test.name.clone(), test.manager.clone());
        instances.insert(key.clone(), (id, now.clone()));
        let state = match pre_failed.get(&key) {
            Some(stderr) => TestState::Failed {
                discovered_at: now.clone(),
                started_at: now.clone(),
                finished_at: now.clone(),
                duration_ms: 0,
                failure_message: crate::uploader::failure_message_from(stderr),
                stderr: stderr.clone(),
                stdout: None,
            },
            None => TestState::Discovered {
                discovered_at: now.clone(),
            },
        };
        docs.push(TestResultDoc {
            id,
            rank: None,
            run: run.clone(),
            state,
            test_group: Some(group_id),
            test_manager: Some(test.manager.clone()),
            test_name: test.name.clone(),
        });
    }
    (docs, instances)
}

fn spawn_page_stream(
    client: Client,
    repo_id: String,
    group_id: Uuid,
    name_to_test: HashMap<(String, TestManager), Test>,
    cfg: testquorum_config::Cloud,
) -> PageStream {
    let (tx, rx) = mpsc::channel::<Vec<Test>>(PAGE_BUFFER);
    let group_str = group_id.to_string();
    let handle = tokio::spawn(async move {
        let mut cursor: Option<i32> = None;
        loop {
            let resp = loop {
                match client
                    .get_queue_page(&repo_id, &group_str, cursor, Some(PAGE_LIMIT))
                    .await
                {
                    Ok(r) => break r,
                    Err(e) => match handle_429(&cfg, "get_queue_page", e).await {
                        Ok(()) => continue,
                        Err(reason) => {
                            eprintln!(
                                "ranking: queue stream aborted: {}; remaining ranked tests will not run",
                                reason
                            );
                            return;
                        }
                    },
                }
            };
            let resp = resp.into_inner();

            let mut batch: Vec<Test> = Vec::with_capacity(resp.tests.len());
            for doc in &resp.tests {
                let manager = match doc.test_manager.as_ref() {
                    Some(m) => m.clone(),
                    None => continue,
                };
                if let Some(t) = name_to_test.get(&(doc.test_name.clone(), manager)) {
                    batch.push(t.clone());
                }
            }

            if !batch.is_empty() && tx.send(batch).await.is_err() {
                return;
            }

            match resp.next_cursor {
                Some(c) => cursor = Some(c),
                None => return,
            }
        }
    });
    PageStream { rx, handle }
}

/// Inspects an API error and either sleeps for `Retry-After` (returning
/// `Ok(())` so the caller retries) or converts it to a `RankerError`.
async fn handle_429(
    cfg: &testquorum_config::Cloud,
    op_name: &str,
    err: ApiError<()>,
) -> Result<(), RankerError> {
    if err.status() != Some(StatusCode::TOO_MANY_REQUESTS) {
        return Err(RankerError::Other(format!("{}: {}", op_name, err)));
    }
    let retry_after = read_retry_after(&err).unwrap_or(Duration::from_secs(1));
    if retry_after.as_secs() > cfg.max_wait_seconds {
        return Err(RankerError::RetryAfterTooLong(retry_after.as_secs()));
    }
    eprintln!(
        "ranking: {} rate limited, retrying in {}s",
        op_name,
        retry_after.as_secs()
    );
    tokio::time::sleep(retry_after).await;
    Ok(())
}

fn read_retry_after(err: &ApiError<()>) -> Option<Duration> {
    let header = match err {
        ApiError::ErrorResponse(rv) => rv.headers().get(RETRY_AFTER)?,
        ApiError::UnexpectedResponse(resp) => resp.headers().get(RETRY_AFTER)?,
        _ => return None,
    };
    let s = header.to_str().ok()?;
    s.parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranker_error_display_includes_retry_after_seconds() {
        let e = RankerError::RetryAfterTooLong(120);
        let s = format!("{}", e);
        assert!(s.contains("120"));
        assert!(s.contains("max_wait_seconds"));
    }

    #[test]
    fn ranker_error_display_for_other_passes_through() {
        let e = RankerError::Other("rank_test_group: 500 server error".to_string());
        assert_eq!(format!("{}", e), "rank_test_group: 500 server error");
    }

    use testquorum_api::types::Commit;
    use testquorum_api::types::RunKind;
    use testquorum_api::types::WellKnownTestManager;

    use crate::Test;

    fn test(name: &str, manager: WellKnownTestManager) -> Test {
        Test {
            name: name.to_string(),
            manager: manager.into(),
            payload: serde_json::Value::Null,
        }
    }

    #[test]
    fn pre_failed_tests_submit_terminal_before_rank() {
        let run = Run {
            head: Commit {
                sha: "a".to_string(),
                height: 1,
            },
            kind: RunKind::Diff {
                merge_base: Commit {
                    sha: "b".to_string(),
                    height: 0,
                },
            },
        };
        let tests = vec![
            test("archive", WellKnownTestManager::Nix),
            test("pkg::unit", WellKnownTestManager::Cargo),
        ];
        let mut pre_failed = PreFailed::new();
        pre_failed.insert(
            ("archive".to_string(), WellKnownTestManager::Nix.into()),
            "boom: build failed".to_string(),
        );

        let now: EpochSecs = SystemTime::now().into();
        let (docs, instances) =
            build_submission_docs(&tests, &pre_failed, &run, Uuid::now_v7(), &now);

        // The pre-failed test is created straight as terminal Failed — so a
        // later `/rank` can't move it to Skipped — carrying its stderr.
        let archive = docs.iter().find(|d| d.test_name == "archive").unwrap();
        match &archive.state {
            TestState::Failed {
                stderr,
                failure_message,
                ..
            } => {
                assert_eq!(stderr, "boom: build failed");
                assert_eq!(failure_message, "boom: build failed");
            }
            other => panic!("expected terminal Failed, got {:?}", other),
        }

        // Everything else stays Discovered for ranking.
        let unit = docs.iter().find(|d| d.test_name == "pkg::unit").unwrap();
        assert!(matches!(unit.state, TestState::Discovered { .. }));

        // Every test — failed or not — is minted for later stitching.
        assert_eq!(instances.len(), 2);
    }
}
