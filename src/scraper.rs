use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::network::{Cookie, SetUserAgentOverrideParams};
use chromiumoxide::{Browser, BrowserConfig};
use futures_util::stream::StreamExt;
use futures_util::Stream;
use once_cell::sync::Lazy;
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::error::Error;
use crate::header_persist::{load_headers, save_headers, PersistHeadersError};

pub struct TweetScraper {
    client: Client,
    fetch_state: FetchState,
}

pub enum HeaderPersist {
    Load(PathBuf),
    Save(PathBuf),
    None,
}

// State during stream iteration
#[derive(Default)]
struct FetchState {
    tweets: VecDeque<Value>,
    query: String,
    limit: Option<usize>,
    min_id: Option<u128>,
    tweets_count: usize,
    cursor: Option<String>,
    errored: bool,
}

#[derive(Debug)]
struct BrowserData {
    cookies: Vec<Cookie>,
}

static USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/109.0.0.0 Safari/537.36";
static ACCEPT_VALUE: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.9";
static AUTHORIZATION_VALUE: &str = "Bearer AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

impl TweetScraper {
    pub async fn initialize(header_persist: HeaderPersist) -> Result<Self, Error> {
        // If requested, load headers from file, otherwise spawn chromium process to get headers
        let headers = if let HeaderPersist::Load(p) = &header_persist {
            // Load headers from file
            load_headers(&p)
                .await
                .map_err(|e| Error::PersistHeaders(PersistHeadersError::Load(e, p.clone())))?
        } else {
            // Load headers from chromium
            let browser_data = browser_data().await?;
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT, HeaderValue::from_static(ACCEPT_VALUE));
            headers.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_static("gzip, deflate, br"),
            );
            headers.insert(
                header::ACCEPT_LANGUAGE,
                HeaderValue::from_static("en-US,en;q=0.9"),
            );
            headers.insert(
                header::UPGRADE_INSECURE_REQUESTS,
                HeaderValue::from_static("1"),
            );
            headers.insert(
                header::AUTHORIZATION,
                HeaderValue::from_static(AUTHORIZATION_VALUE),
            );
            let guest_token = &browser_data
                .cookies
                .iter()
                .find(|c| c.name == "gt")
                .ok_or_else(|| Error::NoGuestToken)?
                .value;
            headers.insert(
                "x-guest-token",
                HeaderValue::from_str(guest_token).map_err(|_| Error::InvalidGuestToken)?,
            );
            headers.insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
            headers
        };

        // Save headers
        if let HeaderPersist::Save(p) = &header_persist {
            save_headers(&headers, &p)
                .await
                .map_err(|e| Error::PersistHeaders(PersistHeadersError::Save(e, p.clone())))?;
        }

        let client = Client::builder()
            .default_headers(headers)
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .build()
            .map_err(|_| Error::Internal("could not build reqwest client".into()))?;

        Ok(Self {
            client,
            fetch_state: Default::default(),
        })
    }

    pub async fn tweets(
        &mut self,
        query: impl AsRef<str>,
        limit: Option<usize>,
        min_id: Option<u128>,
    ) -> impl Stream<Item = Result<Value, Error>> + '_ {
        // Reset internal state
        self.fetch_state = FetchState {
            query: query.as_ref().to_owned(),
            limit,
            min_id,
            ..Default::default()
        };

        futures_util::stream::unfold(self, |state| async {
            // Stop if previously errored
            if state.fetch_state.errored {
                return None;
            }

            // Stop if limit number reached
            if let Some(limit) = state.fetch_state.limit {
                if state.fetch_state.tweets_count >= limit {
                    return None;
                }
            }

            let mut should_return_tweet = |tweet| {
                // Stop if minimum tweet id reached
                if let Some(min_id) = state.fetch_state.min_id {
                    let parse_id = |tweet: &Value| -> Result<u128, Error> {
                        let id = tweet["id_str"]
                            .as_str()
                            .ok_or_else(|| Error::TweetParse("no id_str key".into()))?
                            .parse()
                            .map_err(|e| Error::TweetParse(format!("invalid id_str: {e}")))?;
                        Ok(id)
                    };
                    match parse_id(&tweet) {
                        Ok(id) => {
                            if id < min_id {
                                return None;
                            }
                        }
                        Err(e) => {
                            state.fetch_state.errored = true;
                            return Some(Err(e));
                        }
                    }
                }

                // Return next tweet
                state.fetch_state.tweets_count += 1;
                Some(Ok(tweet))
            };

            // Try returning the next tweet if available
            if let Some(tweet) = state.fetch_state.tweets.pop_front() {
                if let Some(r) = should_return_tweet(tweet) {
                    return Some((r, state));
                }
            }

            // Scrape Twitter
            match query_twitter(
                &state.client,
                state.fetch_state.query.as_str(),
                state.fetch_state.cursor.as_deref(),
            )
            .await
            {
                Ok((tweets, cursor)) => {
                    state.fetch_state.tweets.extend(tweets.into_iter());
                    state.fetch_state.cursor = Some(cursor);
                }
                Err(e) => {
                    state.fetch_state.errored = true;
                    return Some((Err(e), state));
                }
            }

            // Try returning the next tweet if available
            if let Some(tweet) = state.fetch_state.tweets.pop_front() {
                if let Some(r) = should_return_tweet(tweet) {
                    return Some((r, state));
                }
            }

            None
        })
    }
}

/// Get cookies for twitter.com
async fn browser_data() -> Result<BrowserData, Error> {
    let (mut browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            // Sometimes twitter webpage hangs if window is larger???
            .window_size(800, 600)
            .build()
            .map_err(Error::Internal)?,
    )
    .await
    .map_err(Error::Cdp)?;

    let browser_handler =
        tokio::task::spawn(async move { while handler.next().await.is_some() {} });

    let page = Arc::new(
        browser
            .start_incognito_context()
            .await
            .map_err(Error::Cdp)?
            .new_page("about:blank")
            .await
            .map_err(Error::Cdp)?,
    );

    page.set_user_agent(SetUserAgentOverrideParams::new(USER_AGENT))
        .await
        .map_err(Error::Cdp)?;

    // Navigate to website to extract cookies
    page.goto("https://twitter.com/explore")
        .await
        .map_err(Error::Cdp)?;
    page.wait_for_navigation().await.map_err(Error::Cdp)?;

    let cookies = page.get_cookies().await.map_err(Error::Cdp)?;

    browser.close().await.map_err(Error::Cdp)?;
    _ = browser_handler.await;

    Ok(BrowserData { cookies })
}

async fn query_twitter(
    client: &Client,
    query: impl AsRef<str>,
    cursor: Option<&str>,
) -> Result<(Vec<Value>, String), Error> {
    static URL: &str = "https://api.twitter.com/2/search/adaptive.json";

    let mut url = Url::parse(URL).map_err(|_| Error::Internal("could not parse api url".into()))?;
    url.query_pairs_mut()
        .clear()
        .append_pair("include_profile_interstitial_type", "1")
        .append_pair("include_blocking", "1")
        .append_pair("include_blocked_by", "1")
        .append_pair("include_followed_by", "1")
        .append_pair("include_want_retweets", "1")
        .append_pair("include_mute_edge", "1")
        .append_pair("include_can_dm", "1")
        .append_pair("include_can_media_tag", "1")
        .append_pair("skip_status", "1")
        .append_pair("cards_platform", "Web-12")
        .append_pair("include_cards", "1")
        .append_pair("include_ext_alt_text", "true")
        .append_pair("include_quote_count", "true")
        .append_pair("include_reply_count", "1")
        .append_pair("tweet_mode", "extended")
        .append_pair("include_entities", "true")
        .append_pair("include_user_entities", "true")
        .append_pair("include_ext_media_color", "true")
        .append_pair("include_ext_media_availability", "true")
        .append_pair("send_error_codes", "true")
        .append_pair("simple_quoted_tweet", "true")
        .append_pair("query_source", "typed_query")
        .append_pair("pc", "1")
        .append_pair("spelling_corrections", "1")
        .append_pair("ext", "mediaStats%2ChighlightedLabel")
        .append_pair("count", "20")
        .append_pair("tweet_search_mode", "live")
        .append_pair("q", query.as_ref());

    if let Some(cursor) = cursor {
        url.query_pairs_mut().append_pair("cursor", cursor);
    }

    static RETRY_STATUS: Lazy<Vec<StatusCode>> =
        Lazy::new(|| [StatusCode::TOO_MANY_REQUESTS, StatusCode::REQUEST_TIMEOUT].into());
    let json = loop {
        let response = client
            .get(url.as_str())
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
        if response.status().is_success() {
            break response
                .json::<Value>()
                .await
                .map_err(|e| Error::TweetParse(e.to_string()))?;
        }

        if response.status().is_server_error() || RETRY_STATUS.contains(&response.status()) {
            eprintln!(
                "received response status code: {}, waiting 60 seconds",
                response.status().as_u16()
            );
            tokio::time::sleep(Duration::from_secs(60)).await;
        } else {
            return Err(Error::BadStatus(response.status().as_u16()));
        }
    };

    parse_tweets(json)
}

fn parse_tweets(json: Value) -> Result<(Vec<Value>, String), Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Root {
        global_objects: GlobalObjects,
        timeline: Value,
    }

    #[derive(Deserialize)]
    struct GlobalObjects {
        tweets: BTreeMap<String, Value>,
        users: BTreeMap<String, Value>,
    }

    let root: Root = serde_json::from_value(json).map_err(|e| Error::TweetParse(e.to_string()))?;

    // Add user object to every tweet
    let mut tweets = root.global_objects.tweets;
    let users = root.global_objects.users;
    for (_, tweet) in tweets.iter_mut() {
        if let Some(tweet) = tweet.as_object_mut() {
            if let Some(user_id_str) = tweet["user_id_str"].as_str() {
                if let Some(user) = users.get(user_id_str) {
                    tweet.insert("user".to_owned(), user.clone());
                }
            }
        }
    }
    let tweets: Vec<_> = tweets.into_values().rev().collect();

    // Parse cursor
    let timeline_str =
        serde_json::to_string(&root.timeline).map_err(|e| Error::TweetParse(e.to_string()))?;
    let cursor_re = regex::Regex::new(r#""(scroll:.+?)""#)
        .map_err(|_| Error::Internal("could not build regex".into()))?;
    let cursor = cursor_re
        .captures(&timeline_str)
        .ok_or_else(|| Error::TweetParse("could not find cursor".into()))?
        .get(1)
        .unwrap()
        .as_str()
        .to_owned();

    Ok((tweets, cursor))
}
