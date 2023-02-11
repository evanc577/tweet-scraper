use std::fmt::Display;
use std::path::{Path, PathBuf};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

pub async fn save_headers(
    headers: &HeaderMap,
    path: impl AsRef<Path>,
) -> Result<(), std::io::Error> {
    let file = File::create(path.as_ref()).await?;
    let mut writer = BufWriter::new(file);

    // Write one header per line in "name=value" format
    for (k, v) in headers {
        writer.write_all(k.as_str().as_bytes()).await?;
        writer.write_all(b"=").await?;
        writer.write_all(v.as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }
    writer.flush().await?;

    Ok(())
}

pub async fn load_headers(path: impl AsRef<Path>) -> Result<HeaderMap, LoadHeadersError> {
    let file = File::open(path.as_ref())
        .await
        .map_err(LoadHeadersError::Io)?;
    let mut reader = BufReader::new(file).lines();
    let mut headers = HeaderMap::new();

    // Iterate over lines of input file
    while let Some(line) = reader.next_line().await.map_err(LoadHeadersError::Io)? {
        // Ignore empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Parse line
        if let Some((k, v)) = line.split_once('=') {
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| LoadHeadersError::Parse(e.to_string()))?;
            let value = HeaderValue::from_bytes(v.as_bytes())
                .map_err(|e| LoadHeadersError::Parse(e.to_string()))?;
            headers.insert(name, value);
        } else {
            return Err(LoadHeadersError::Parse("invalid line no '=' found".into()));
        }
    }

    Ok(headers)
}

#[derive(Debug)]
pub enum PersistHeadersError {
    Load(LoadHeadersError, PathBuf),
    Save(std::io::Error, PathBuf),
}

impl Display for PersistHeadersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(l, p) => write!(
                f,
                "could not load persisted headers from file {:?}: {}",
                p, l
            ),
            Self::Save(e, p) => write!(f, "could not save headers to file {:?}: {}", p, e),
        }
    }
}

#[derive(Debug)]
pub enum LoadHeadersError {
    Io(std::io::Error),
    Parse(String),
}

impl Display for LoadHeadersError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{}", e),
            Self::Parse(s) => write!(f, "{}", s),
        }
    }
}
