use anyhow::Result;
use clap::Parser;
use futures_util::stream::StreamExt;
use tweet_scraper::TweetScraper;

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    query: String,

    #[arg(short, long)]
    limit: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut scraper = TweetScraper::initialize().await?;
    let tweets_stream = scraper.tweets(args.query, args.limit).await;
    futures_util::pin_mut!(tweets_stream);
    tweets_stream
        .for_each(|tweet_result| {
            match tweet_result {
                Ok(tweet) => println!("{}", serde_json::to_string(&tweet).unwrap()),
                Err(e) => eprintln!("{:?}", e),
            }
            futures_util::future::ready(())
        })
        .await;

    Ok(())
}
