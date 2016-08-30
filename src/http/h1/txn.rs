use std::borrow::Cow;
use std::fmt;
use std::io;
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
    pub fn incoming(&mut self) -> ::Result<http::MessageHead<X::Incoming>> {
        unimplemented!()
    }

    pub fn outgoing(&mut self) -> &mut http::MessageHead<X::Outgoing> {
        &mut self.heads.outgoing
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut eof = false;
        let res = match self.state.writing {
            Writing::Init | Writing::Head => {
                trace!("Txn::write Head");
                let mut buf = Vec::new();
                // if the client wants to close, server cannot stop it
                if self.state.keep_alive {
                    // if the client wants to stay alive, then it depends
                    // on the server to agree
                    self.state.keep_alive = self.heads.outgoing.should_keep_alive();
                }
                let mut encoder = X::encode(&mut self.heads.outgoing, &mut buf);
                encoder.prefix(WriteBuf::new(buf));
                self.state.writing = Writing::Body(encoder);
                return self.write(data);
            },
            Writing::Chunk(..) => unimplemented!("Writing::Chunk"),
            Writing::Body(ref mut encoder) => {
                trace!("Txn::write body");
                let res = encoder.encode(&mut self.io.transport, data);
                eof = encoder.is_eof();
                res
            },
            Writing::KeepAlive | Writing::Closed => Ok(0),
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
        match self.state.reading {
            Reading::Init | Reading::Parse => {
                unimplemented!()
            },
            Reading::Body(..) => unimplemented!(),
            Reading::KeepAlive | Reading::Closed => Ok(0),
        }
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
    Chunk(Chunk) ,
    Body(h1::Encoder),
    KeepAlive,
    Closed
}

#[derive(Debug)]
struct Chunk {
    buf: Cow<'static, [u8]>,
    pos: usize,
    next: h1::Encoder,
}

impl Chunk {
    fn is_written(&self) -> bool {
        self.pos >= self.buf.len()
    }
}
