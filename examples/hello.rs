#![deny(warnings)]
extern crate hyper;
extern crate pretty_env_logger;
extern crate num_cpus;

use hyper::{HttpStream};
use hyper::header::{ContentLength, /*ContentType*/};
use hyper::server::{Server, Handler, Transaction/*, HttpListener*/};

static PHRASE: &'static [u8] = b"Hello World!";

struct Hello;

impl Handler<HttpStream> for Hello {
    fn ready(&mut self, txn: &mut Transaction<HttpStream>) {
        txn.response().headers_mut()
            .set(ContentLength(PHRASE.len() as u64));
            //.set(ContentType::plaintext());
        let n = txn.write(PHRASE).unwrap();
        debug_assert_eq!(n, PHRASE.len());
    }
}

fn main() {
    //env_logger::init().unwrap();
    pretty_env_logger::init();

    println!("Listening on http://127.0.0.1:3000");
    Server::http(&"127.0.0.1:3000".parse().unwrap()).unwrap()
        .handle(|inc: hyper::Result<_>| inc.map(|_| Hello)).unwrap();

    /*
    let listener = HttpListener::bind(&"127.0.0.1:3000".parse().unwrap()).unwrap();
    let mut handles = Vec::new();

    for _ in 0..num_cpus::get() {
        let listener = listener.try_clone().unwrap();
        handles.push(::std::thread::spawn(move || {
            Server::new(listener)
                .handle(|| Hello).unwrap();
        }));
    }
    println!("Listening on http://127.0.0.1:3000");

    for handle in handles {
        handle.join().unwrap();
    }
    */
}
