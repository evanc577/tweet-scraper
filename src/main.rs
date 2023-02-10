use std::io::{self, Write};

use clap::Parser;
use futures_util::stream::StreamExt;
use tweet_scraper::TweetScraper;

#[derive(Parser)]
struct Args {
    query: String,

    #[arg(short, long)]
    limit: Option<usize>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let mut scraper = TweetScraper::initialize().await.unwrap();
    let tweets_stream = scraper.tweets(args.query, args.limit).await;
    futures_util::pin_mut!(tweets_stream);
    while let Some(tweet_result) = tweets_stream.next().await {
        let tweet = tweet_result.unwrap();
        if let Err(e) = writeln!(io::stdout(), "{}", serde_json::to_string(&tweet).unwrap()) {
            match e.kind() {
                io::ErrorKind::BrokenPipe => break,
                _ => Err(e).unwrap(),
            }
        }
    }
}
