use std::{collections::HashSet, path::PathBuf, str::FromStr, sync::Arc};

use clap::Parser;
use indicatif::{MultiProgress, ProgressBar};
use indicatif_log_bridge::LogWrapper;
use log::{debug, error, info};
use reqwest::{Response, StatusCode, Url};
use tokio::io::AsyncWriteExt;

#[derive(Clone)]
struct Client {
    req_client: reqwest::Client,
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl Client {
    pub async fn get(&self, url: impl Into<Url>) -> Result<Response, reqwest::Error> {
        let permit = self.semaphore.acquire().await.unwrap();

        let resp = self
            .req_client
            .execute(self.req_client.get(url.into()).build()?)
            .await;

        drop(permit);

        resp
    }

    pub fn new(max_connections: usize) -> Self {
        Self {
            req_client: reqwest::Client::new(),
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_connections)),
        }
    }
}

#[derive(Parser)]
#[command(
    name = "rsbuster",
    author = "HyperCodec",
    about = "Fast directory finder"
)]
struct Cli {
    #[arg(short, long, help = "The target domain to bruteforce")]
    root: String,

    #[arg(short, long, help = "The path to the list of words to try")]
    wordlist: PathBuf,

    #[arg(
        short,
        long,
        help = "The path to the list of status codes to ignore/not print. If not provided, it only excludes 404"
    )]
    ignore_status: Option<PathBuf>,

    #[arg(short, long, help = "The path to write the directory info to")]
    output: PathBuf,

    #[arg(
        long,
        help = "The max number of concurrent connections. Too many will cause requests to fail.",
        default_value = "50"
    )]
    max_connections: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    let level = logger.filter();
    let multi = MultiProgress::new();

    LogWrapper::new(multi.clone(), logger).try_init()?;
    log::set_max_level(level);

    let args = Cli::parse();

    let root = Url::parse(&args.root)?;

    let wordlist = tokio::fs::read_to_string(args.wordlist).await?;
    let wordlist: Vec<_> = wordlist.split("\n").map(ToString::to_string).collect();

    let ignore_status: Arc<HashSet<StatusCode>> = match args.ignore_status {
        Some(path) => Arc::new(
            tokio::fs::read_to_string(path)
                .await?
                .split("\n")
                .map(|line| StatusCode::from_str(line).unwrap())
                .collect(),
        ),
        None => Arc::new(HashSet::from([StatusCode::NOT_FOUND])),
    };

    let mut output = tokio::fs::File::create(args.output).await?;

    let client = Client::new(args.max_connections);

    info!("Starting bruteforce on url {root}");

    let pb = multi.add(ProgressBar::new(wordlist.len() as u64));

    let mut handles = Vec::with_capacity(wordlist.len());

    for word in wordlist {
        let w2 = word.clone();
        let pb2 = pb.clone();
        let r2 = root.clone();
        let c2 = client.clone();
        let ignore2 = ignore_status.clone();
        handles.push(tokio::spawn(async move {
            let full_url = r2.join(&w2)?;

            match c2.get(full_url.clone()).await {
                Ok(response) => {
                    pb2.inc(1);
                    let status = response.status();

                    Ok(process_connection(status, ignore2, &full_url))
                }
                Err(e) => {
                    pb2.inc(1);

                    if let Some(status) = e.status() {
                        return Ok(process_connection(status, ignore2, &full_url));
                    }

                    error!("Connection failed for {full_url}: {e}");
                    Ok(None)
                }
            }
        }));
    }

    for h in handles {
        let res: Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> = h.await?;
        if let Some(line) = res.unwrap() {
            output.write(line.as_bytes()).await?;
        }
    }

    pb.finish();

    Ok(())
}

fn process_connection(
    status: StatusCode,
    ignore: Arc<HashSet<StatusCode>>,
    full_url: &Url,
) -> Option<String> {
    if ignore.contains(&status) {
        debug!("Ignored successful connection to {full_url} with status_code: {status}");
        return None;
    }

    info!("Successful connection to {full_url} with status code: {status}");

    Some(format!("{full_url} {status}\n"))
}
