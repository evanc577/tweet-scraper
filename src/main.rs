use anyhow::Result;
use clap::Parser;
use tweet_scraper::TweetScraper;

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    query: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut scraper = TweetScraper::initialize().await?;
    for query in args.query {
        let json = scraper.tweets(query, None).await?;
        println!("{:#?}", json);
    }

    Ok(())
}
