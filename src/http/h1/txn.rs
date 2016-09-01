use std::borrow::Cow;
use std::fmt;
use std::io;
use std::mem;
use std::marker::PhantomData;

use futures::Poll;

use http::{self, MessageHead, h1, Http1Transaction, TransactionHandler, WriteBuf};
use http::conn::TxnResult;
use http::h1::Decoder;
use net::Transport;

pub struct Txn<H, T> where H: TransactionHandler<T>, T: Transport {
    handler: H,
    state: State,
    heads: Heads<<<H as TransactionHandler<T>>::Transaction as Http1Transaction>::Incoming, <<H as TransactionHandler<T>>::Transaction as Http1Transaction>::Outgoing>,
    _marker: PhantomData<T>
}

impl<H: TransactionHandler<T>, T: Transport> Txn<H, T> {
    pub fn incoming(incoming: ::Result<(MessageHead<<<H as TransactionHandler<T>>::Transaction as Http1Transaction>::Incoming>, Decoder)>, handler: H) -> Txn<H, T> {
        let (head, body, keep_alive) = match incoming {
            Ok((head, body)) => {
                let keep_alive = head.should_keep_alive();
                (Ok(head), if body.is_eof() {
                    if keep_alive { Reading::KeepAlive } else { Reading::Closed }
                } else {
                    Reading::Body(body)
                }, keep_alive)
            },
            Err(e) => (Err(e), Reading::Closed, false)
        };
        Txn {
            handler: handler,
            state: State {
                keep_alive: keep_alive,
                reading: body,
                writing: Writing::Init,
            },
            heads: Heads {
                incoming: Some(head),
                outgoing: Default::default(),
            },
            _marker: PhantomData,
        }
    }
}

impl<H: TransactionHandler<T>, T: Transport> Txn<H, T> {
    pub fn tick(&mut self, io: &mut http::Io<T>) -> Poll<TxnResult, ::error::Void> {
        trace!("Txn::tick self={:?}", self);
        {
            let mut txn = http::Transaction::h1(TxnIo {
                io: io,
                state: &mut self.state,
                heads: &mut self.heads,
                _marker: PhantomData,
            });
            self.handler.ready(&mut txn);
        }

        match (&self.state.reading, &self.state.writing) {
            (&Reading::KeepAlive, &Writing::KeepAlive) => Poll::Ok(TxnResult::KeepAlive),
            (&Reading::KeepAlive, &Writing::Closed) |
            (&Reading::Closed, &Writing::KeepAlive) |
            (&Reading::Closed, &Writing::Closed) => Poll::Ok(TxnResult::Close),
            _ => Poll::NotReady
        }
    }
}

impl<H: TransactionHandler<T>, T: Transport> fmt::Debug for Txn<H, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Txn")
            //.field("heads", &self.heads)
            .field("state", &self.state)
            .finish()
    }
}

pub struct TxnIo<'a, T: 'a, X: Http1Transaction + 'a> {
    io: &'a mut http::Io<T>,
    state: &'a mut State,
    heads: &'a mut Heads<X::Incoming, X::Outgoing>,
    _marker: PhantomData<X>,
}

impl<'a, T: Transport + 'a, X: Http1Transaction + 'a> TxnIo<'a, T, X> {
    #[inline]
    pub fn incoming(&mut self) -> ::Result<http::MessageHead<X::Incoming>> {
        self.heads.incoming.take().unwrap()
    }

    #[inline]
    pub fn outgoing(&mut self) -> &mut http::MessageHead<X::Outgoing> {
        &mut self.heads.outgoing
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let eof;
        let res = match self.state.writing {
            Writing::Init | Writing::Head => {
                trace!("Txn::write Head");
                write_head::<X>(&mut self.state, &mut self.heads.outgoing);
                return self.write(data);
            },
            Writing::Body(ref mut encoder) => {
                trace!("Txn::write body");
                let res = encoder.encode(&mut self.io.transport, data);
                eof = encoder.is_eof();
                res
            },
            Writing::Ending(..) => unimplemented!("Writing::Ending"),
            Writing::KeepAlive | Writing::Closed => return Ok(0),
        };

        if eof {
            trace!("Txn::write is eof");
            self.state.writing = if self.state.keep_alive {
                Writing::KeepAlive
            } else {
                Writing::Closed
            }
        }
        res
    }

    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let eof;
        let res = match self.state.reading {
            Reading::Init | Reading::Parse => {
                unimplemented!("Reading::Parse")
            },
            Reading::Body(ref mut decoder) => {
                trace!("Txn::read body buf = [{}]", buf.len());
                let res = decoder.decode(&mut self.io, buf);
                eof = decoder.is_eof();
                res
            }
            Reading::KeepAlive | Reading::Closed => return Ok(0),
        };

        if eof {
            trace!("Txn::read is eof");
            self.state.reading = if self.state.keep_alive {
                Reading::KeepAlive
            } else {
                Reading::Closed
            }
        }
        res
    }

    #[inline]
    pub fn end(&mut self) {
        trace!("h1::Txn::end");
        self.end_reading();
        self.end_writing();
    }

    fn end_reading(&mut self) {
        self.state.reading = match self.state.reading {
            Reading::Body(ref decoder) if decoder.is_eof() => {
                if self.state.keep_alive {
                    Reading::KeepAlive
                } else {
                    Reading::Closed
                }
            },
            Reading::KeepAlive => Reading::KeepAlive,
            _ => Reading::Closed
        };
    }

    fn end_writing(&mut self) {
        self.state.writing = match mem::replace(&mut self.state.writing, Writing::Closed) {
            Writing::Init | Writing::Head => {
                self.state.writing = Writing::Head;
                write_head::<X>(&mut self.state, &mut self.heads.outgoing);
                return self.end_writing();
            },
            Writing::Body(encoder) => {
                if encoder.is_eof() {
                    if self.state.keep_alive {
                        Writing::KeepAlive
                    } else {
                        Writing::Closed
                    }
                } else if let Some(mut buf) = encoder.finish() {
                    match write_ending(&mut buf, &mut self.io.transport) {
                        Poll::Ok(()) => {
                            if self.state.keep_alive {
                                Writing::KeepAlive
                            } else {
                                Writing::Closed
                            }
                        },
                        Poll::NotReady => Writing::Ending(buf),
                        Poll::Err(e) => Writing::Closed,
                    }
                } else {
                    Writing::Closed
                }
            },
            Writing::Ending(e) => Writing::Ending(e),
            Writing::KeepAlive => Writing::KeepAlive,
            _ => return
        };
    }

    #[inline]
    pub fn abort(&mut self) {
        self.state.reading = Reading::Closed;
        self.state.writing = Writing::Closed;
    }
}

#[derive(Debug)]
struct Heads<I, O> {
    incoming: Option<::Result<http::MessageHead<I>>>,
    outgoing: http::MessageHead<O>,
}

#[derive(Debug)]
struct State {
    keep_alive: bool,
    reading: Reading,
    writing: Writing,
}

fn write_head<X: Http1Transaction>(state: &mut State, head: &mut MessageHead<X::Outgoing>) {
    debug_assert!(match state.writing {
        Writing::Init | Writing::Head => true,
        _ => false
    }, "write_head must be called only when Writing is Init | Head, was {:?}", state.writing);

    let mut buf = Vec::new();
    // if the client wants to close, server cannot stop it
    if state.keep_alive {
        // if the client wants to stay alive, then it depends
        // on the server to agree
        state.keep_alive = head.should_keep_alive();
    }
    let mut encoder = X::encode(head, &mut buf);
    encoder.prefix(WriteBuf::new(buf));
    state.writing = Writing::Body(encoder);
}

fn write_ending<T: io::Write>(ending: &mut Ending, dst: &mut T) -> Poll<(), io::Error> {
    loop {
        match ending.write_to(dst) {
            Ok(_) => {
                if ending.is_written() {
                    return Poll::Ok(());
                }
            },
            Err(e) => match e.kind() {
                io::ErrorKind::WouldBlock => return Poll::NotReady,
                _ => {
                    error!("io error ending transaction: {}", e);
                    return Poll::Err(e);
                }
            }
        }
    }
}

#[derive(Debug)]
enum Reading {
    Init,
    Parse,
    Body(h1::Decoder),
    KeepAlive,
    Closed
}

#[derive(Debug)]
enum Writing {
    Init,
    Head,
    Body(h1::Encoder),
    Ending(Ending),
    KeepAlive,
    Closed
}

type Ending = WriteBuf<Cow<'static, [u8]>>;
