use tweet_scraper::fetch_tweets;

#[tokio::main]
async fn main() {
    fetch_tweets("hf_dreamcatcher").await.unwrap();
}
