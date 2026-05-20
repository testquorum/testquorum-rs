use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

const TWIRP_BASE: &str = "twirp/github.actions.results.api.v1.ArtifactService";
const AZURE_BLOB_API_VERSION: &str = "2020-04-08";

#[derive(Debug, Clone)]
pub(super) struct BackendIds {
    pub workflow_run_backend_id: String,
    pub workflow_job_run_backend_id: String,
}

/// Parse the `Actions.Results:<runId>:<jobId>` scope from an ACTIONS_RUNTIME_TOKEN JWT.
/// We do not verify the signature — the toolkit doesn't either, the IDs are non-secret
/// routing keys, and the artifact service itself re-validates the token.
pub(super) fn backend_ids_from_token(token: &str) -> Result<BackendIds, anyhow::Error> {
    let payload_b64 = token
        .split('.')
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("ACTIONS_RUNTIME_TOKEN is not a JWT"))?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| anyhow::anyhow!("ACTIONS_RUNTIME_TOKEN payload is not base64url"))?;

    #[derive(Deserialize)]
    struct Claims {
        scp: String,
    }
    let claims: Claims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| anyhow::anyhow!("ACTIONS_RUNTIME_TOKEN payload is not JSON"))?;

    for scope in claims.scp.split(' ') {
        let mut parts = scope.split(':');
        if parts.next() != Some("Actions.Results") {
            continue;
        }
        let run = parts.next();
        let job = parts.next();
        if let (Some(run), Some(job)) = (run, job) {
            return Ok(BackendIds {
                workflow_run_backend_id: run.to_string(),
                workflow_job_run_backend_id: job.to_string(),
            });
        }
    }
    Err(anyhow::anyhow!(
        "ACTIONS_RUNTIME_TOKEN scp claim missing Actions.Results entry"
    ))
}

#[derive(Serialize)]
struct CreateArtifactRequest<'a> {
    workflow_run_backend_id: &'a str,
    workflow_job_run_backend_id: &'a str,
    name: &'a str,
    version: u32,
}

#[derive(Deserialize)]
struct CreateArtifactResponse {
    signed_upload_url: String,
}

#[derive(Serialize)]
struct FinalizeArtifactRequest<'a> {
    workflow_run_backend_id: &'a str,
    workflow_job_run_backend_id: &'a str,
    name: &'a str,
    size: String,
    hash: String,
}

/// Upload `body` as the sole file (named `filename`) inside a GitHub Actions
/// artifact named `name`. Implements the bare minimum of the v2 protocol: one
/// Twirp CreateArtifact call, a single-shot block-blob PUT, one FinalizeArtifact.
///
/// `results_url` is expected to be the value of `ACTIONS_RESULTS_URL` and must
/// end in a trailing slash (it always does in practice).
pub(super) async fn upload_text_artifact(
    http: &reqwest::Client,
    results_url: &str,
    runtime_token: &str,
    backend_ids: &BackendIds,
    name: &str,
    filename: &str,
    body: &str,
) -> Result<(), anyhow::Error> {
    let archive = build_single_file_zip(filename, body.as_bytes());
    let hash = format!("sha256:{}", hex_lower(&Sha256::digest(&archive)));

    let create_url = format!("{}{}/CreateArtifact", results_url, TWIRP_BASE);
    let create_req = CreateArtifactRequest {
        workflow_run_backend_id: &backend_ids.workflow_run_backend_id,
        workflow_job_run_backend_id: &backend_ids.workflow_job_run_backend_id,
        name,
        version: 4,
    };
    let create_resp = http
        .post(&create_url)
        .bearer_auth(runtime_token)
        .json(&create_req)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("CreateArtifact request failed: {}", e))?;
    if !create_resp.status().is_success() {
        let status = create_resp.status();
        let body = create_resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "CreateArtifact returned {}: {}",
            status,
            body
        ));
    }
    let create_body: CreateArtifactResponse = create_resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("CreateArtifact response not JSON: {}", e))?;

    let put_resp = http
        .put(&create_body.signed_upload_url)
        .header("x-ms-blob-type", "BlockBlob")
        .header("x-ms-version", AZURE_BLOB_API_VERSION)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(archive.clone())
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("artifact blob PUT failed: {}", e))?;
    if !put_resp.status().is_success() {
        let status = put_resp.status();
        let body = put_resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("blob PUT returned {}: {}", status, body));
    }

    let finalize_url = format!("{}{}/FinalizeArtifact", results_url, TWIRP_BASE);
    let finalize_req = FinalizeArtifactRequest {
        workflow_run_backend_id: &backend_ids.workflow_run_backend_id,
        workflow_job_run_backend_id: &backend_ids.workflow_job_run_backend_id,
        name,
        size: archive.len().to_string(),
        hash,
    };
    let finalize_resp = http
        .post(&finalize_url)
        .bearer_auth(runtime_token)
        .json(&finalize_req)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("FinalizeArtifact request failed: {}", e))?;
    if !finalize_resp.status().is_success() {
        let status = finalize_resp.status();
        let body = finalize_resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "FinalizeArtifact returned {}: {}",
            status,
            body
        ));
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Build a minimal uncompressed ZIP (store-only) holding a single file. Artifacts
/// v2 expects a ZIP archive as the uploaded blob; the artifact content is the file
/// inside it, not the raw bytes.
fn build_single_file_zip(filename: &str, content: &[u8]) -> Vec<u8> {
    let crc = crc32(content);
    let name_bytes = filename.as_bytes();
    let mut out = Vec::with_capacity(name_bytes.len() * 2 + content.len() + 100);

    // Local file header
    let local_header_offset = out.len() as u32;
    out.extend_from_slice(&0x04034b50u32.to_le_bytes()); // signature
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // gp flag
    out.extend_from_slice(&0u16.to_le_bytes()); // method = stored
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(content.len() as u32).to_le_bytes()); // compressed size
    out.extend_from_slice(&(content.len() as u32).to_le_bytes()); // uncompressed size
    out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra length
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(content);

    // Central directory header
    let central_offset = out.len() as u32;
    out.extend_from_slice(&0x02014b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // version made by
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // gp flag
    out.extend_from_slice(&0u16.to_le_bytes()); // method
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(&(content.len() as u32).to_le_bytes());
    out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra length
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
    out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
    out.extend_from_slice(&local_header_offset.to_le_bytes());
    out.extend_from_slice(name_bytes);

    let central_size = (out.len() as u32) - central_offset;

    // End of central directory
    out.extend_from_slice(&0x06054b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // disk where central dir starts
    out.extend_from_slice(&1u16.to_le_bytes()); // entries on this disk
    out.extend_from_slice(&1u16.to_le_bytes()); // total entries
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length

    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backend_ids_from_real_shape_token() {
        // Header + payload + signature; only payload matters here.
        let payload = serde_json::json!({
            "scp": "Actions.GenericRead:abc Actions.Results:run-id-123:job-id-456 Actions.UploadArtifacts:foo"
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string());
        let token = format!("header.{}.sig", payload_b64);
        let ids = backend_ids_from_token(&token).unwrap();
        assert_eq!(ids.workflow_run_backend_id, "run-id-123");
        assert_eq!(ids.workflow_job_run_backend_id, "job-id-456");
    }

    #[test]
    fn rejects_token_without_results_scope() {
        let payload = serde_json::json!({"scp": "Actions.GenericRead:abc"});
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string());
        let token = format!("header.{}.sig", payload_b64);
        assert!(backend_ids_from_token(&token).is_err());
    }

    #[test]
    fn rejects_non_jwt_token() {
        assert!(backend_ids_from_token("not-a-jwt").is_err());
    }

    #[test]
    fn zip_archive_has_expected_layout() {
        // CRC, EOCD signature, and the file content should all appear.
        let zip = build_single_file_zip("challenge.txt", b"hello");
        assert!(zip.windows(4).any(|w| w == 0x04034b50u32.to_le_bytes()));
        assert!(zip.windows(4).any(|w| w == 0x02014b50u32.to_le_bytes()));
        assert!(zip.windows(4).any(|w| w == 0x06054b50u32.to_le_bytes()));
        assert!(zip.windows(5).any(|w| w == b"hello"));
        assert!(zip.windows(13).any(|w| w == b"challenge.txt"));
    }

    #[test]
    fn crc32_matches_known_vector() {
        // CRC-32 of "123456789" is 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }
}
