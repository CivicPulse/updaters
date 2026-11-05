//! Deployment admission may lower the source contract's caps, never raise them.
use crate::{Error, Result};
use civpulse_results_source::Limits;

#[derive(Clone, Copy)]
pub struct Admission {
    pub download_bytes: u64,
    pub scope_bytes: usize,
}
impl Admission {
    /// Validate deployment caps against the pinned source contract.
    /// # Errors
    /// Rejects zero, raised, or internally inconsistent caps.
    pub fn new(download_bytes: u64, scope_bytes: usize) -> Result<Self> {
        let hard = Limits::default();
        if download_bytes == 0
            || scope_bytes == 0
            || download_bytes > hard.max_download
            || scope_bytes > hard.max_scope
            || scope_bytes as u64 > download_bytes
        {
            return Err(Error::Config(
                "Admission caps must be positive, lower-only, and scope <= download",
            ));
        }
        Ok(Self {
            download_bytes,
            scope_bytes,
        })
    }
    /// Read optional lower-only caps; defaults retain the reviewed 512/64 MiB contract.
    /// # Errors
    /// Rejects malformed values and caps outside the supported contract.
    pub fn read(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self> {
        let defaults = Limits::default();
        let download =
            value("SOS_MAX_DOWNLOAD_BYTES").map_or(Ok(defaults.max_download), |text| {
                text.parse::<u64>()
                    .map_err(|_| Error::Config("Invalid raw admission cap"))
            })?;
        let scope = value("SOS_MAX_SCOPE_BYTES").map_or(Ok(defaults.max_scope), |text| {
            text.parse::<usize>()
                .map_err(|_| Error::Config("Invalid scope admission cap"))
        })?;
        Self::new(download, scope)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lower_caps_are_explicit_and_cannot_raise_the_contract() {
        let default = Admission::read(|_| None).unwrap();
        assert_eq!(default.download_bytes, 512 * 1024 * 1024);
        assert_eq!(default.scope_bytes, 64 * 1024 * 1024);
        let lower = Admission::read(|key| {
            Some(
                match key {
                    "SOS_MAX_DOWNLOAD_BYTES" => "134217728",
                    _ => "16777216",
                }
                .into(),
            )
        })
        .unwrap();
        assert_eq!(lower.download_bytes, 128 * 1024 * 1024);
        assert_eq!(lower.scope_bytes, 16 * 1024 * 1024);
        for (raw, scope) in [
            (0, 1),
            (1, 0),
            (1, 2),
            (513 * 1024 * 1024, 64 * 1024 * 1024),
            (512 * 1024 * 1024, 65 * 1024 * 1024),
        ] {
            assert!(Admission::new(raw, scope).is_err());
        }
        assert!(Admission::read(|_| Some("-1".into())).is_err());
    }
}
