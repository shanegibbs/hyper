#![deny(warnings)]
extern crate hyper;
extern crate pretty_env_logger;
#[macro_use]
extern crate log;

use hyper::{Get, Post, StatusCode, HttpStream};
use hyper::header::ContentLength;
use hyper::server::{Server, Handler, Transaction, Request};

struct Echo {
    buf: Vec<u8>,
    read_pos: usize,
    write_pos: usize,
    in_body: bool,
    eof: bool,
    route: Route,
}

enum Route {
    NotFound,
    Index,
    Echo(Body),
}

#[derive(Clone, Copy)]
enum Body {
    Len(u64),
    Chunked
}

static INDEX: &'static [u8] = b"Try POST /echo";

impl Echo {
    fn new(req: Request) -> Echo {
        let route = match (req.method(), req.path()) {
            (&Get, Some("/")) | (&Get, Some("/echo")) => {
                    info!("GET Index");
                    Route::Index
            },
            (&Post, Some("/echo")) => {
                info!("POST Echo");
                if let Some(len) = req.headers().get::<ContentLength>() {
                    Route::Echo(Body::Len(**len))
                } else {
                    Route::Echo(Body::Chunked)
                }
            },
            _ => Route::NotFound
        };
        Echo {
            buf: vec![0; 4096],
            read_pos: 0,
            write_pos: 0,
            eof: false,
            in_body: false,
            route: route,
        }
    }
}

impl Handler<HttpStream> for Echo {
    fn ready(&mut self, txn: &mut Transaction<HttpStream>) {
        match self.route {
            Route::Index => {
                txn.response().headers_mut().set(ContentLength(INDEX.len() as u64));
                txn.write(INDEX).unwrap();
                txn.end();
            },
            Route::NotFound => {
                txn.response().set_status(StatusCode::NotFound);
                txn.end();
            },
            Route::Echo(ref body) => {
                if self.read_pos < self.buf.len() {
                    match txn.try_read(&mut self.buf[self.read_pos..]) {
                        Ok(Some(0)) => {
                            debug!("Read 0, eof");
                            self.eof = true;
                        },
                        Ok(Some(n)) => {
                            self.read_pos += n;
                            match *body {
                                Body::Len(max) if max <= self.read_pos as u64 => {
                                    self.eof = true;
                                },
                                _ => ()
                            }
                        }
                        Ok(None) => (),
                        Err(e) => {
                            println!("read error {:?}", e);
                            txn.end();
                        }
                    }
                }

                if !self.in_body {
                    match *body {
                        Body::Len(len) => txn.response().headers_mut().set(ContentLength(len)),
                        _ => ()
                    }
                    self.in_body = true;
                }

                if self.write_pos < self.read_pos {
                    match txn.try_write(&self.buf[self.write_pos..self.read_pos]) {
                        Ok(Some(0)) => panic!("write ZERO"),
                        Ok(Some(n)) => {
                            self.write_pos += n;
                        }
                        Ok(None) => (),
                        Err(e) => {
                            println!("write error {:?}", e);
                            txn.end();
                        }
                    }
                } else if self.eof {
                    txn.end();
                }
            }
        }
    }
}


fn main() {
    pretty_env_logger::init();
    let server = Server::http(&"127.0.0.1:1337".parse().unwrap()).unwrap();
    println!("Listening on http://127.0.0.1:1337");
    let _ /*(listening, server)*/ = server.handle(|inc: hyper::Result<_>| inc.map(Echo::new)).unwrap();
    //println!("Listening on http://{}", listening);
    //server.run();
}
