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

use hyper::{Client, Uri};
use hyper::body::to_bytes;
use hyper::client::HttpConnector;
use hyper_rustls::{HttpsConnectorBuilder, HttpsConnector};
use hyper::client::connect::dns::GaiResolver;
use rustls::{ClientConfig, Certificate, RootCertStore, OwnedTrustAnchor};
use rustls::client::{ServerCertVerified, ServerCertVerifier, ServerName};

struct SkipCertificationVerification;

impl ServerCertVerifier for SkipCertificationVerification {
    fn verify_server_cert(
	&self,
        _end_entity: &Certificate,
	_intermediates: &[Certificate],
	_server_name: &ServerName,
	_scts: &mut dyn Iterator<Item = &[u8]>,
	_ocsp_response: &[u8],
	_now: SystemTime,
    ) -> Result<ServerCertVerified, rustls::TLSError> {
	Ok(ServerCertVerified::assertion())
    }
}

async fn testurl(client: Client<HttpsConnector<HttpConnector<GaiResolver>>>, pb: ProgressBar, rx: spmc::Receiver<String>) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
    //no certs
    loop {
	tracing::trace!("waiting for work");
	let url = rx.recv().unwrap();
	if url == "close" { return Ok(()); }
	tracing::trace!("sending request");
	pb.inc(1);
	let uri: Uri = match format!("{}/AirWatch", url).parse() {
	    Ok(uri) => uri,
	    Err(_) => continue
	};
	let resp = match client.get(uri).await {
	    Ok(resp) => match to_bytes(resp).await {
		Ok(bytes) => bytes,
		Err(_) => continue
	    },
	    Err(_) => continue
	};

	let body: String = match String::from_utf8(resp.to_vec()) {
	    Ok(string) => string,
	    Err(_) => continue
	};

	tracing::trace!("checking response");
	if body.contains("/AirWatch/Login") {
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

    let mut root_store = RootCertStore::empty();
    root_store.add_server_trust_anchors(
	webpki_roots::TLS_SERVER_ROOTS
	    .0
	    .iter()
	    .map(|ta| {
		OwnedTrustAnchor::from_subject_spki_name_constraints(
		    ta.subject,
		    ta.spki,
		    ta.name_constraints,
		)
	    }),
	    );
    let mut tls: ClientConfig = ClientConfig::builder()
	.with_safe_defaults()
	.with_root_certificates(root_store)
	.with_no_client_auth();
    tls.dangerous().set_certificate_verifier(Arc::new(SkipCertificationVerification {}));

    let https = HttpsConnectorBuilder::new()
	.with_tls_config(tls)
	.https_or_http()
	.enable_http1()
	.build();

    let client = Client::builder().build::<_, hyper::Body>(https);

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
