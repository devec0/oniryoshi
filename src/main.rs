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
use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

use std::num::NonZeroU32;
use nonzero_ext::*;
use governor::{Quota, RateLimiter};

use clap::{Arg, App};

struct Job {
    host: String,
    probe: String,
    matchstr: String,
}

async fn testurl(pb: ProgressBar, rx: spmc::Receiver<Job>) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::USER_AGENT, reqwest::header::HeaderValue::from_static("Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:95.0) Gecko/20100101 Firefox/95.0"));

    //no certs
    let client = reqwest::Client::builder()
	.default_headers(headers)
	.timeout(Duration::from_secs(5))
	.redirect(reqwest::redirect::Policy::none())
	.danger_accept_invalid_hostnames(true)
	.danger_accept_invalid_certs(true)
	.build()
	.unwrap();

    loop {
	tracing::trace!("waiting for work");
	let job = rx.recv().unwrap();

	tracing::trace!("sending request");
	pb.inc(1);

	let get = client.get(format!("{}{}", job.host, job.probe));
	let req = match get.build() {
	    Ok(req) => req,
	    Err(e) => {
		//eprintln!("error building get request for {}: {:?}", job.host, e);
		continue
	    }
	};
	let resp = match client.execute(req).await {
	    Ok(resp) => match resp.text().await {
		Ok(str) => str,
		Err(e) => {
		    //eprintln!("error matching on response: {:?}", e);
		    continue
		}
	    },
	    Err(e) => {
		//eprintln!("error executing request for {}: {:?}", job.host, e);
		continue
	    }
	};

	tracing::trace!("checking response");
	if resp.contains(&job.matchstr) {
            pb.inc_length(1);
	    println!("{}", job.host);
	}
    }
}

async fn sendurl(filename: String, probe: String, matchstr: String, rate: u32,  mut tx: spmc::Sender<Job>) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {

    //set rate limit
    let lim = RateLimiter::direct(Quota::per_second(std::num::NonZeroU32::new(rate).unwrap()));

    //open file
    let file = File::open(filename).await.unwrap();

    let buf = BufReader::new(file);
    let mut lines = buf.lines();
    while let Some(line) = lines.next_line().await.unwrap() {
	//request from url on line
	tracing::trace!("sending url");
	let msg = Job{
	    host: line,
	    probe: probe.clone(),
	    matchstr: matchstr.clone(),
	};
	tx.send(msg).unwrap();
	lim.until_ready().await;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync + 'static>> {

    console_subscriber::init();

    let matches = App::new("oniryoshi")
	.version("0.1.0")
	.author("James Hebden <ec0@spooky.computer>")
	.about("Scans endpoints for signatures")
	.arg(Arg::with_name("in")
	     .short("i")
	     .long("in")
	     .takes_value(true)
	     .help("A file containing targets to scan in <scheme>://host:port format"))
	.arg(Arg::with_name("rate")
	     .short("r")
	     .long("rate")
	     .takes_value(true)
	     .default_value("100")
	     .help("Maximum in-flight requests per second"))
	.arg(Arg::with_name("path")
	     .short("p")
	     .long("path")
	     .takes_value(true)
	     .help("HTTP path to request"))
	.arg(Arg::with_name("match")
	     .short("m")
	     .long("match")
	     .takes_value(true)
	     .help("String to match in the response"))
	.get_matches();

    let pb = ProgressBar::new(0);

    let infile: String = matches.value_of("in").unwrap().to_string();
    let probe: String = matches.value_of("path").unwrap().to_string();
    let matchstr: String = matches.value_of("match").unwrap().to_string();
    let rate = match matches.value_of("rate").unwrap().parse::<i32>() {
	Ok(n) => n,
	Err(_) => {
	    pb.println("could not parse rate, using default of 100");
	    100
	}
    };

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
	sendurl(infile, probe, matchstr, rate.try_into().unwrap(), tx).await.unwrap();
    });


    let workers = FuturesUnordered::new();
    for _n in 0..1000 {
	let rx = rx.clone();
	let pb = pb.clone();
	workers.push(task::spawn( async move { testurl(pb, rx).await.unwrap()}));
    }

    let _results: Vec<_> = workers.collect().await;

    Ok(())
}
