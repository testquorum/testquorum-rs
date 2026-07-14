use super::OcamlError;

pub(crate) fn detect_ocaml(dune_project_path: Option<&str>) -> Result<String, OcamlError> {
    which::which("opam").map_err(|_| OcamlError::OpamNotFound)?;

    let resolved = dune_project_path.unwrap_or("dune-project");
    if !std::path::Path::new(resolved).exists() {
        return Err(OcamlError::DuneProjectNotFound {
            path: resolved.to_string(),
        });
    }

    Ok(resolved.to_string())
}
