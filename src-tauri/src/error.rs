use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("not an archive")]
    NotAnArchive,
    #[error("password required")]
    PasswordRequired,
    #[error("wrong password")]
    WrongPassword,
    #[error("missing volume: {0}")]
    MissingVolume(String),
    #[error("corrupt archive: {0}")]
    Corrupt(String),
    #[error("disk full")]
    DiskFull,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    #[error("destination conflict: {0}")]
    DestConflict(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("cancelled")]
    Cancelled,
    #[error("io error: {0}")]
    Io(String),
}

impl ArchiveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAnArchive => "not_archive",
            Self::PasswordRequired => "password_required",
            Self::WrongPassword => "wrong_password",
            Self::MissingVolume(_) => "missing_volume",
            Self::Corrupt(_) => "corrupt",
            Self::DiskFull => "disk_full",
            Self::PermissionDenied(_) => "permission",
            Self::UnsafePath(_) => "unsafe_path",
            Self::DestConflict(_) => "dest_conflict",
            Self::Unsupported(_) => "unsupported",
            Self::Cancelled => "cancelled",
            Self::Io(_) => "io",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::MissingVolume(s)
            | Self::Corrupt(s)
            | Self::PermissionDenied(s)
            | Self::UnsafePath(s)
            | Self::DestConflict(s)
            | Self::Unsupported(s)
            | Self::Io(s) => s.clone(),
            _ => String::new(),
        }
    }
}

impl Serialize for ArchiveError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("ArchiveError", 2)?;
        st.serialize_field("code", self.code())?;
        st.serialize_field("detail", &self.detail())?;
        st.end()
    }
}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::StorageFull => Self::DiskFull,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied(e.to_string()),
            _ => Self::Io(e.to_string()),
        }
    }
}
