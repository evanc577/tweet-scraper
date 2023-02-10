use std::fmt::Display;

use chromiumoxide::error::CdpError;

#[derive(Debug)]
pub enum Error {
    Internal(String),
    Cdp(CdpError),
    NoGuestToken,
    InvalidGuestToken,
    TweetParse(String),
    BadStatus(u16),
    Network(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal(s) => write!(f, "internal error: {}", s),
            Self::Cdp(e) => write!(f, "CDP error: {}", e),
            Self::NoGuestToken => write!(f, "no gueset token"),
            Self::InvalidGuestToken => write!(f, "invalid gueset token"),
            Self::TweetParse(s) => write!(f, "could not parse tweet: {}", s),
            Self::BadStatus(c) => write!(f, "api returned status code: {}", c),
            Self::Network(s) => write!(f, "network error: {}", s),
        }
    }
}

impl std::error::Error for Error {}
