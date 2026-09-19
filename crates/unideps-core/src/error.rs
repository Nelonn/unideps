use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization/Deserialization error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("Serialization toml error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("Target parsing error: {0}")]
    TargetParse(String),

    #[error("Manifest error: {0}")]
    Manifest(String),

    #[error("Dependency cycle detected: {0}")]
    CycleDetected(String),

    #[error("Dependency not found: {0}")]
    DependencyNotFound(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
