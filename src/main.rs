use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use futures_util::stream::StreamExt;
use tweet_scraper::{HeaderPersist, TweetScraper};

#[derive(Parser)]
struct Args {
    query: String,

    #[arg(short, long)]
    limit: Option<usize>,

    #[arg(short, long)]
    min_id: Option<u128>,

    #[arg(long)]
    save_headers: Option<PathBuf>,

    #[arg(long, conflicts_with = "save_headers")]
    load_headers: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    let header_persist = if let Some(save) = args.save_headers {
        HeaderPersist::Save(save)
    } else if let Some(load) = args.load_headers {
        HeaderPersist::Load(load)
    } else {
        HeaderPersist::None
    };

    let mut scraper = match TweetScraper::initialize(header_persist).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", e);
            return ExitCode::FAILURE;
        }
    };

    let tweets_stream = scraper.tweets(args.query, args.limit, args.min_id).await;
    futures_util::pin_mut!(tweets_stream);

    while let Some(tweet_result) = tweets_stream.next().await {
        let tweet = tweet_result.unwrap();
        if let Err(e) = writeln!(io::stdout(), "{}", serde_json::to_string(&tweet).unwrap()) {
            match e.kind() {
                io::ErrorKind::BrokenPipe => break,
                _ => {
                    eprintln!("e");
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    ExitCode::SUCCESS
}
