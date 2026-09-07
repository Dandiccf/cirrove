//! Safe presentation causes. Never retain raw settings, response bodies or URLs.
use std::io::ErrorKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsFailure {
    Read(ErrorKind),
    InvalidFormat { line: usize, column: usize },
    InvalidConfiguration,
    WorkerUnavailable,
}
impl SettingsFailure {
    pub(super) fn from_error(error: anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            Self::Read(error.kind())
        } else if let Some(error) = error.downcast_ref::<serde_json::Error>() {
            Self::InvalidFormat {
                line: error.line(),
                column: error.column(),
            }
        } else {
            Self::InvalidConfiguration
        }
    }
    pub fn description(&self) -> String {
        match self {
            Self::Read(ErrorKind::PermissionDenied) =>
                "Cirrove does not have permission to read your saved connections. Check access to the settings directory, then retry.".into(),
            Self::Read(ErrorKind::IsADirectory) =>
                "The accounts settings path is a directory instead of a file. Restore the settings file, then retry.".into(),
            Self::Read(_) =>
                "The saved connections could not be read from disk. Check that the settings location is accessible, then retry.".into(),
            Self::InvalidFormat { line, column } => format!(
                "The settings file has invalid data at line {line}, column {column}. Restore a valid settings file, then retry. Your cloud files have not been changed."
            ),
            Self::InvalidConfiguration =>
                "The saved connections contain unsupported or invalid settings. Check the Cirrove version and restore a valid settings file, then retry.".into(),
            Self::WorkerUnavailable =>
                "The task reading your saved connections stopped unexpectedly. Retry or reopen Cirrove.".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceFailure {
    Io(ErrorKind),
    TimedOut,
    InvalidResponse,
    Incompatible { expected: u32, actual: u32 },
}
impl ServiceFailure {
    pub(super) fn from_error(error: anyhow::Error) -> Self {
        if error.is::<tokio::time::error::Elapsed>() {
            Self::TimedOut
        } else if let Some(error) = error.downcast_ref::<std::io::Error>() {
            Self::Io(error.kind())
        } else {
            // The bounded local status reader's other failures are malformed
            // JSON or an oversized response. Do not copy its raw error text.
            Self::InvalidResponse
        }
    }
    pub fn description(&self) -> &'static str {
        match self {
            Self::Io(ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
                "The Cirrove service is not running at this location. Start the service, then retry."
            }
            Self::Io(ErrorKind::PermissionDenied) => {
                "Access to the Cirrove service was denied. Check the service permissions, then retry."
            }
            Self::TimedOut | Self::Io(ErrorKind::TimedOut) => {
                "The Cirrove service did not respond in time. Check the service, then retry."
            }
            Self::Io(_) => {
                "The connection to the Cirrove service failed. Check the service, then retry."
            }
            Self::InvalidResponse => {
                "The Cirrove service returned an invalid response. Check that the desktop and service versions match."
            }
            Self::Incompatible { .. } => {
                "This Cirrove service version is incompatible. Update the desktop and service together, then retry."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_kinds_survive_context_without_copying_sensitive_messages() {
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::IsADirectory,
            ErrorKind::ConnectionReset,
        ] {
            let error = || {
                anyhow::Error::new(std::io::Error::new(
                    kind,
                    "PRIVATE CONTENT https://signed.invalid",
                ))
                .context("PRIVATE PATH")
            };
            let settings = SettingsFailure::from_error(error());
            let service = ServiceFailure::from_error(error());
            assert_eq!(settings, SettingsFailure::Read(kind));
            assert_eq!(service, ServiceFailure::Io(kind));
            let visible = format!(
                "{settings:?} {} {service:?} {}",
                settings.description(),
                service.description()
            );
            assert!(!visible.contains("PRIVATE") && !visible.contains("signed.invalid"));
        }
    }
}
