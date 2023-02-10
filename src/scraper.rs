use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chromiumoxide::cdp::browser_protocol::fetch;
use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures_util::stream::StreamExt;
use futures_util::Stream;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

pub struct TweetScraper {
    #[allow(dead_code)]
    browser: Browser,
    page: Arc<Page>,

    // State during stream iteration
    tweets: VecDeque<Value>,
    query: String,
    limit: Option<usize>,
    tweets_count: usize,
    cursor: Option<String>,
    errored: bool,
}

impl TweetScraper {
    pub async fn initialize() -> Result<Self> {
        let mut browser = setup_browser().await?;
        let page = setup_interception(&mut browser).await?;
        Ok(Self {
            browser,
            page,
            tweets: Default::default(),
            query: Default::default(),
            limit: None,
            tweets_count: 0,
            cursor: Default::default(),
            errored: false,
        })
    }

    pub async fn tweets<'a>(
        &'a mut self,
        query: impl AsRef<str>,
        limit: Option<usize>,
    ) -> impl Stream<Item = Result<Value>> + 'a {
        self.tweets.clear();
        self.query = query.as_ref().into();
        self.limit = limit;
        self.tweets_count = 0;
        self.cursor = None;
        self.errored = false;

        futures_util::stream::unfold(self, |state| async {
            if state.errored {
                return None;
            }

            if let Some(limit) = state.limit {
                if state.tweets_count >= limit {
                    return None;
                }
            }

            if let Some(tweet) = state.tweets.pop_front() {
                state.tweets_count += 1;
                return Some((Ok(tweet), state));
            }

            match query_twitter(
                state.page.clone(),
                state.query.as_str(),
                state.cursor.as_deref(),
            )
            .await
            {
                Ok((tweets, cursor)) => {
                    state.tweets.extend(tweets.into_iter());
                    state.cursor = Some(cursor);
                }
                Err(e) => {
                    state.errored = true;
                    return Some((Err(e), state));
                }
            }

            if let Some(tweet) = state.tweets.pop_front() {
                state.tweets_count += 1;
                return Some((Ok(tweet), state));
            }

            None
        })
    }
}

async fn setup_browser() -> Result<Browser> {
    // Spawn browser
    let (browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .window_size(800, 600)
            .build()
            .map_err(|s| anyhow!(s))?,
    )
    .await?;

    tokio::task::spawn(async move { while handler.next().await.is_some() {} });

    Ok(browser)
}

/// Setup request interception to add headers
async fn setup_interception(browser: &mut Browser) -> Result<Arc<Page>> {
    let page = Arc::new(
        browser
            .start_incognito_context()
            .await?
            .new_page("about:blank")
            .await?,
    );

    // First navigate to website to extract cookies
    page.goto("https://twitter.com/explore").await?;
    page.wait_for_navigation().await?;

    let cookies: Vec<_> = page
        .get_cookies()
        .await?
        .into_iter()
        .map(|c| {
            CookieParam::builder()
                .name(c.name)
                .value(c.value)
                .domain("api.twitter.com")
                .build()
                .unwrap()
        })
        .collect();

    let guest_token = cookies
        .iter()
        .find(|c| c.name == "gt")
        .ok_or_else(|| anyhow!("no guest token"))?
        .value
        .clone();

    page.goto("about:blank").await?;
    page.wait_for_navigation().await?;

    page.set_cookies(cookies).await?;

    page.execute(fetch::EnableParams {
        patterns: vec![
            fetch::RequestPattern {
                url_pattern: "https://api.twitter.com/2/search/adaptive.json*"
                    .to_string()
                    .into(),
                resource_type: None,
                request_stage: fetch::RequestStage::Request.into(),
            },
            fetch::RequestPattern {
                url_pattern: "https://api.twitter.com/2/search/adaptive.json*"
                    .to_string()
                    .into(),
                resource_type: None,
                request_stage: fetch::RequestStage::Response.into(),
            },
        ]
        .into(),
        handle_auth_requests: None,
    })
    .await?;

    let mut request_paused = page
        .event_listener::<fetch::EventRequestPaused>()
        .await
        .unwrap();
    let intercept_page = page.clone();
    tokio::task::spawn(async move {
        while let Some(event) = request_paused.next().await {
            match (*event).clone() {
                fetch::EventRequestPaused {
                    response_status_code: Some(status_code),
                    ..
                } => {
                    let headers: Vec<fetch::HeaderEntry> = Vec::new();
                    let f = fetch::FulfillRequestParams::builder()
                        .request_id(event.request_id.clone())
                        .response_headers(headers)
                        .response_code(status_code)
                        .build()
                        .unwrap();
                    intercept_page.execute(f).await.unwrap();
                }
                _ => {
                    let mut headers = vec![];
                    for (k, v) in event.request.headers.inner().as_object().unwrap() {
                        headers.push(fetch::HeaderEntry {
                            name: k.clone(),
                            value: v.as_str().unwrap().into(),
                        })
                    }
                    let he = fetch::HeaderEntry {
                        name: "authorization".into(),
                        value: "Bearer AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA".into()
                    };
                    headers.push(he);
                    let he = fetch::HeaderEntry {
                        name: "x-guest-token".into(),
                        value: guest_token.clone(),
                    };
                    headers.push(he);

                    let c = fetch::ContinueRequestParams::builder()
                        .request_id(event.request_id.clone())
                        .headers(headers)
                        .build()
                        .unwrap();
                    intercept_page.execute(c).await.unwrap();
                }
            }
        }
    });

    Ok(page)
}

async fn query_twitter(
    page: Arc<Page>,
    query: impl AsRef<str>,
    cursor: Option<&str>,
) -> Result<(Vec<Value>, String)> {
    static URL: &str = "https://api.twitter.com/2/search/adaptive.json";

    let mut url = Url::parse(URL)?;
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

    page.goto(url.as_str()).await?;
    page.wait_for_navigation().await?;
    let content = page.content().await?;

    let re = regex::Regex::new(r"\{.*\}").unwrap();
    let json = re
        .find(&content)
        .ok_or_else(|| anyhow!("no json found"))?
        .as_str()
        .to_owned();

    let value = serde_json::from_str(&json)?;
    parse_tweets(value)
}

fn parse_tweets(json: Value) -> Result<(Vec<Value>, String)> {
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

    let root: Root = serde_json::from_value(json)?;

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
    let tweets: Vec<_> = tweets.into_iter().map(|(_, tweet)| tweet).rev().collect();

    // Parse cursor
    let timeline_str = serde_json::to_string(&root.timeline)?;
    let cursor_re = regex::Regex::new(r#""(scroll:.+?)""#)?;
    let cursor = cursor_re
        .captures(&timeline_str)
        .ok_or_else(|| anyhow!("cursor not found"))?
        .get(1)
        .unwrap()
        .as_str()
        .to_owned();

    Ok((tweets, cursor))
}
