use tokio::fs::File;
use tokio::io::{BufReader, AsyncBufReadExt};
use tokio::task::{self, LocalSet};
use tokio::runtime;

use futures::future::{join_all, FutureExt};
use futures::stream::{StreamExt, futures_unordered::FuturesUnordered};

use std::env;
use std::error::Error;
use std::sync::Arc;
use std::time::SystemTime;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use std::num::NonZeroU32;
use nonzero_ext::*;
use governor::{Quota, RateLimiter};

async fn testurl(client: reqwest::Client, pb: ProgressBar, rx: spmc::Receiver<String>) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
    //no certs
    loop {
	tracing::trace!("waiting for work");
	let url = rx.recv().unwrap();
	if url == "close" { return Ok(()); }
	tracing::trace!("sending request");
	pb.inc(1);
	let resp = match client.get(format!("{}/AirWatch", url)).send().await {
	    Ok(resp) => match resp.text().await {
		Ok(str) => str,
		Err(_) => continue
	    },
	    Err(_) => continue
	};

	tracing::trace!("checking response");
	if resp.contains("/AirWatch/Login") {
            pb.inc_length(1);
	    println!("{}", url);
	}
    }
}

async fn sendurl(filename: String, mut tx: spmc::Sender<String>) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {

    //set rate limit
    let mut lim = RateLimiter::direct(Quota::per_second(nonzero!(10000u32))); // Allow 50 units per second

    //open file
    let file = File::open(filename).await.unwrap();

    let buf = BufReader::new(file);
    let mut lines = buf.lines();
    while let Some(line) = lines.next_line().await.unwrap() {
	//request from url on line
	tracing::trace!("sending url");
	tx.send(line.to_string()).unwrap();
	lim.until_ready().await;
    }
    for _n in 0..100000 {
	tx.send("close".to_string());
    }

    Ok(())
}

//#[tokio::main(flavor = "current_thread")]
fn main() -> Result<(), Box<dyn Error + Send + Sync + 'static>> {

    console_subscriber::init();

    let args: Vec<String> = env::args().collect();
    let filename: String = args[1].to_string();

    let client = reqwest::Client::builder()
	.redirect(reqwest::redirect::Policy::none())
	.danger_accept_invalid_certs(true)
	.build()
	.unwrap();

    let pb = ProgressBar::new(0);
    pb.set_draw_target(ProgressDrawTarget::stderr());
    pb.set_style(ProgressStyle::default_bar()
		 .template("{spinner:.green} {elapsed} ({len}) {pos} {per_sec}")
		 .progress_chars("#>-"));

    let (tx, rx) = spmc::channel();

    let rx = rx.clone();
    let rt = runtime::Builder::new_multi_thread()
	.enable_all()
	.build()
	.unwrap();

    // start worker tasks
    rt.spawn(async move{
	sendurl(filename, tx).await.unwrap();
    });


    let workers = FuturesUnordered::new();
    let set = LocalSet::new();

    for _n in 0..100000 {
	let rx = rx.clone();
	let pb = pb.clone();
	let client = client.clone();
	workers.push(set.spawn_local(async move { task::spawn_local( async move { testurl(client, pb, rx).await.unwrap()}).await.unwrap()}));
    }

    rt.block_on(set.run_until(workers.for_each(|_| async { () })));

    Ok(())
}
