#![deny(warnings)]
extern crate hyper;

use std::net::{TcpStream, SocketAddr};
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use hyper::net::{HttpStream};
use hyper::server::{Server, Handler, Transaction};

struct Serve {
    listening: Option<hyper::server::Listening>,
    msg_rx: mpsc::Receiver<Msg>,
    reply_tx: mpsc::Sender<Reply>,
    spawn_rx: mpsc::Receiver<()>,
}

impl Serve {
    fn addrs(&self) -> &[SocketAddr] {
        self.listening.as_ref().unwrap().addrs()
    }

    fn addr(&self) -> &SocketAddr {
        let addrs = self.addrs();
        assert!(addrs.len() == 1);
        &addrs[0]
    }

    /*
    fn head(&self) -> Request {
        unimplemented!()
    }
    */

    fn body(&self) -> Vec<u8> {
        let mut buf = vec![];
        while let Ok(Msg::Chunk(msg)) = self.msg_rx.try_recv() {
            buf.extend(&msg);
        }
        buf
    }

    fn reply(&self) -> ReplyBuilder {
        ReplyBuilder {
            tx: &self.reply_tx
        }
    }
}

struct ReplyBuilder<'a> {
    tx: &'a mpsc::Sender<Reply>,
}

impl<'a> ReplyBuilder<'a> {
    fn status(self, status: hyper::StatusCode) -> Self {
        self.tx.send(Reply::Status(status)).unwrap();
        self
    }

    fn header<H: hyper::header::Header>(self, header: H) -> Self {
        let mut headers = hyper::Headers::new();
        headers.set(header);
        self.tx.send(Reply::Headers(headers)).unwrap();
        self
    }

    fn body<T: AsRef<[u8]>>(self, body: T) {
        self.tx.send(Reply::Body(body.as_ref().into())).unwrap();
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        self.listening.take().unwrap().close();
        self.spawn_rx.recv().expect("server thread should shutdown cleanly");
    }
}

struct TestHandler {
    tx: mpsc::Sender<Msg>,
    reply: Vec<Reply>,
    peeked: Option<Vec<u8>>,
    _timeout: Option<Duration>,
    state: State,
}

enum State {
    Read,
    Reply,
    Write,
    End
}

enum Reply {
    Status(hyper::StatusCode),
    Headers(hyper::Headers),
    Body(Vec<u8>),
}

enum Msg {
    //Head(Request),
    Chunk(Vec<u8>),
}

impl Handler<HttpStream> for TestHandler {
    fn ready(&mut self, txn: &mut Transaction<HttpStream>) {
        loop {
            match self.state {
                State::Read => {
                    let mut vec = vec![0; 1024];
                    match txn.read(&mut vec) {
                        Ok(0) => {
                            self.state = State::Reply;
                        }
                        Ok(n) => {
                            vec.truncate(n);
                            self.tx.send(Msg::Chunk(vec)).unwrap();
                        }
                        Err(e) => match e.kind() {
                            io::ErrorKind::WouldBlock => return,
                            _ => panic!("test error: {}", e)
                        }
                    }
                },
                State::Reply => {
                    {
                        let mut res = txn.response();
                        for reply in self.reply.drain(..) {
                            match reply {
                                Reply::Status(s) => {
                                    res.set_status(s);
                                },
                                Reply::Headers(headers) => {
                                    use std::iter::Extend;
                                    res.headers_mut().extend(headers.iter());
                                },
                                Reply::Body(body) => {
                                    self.peeked = Some(body);
                                },
                            }
                        }
                    }

                    if self.peeked.is_some() {
                        self.state = State::Write;
                    } else {
                        txn.end();
                        self.state = State::End
                    }
                },
                State::Write => {
                    if let Some(body) = self.peeked.take() {
                        txn.write(&body).unwrap();
                    }
                    self.state = State::End;
                    txn.end();
                },
                State::End => {
                    txn.end();
                    return;
                }
            }
        }
    }
}

fn connect(addr: &SocketAddr) -> TcpStream {
    let req = TcpStream::connect(addr).unwrap();
    req.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    req.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    req
}

fn serve() -> Serve {
    serve_with_timeout(None)
}

fn serve_with_timeout(dur: Option<Duration>) -> Serve {
    use std::thread;

    let (spawn_tx, spawn_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();

    let addr = "127.0.0.1:0".parse().unwrap();
    let (listening, server) = Server::http(&addr).unwrap()
        .handle(move |incoming: Result<_, _>| {
            incoming.unwrap();
            let mut replies = Vec::new();
            while let Ok(reply) = reply_rx.try_recv() {
                replies.push(reply);
            }
            Ok(TestHandler {
                tx: msg_tx.clone(),
                _timeout: dur,
                reply: replies,
                peeked: None,
                state: State::Read,
            })
        }).unwrap();


    let thread_name = format!("test-server-{}: {:?}", listening, dur);
    thread::Builder::new().name(thread_name).spawn(move || {
        server.run();
        spawn_tx.send(()).unwrap();
    }).unwrap();

    Serve {
        listening: Some(listening),
        msg_rx: msg_rx,
        reply_tx: reply_tx,
        spawn_rx: spawn_rx,
    }
}

#[test]
fn server_get_should_ignore_body() {
    let server = serve();

    let mut req = connect(server.addr());
    // Connection: close = don't try to parse the body as a new request
    req.write_all(b"\
        GET / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Connection: close\r\n
        \r\n\
        I shouldn't be read.\r\n\
    ").unwrap();
    req.read(&mut [0; 256]).unwrap();

    assert_eq!(server.body(), b"");
}

#[test]
fn server_get_with_body() {
    let server = serve();
    let mut req = connect(server.addr());
    req.write_all(b"\
        GET / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Content-Length: 19\r\n\
        \r\n\
        I'm a good request.\r\n\
    ").unwrap();
    req.read(&mut [0; 256]).unwrap();

    // note: doesnt include trailing \r\n, cause Content-Length wasn't 21
    assert_eq!(server.body(), b"I'm a good request.");
}

#[test]
fn server_get_fixed_response() {
    let foo_bar = b"foo bar baz";
    let server = serve();
    server.reply()
        .status(hyper::Ok)
        .header(hyper::header::ContentLength(foo_bar.len() as u64))
        .body(foo_bar);
    let mut req = connect(server.addr());
    req.write_all(b"\
        GET / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Connection: close\r\n
        \r\n\
    ").unwrap();
    let mut body = String::new();
    req.read_to_string(&mut body).unwrap();
    let n = body.find("\r\n\r\n").unwrap() + 4;

    assert_eq!(&body[n..], "foo bar baz");
}

#[test]
fn server_get_chunked_response() {
    let foo_bar = b"foo bar baz";
    let server = serve();
    server.reply()
        .status(hyper::Ok)
        .header(hyper::header::TransferEncoding::chunked())
        .body(foo_bar);
    let mut req = connect(server.addr());
    req.write_all(b"\
        GET / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Connection: close\r\n
        \r\n\
    ").unwrap();
    let mut body = String::new();
    req.read_to_string(&mut body).unwrap();
    let n = body.find("\r\n\r\n").unwrap() + 4;

    assert_eq!(&body[n..], "B\r\nfoo bar baz\r\n0\r\n\r\n");
}

#[test]
fn server_post_with_chunked_body() {
    extern crate pretty_env_logger;
    pretty_env_logger::init();
    let server = serve();
    let mut req = connect(server.addr());
    req.write_all(b"\
        POST / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Transfer-Encoding: chunked\r\n\
        \r\n\
        1\r\n\
        q\r\n\
        2\r\n\
        we\r\n\
        2\r\n\
        rt\r\n\
        0\r\n\
        \r\n\
    ").unwrap();
    req.read(&mut [0; 256]).unwrap();

    assert_eq!(server.body(), b"qwert");
}

#[test]
fn server_empty_response_chunked() {
    let server = serve();
    server.reply()
        .status(hyper::Ok)
        .body("");
    let mut req = connect(server.addr());
    req.write_all(b"\
        GET / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Connection: close\r\n
        \r\n\
    ").unwrap();

    let mut response = String::new();
    req.read_to_string(&mut response).unwrap();

    assert!(response.contains("Transfer-Encoding: chunked\r\n"));

    let mut lines = response.lines();
    assert_eq!(lines.next(), Some("HTTP/1.1 200 OK"));

    let mut lines = lines.skip_while(|line| !line.is_empty());
    assert_eq!(lines.next(), Some(""));
    // 0\r\n\r\n
    assert_eq!(lines.next(), Some("0"));
    assert_eq!(lines.next(), Some(""));
    assert_eq!(lines.next(), None);
}

#[test]
fn server_empty_response_chunked_without_calling_write() {
    let server = serve();
    server.reply()
        .status(hyper::Ok)
        .header(hyper::header::TransferEncoding::chunked());
    let mut req = connect(server.addr());
    req.write_all(b"\
        GET / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Connection: close\r\n
        \r\n\
    ").unwrap();

    let mut response = String::new();
    req.read_to_string(&mut response).unwrap();

    assert!(response.contains("Transfer-Encoding: chunked\r\n"));

    let mut lines = response.lines();
    assert_eq!(lines.next(), Some("HTTP/1.1 200 OK"));

    let mut lines = lines.skip_while(|line| !line.is_empty());
    assert_eq!(lines.next(), Some(""));
    // 0\r\n\r\n
    assert_eq!(lines.next(), Some("0"));
    assert_eq!(lines.next(), Some(""));
    assert_eq!(lines.next(), None);
}

#[test]
fn server_keep_alive() {
    let foo_bar = b"foo bar baz";
    let server = serve();
    server.reply()
        .status(hyper::Ok)
        .header(hyper::header::ContentLength(foo_bar.len() as u64))
        .body(foo_bar);
    let mut req = connect(server.addr());
    req.write_all(b"\
        GET / HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Connection: keep-alive\r\n\
        \r\n\
    ").expect("writing 1");

    let mut buf = [0; 1024 * 8];
    loop {
        let n = req.read(&mut buf[..]).expect("reading 1");
        if n < buf.len() {
            if &buf[n - foo_bar.len()..n] == foo_bar {
                break;
            } else {
                println!("{:?}", ::std::str::from_utf8(&buf[..n]));
            }
        }
    }

    // try again!

    let quux = b"zar quux";
    server.reply()
        .status(hyper::Ok)
        .header(hyper::header::ContentLength(quux.len() as u64))
        .body(quux);
    req.write_all(b"\
        GET /quux HTTP/1.1\r\n\
        Host: example.domain\r\n\
        Connection: close\r\n\
        \r\n\
    ").expect("writing 2");

    let mut buf = [0; 1024 * 8];
    loop {
        let n = req.read(&mut buf[..]).expect("reading 2");
        assert!(n > 0, "n = {}", n);
        if n < buf.len() {
            if &buf[n - quux.len()..n] == quux {
                break;
            }
        }
    }
}

/*
#[test]
fn server_get_with_body_three_listeners() {
    let server = serve_n(3);
    let addrs = server.addrs();
    assert_eq!(addrs.len(), 3);

    for (i, addr) in addrs.iter().enumerate() {
        let mut req = TcpStream::connect(addr).unwrap();
        write!(req, "\
            GET / HTTP/1.1\r\n\
            Host: example.domain\r\n\
            Content-Length: 17\r\n\
            \r\n\
            I'm sending to {}.\r\n\
        ", i).unwrap();
        req.read(&mut [0; 256]).unwrap();

        // note: doesnt include trailing \r\n, cause Content-Length wasn't 19
        let comparison = format!("I'm sending to {}.", i).into_bytes();
        assert_eq!(server.body(), comparison);
    }
}
*/
