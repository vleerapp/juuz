pub const MEMBER_LIBRARY_ROOT: &str = "/libraries";

pub fn allowed_path(
    cfg: &crate::server::utils::config::Config,
    path: &std::path::Path,
    admin: bool,
) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    if let Ok(data) = std::fs::canonicalize(&cfg.data_dir)
        && (path.starts_with(&data) || data.starts_with(&path))
    {
        return false;
    }
    admin || std::fs::canonicalize(MEMBER_LIBRARY_ROOT).is_ok_and(|root| path.starts_with(root))
}
