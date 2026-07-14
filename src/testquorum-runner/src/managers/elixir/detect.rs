use super::ElixirError;

pub(crate) fn detect_elixir(mix_exs_path: Option<&str>) -> Result<String, ElixirError> {
    which::which("mix").map_err(|_| ElixirError::NotFound)?;

    let resolved = mix_exs_path.unwrap_or("mix.exs");
    if !std::path::Path::new(resolved).exists() {
        return Err(ElixirError::MixExsNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}
