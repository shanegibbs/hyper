use std::borrow::Cow;
use std::fmt;
use std::hash::Hash;
use std::io;
use std::marker::PhantomData;
use std::mem;
use std::time::Duration;

use futures::Poll;

use http::{self, h1, Http1Transaction, Io, WriteBuf};
//use http::channel;
use http::buffer::Buffer;
use net::{Transport};
use version::HttpVersion;


/// This handles a connection, which will have been established over a
/// Transport (like a socket), and will likely include multiple
/// `Transaction`s over HTTP.
///
/// The connection will determine when a message begins and ends, creating
/// a new message `TransactionHandler` for each one, as well as determine if this
/// connection can be kept alive after the message, or if it is complete.
pub struct Conn<K: Key, T: Transport, H: ConnectionHandler<T>> {
    handler: H,
    io: Io<T>,
    keep_alive_enabled: bool,
    key: K,
    state: State<H::Txn, T>,
}

impl<K: Key, T: Transport, H: ConnectionHandler<T>> Conn<K, T, H> {
    fn parse(&mut self) -> ::Result<Option<http::MessageHead<<<<H as ConnectionHandler<T>>::Txn as TransactionHandler<T>>::Transaction as Http1Transaction>::Incoming>>> {
        self.io.parse::<<<H as ConnectionHandler<T>>::Txn as TransactionHandler<T>>::Transaction>()
    }

    /*
    fn read(&mut self, state: State<H::Txn, T>) -> State<H::Txn, T> {
         match state {
            State::Init { interest: Next_::Read, .. } => {
                let head = match self.parse() {
                    Ok(head) => head,
                    Err(::Error::Io(e)) => match e.kind() {
                        io::ErrorKind::WouldBlock |
                        io::ErrorKind::Interrupted => return state,
                        _ => {
                            debug!("io error trying to parse {:?}", e);
                            return State::Closed;
                        }
                    },
                    Err(e) => {
                        //TODO: send proper error codes depending on error
                        trace!("parse eror: {:?}", e);
                        return State::Closed;
                    }
                };
                let mut handler = match self.handler.transaction() {
                    Some(handler) => handler,
                    None => unreachable!()
                };
                match <<H as ConnectionHandler<T>>::Txn as TransactionHandler<T>>::Transaction::decoder(&head) {
                    Ok(decoder) => {
                        trace!("decoder = {:?}", decoder);
                        let keep_alive = self.keep_alive_enabled && head.should_keep_alive();
                        let next = handler.on_incoming(head, &self.transport);
                        trace!("handler.on_incoming() -> {:?}", next);

                        match next.interest {
                            Next_::Read => self.read(State::Http1(Http1 {
                                handler: handler,
                                reading: Reading::Body(decoder),
                                writing: Writing::Init,
                                keep_alive: keep_alive,
                                timeout: next.timeout,
                                _marker: PhantomData,
                            })),
                            Next_::Write => State::Http1(Http1 {
                                handler: handler,
                                reading: if decoder.is_eof() {
                                    if keep_alive {
                                        Reading::KeepAlive
                                    } else {
                                        Reading::Closed
                                    }
                                } else {
                                    Reading::Wait(decoder)
                                },
                                writing: Writing::Head,
                                keep_alive: keep_alive,
                                timeout: next.timeout,
                                _marker: PhantomData,
                            }),
                            Next_::ReadWrite => self.read(State::Http1(Http1 {
                                handler: handler,
                                reading: Reading::Body(decoder),
                                writing: Writing::Head,
                                keep_alive: keep_alive,
                                timeout: next.timeout,
                                _marker: PhantomData,
                            })),
                            Next_::Wait => State::Http1(Http1 {
                                handler: handler,
                                reading: Reading::Wait(decoder),
                                writing: Writing::Init,
                                keep_alive: keep_alive,
                                timeout: next.timeout,
                                _marker: PhantomData,
                            }),
                            Next_::End |
                            Next_::Remove => State::Closed,
                        }
                    },
                    Err(e) => {
                        debug!("error creating decoder: {:?}", e);
                        //TODO: update state from returned Next
                        //this would allow a Server to respond with a proper
                        //4xx code
                        let _ = handler.on_error(e);
                        State::Closed
                    }
                }
            },
            State::Init { .. } => {
                trace!("on_readable State::{:?}", state);
                state
            },
            State::Http1(mut http1) => {
                let next = match http1.reading {
                    Reading::Init => None,
                    Reading::Parse => match self.parse() {
                        Ok(head) => match <<H as ConnectionHandler<T>>::Txn as TransactionHandler<T>>::Transaction::decoder(&head) {
                            Ok(decoder) => {
                                trace!("decoder = {:?}", decoder);
                                // if client request asked for keep alive,
                                // then it depends entirely on if the server agreed
                                if http1.keep_alive {
                                    http1.keep_alive = head.should_keep_alive();
                                }
                                let next = http1.handler.on_incoming(head, &self.transport);
                                http1.reading = Reading::Wait(decoder);
                                trace!("handler.on_incoming() -> {:?}", next);
                                Some(next)
                            },
                            Err(e) => {
                                debug!("error creating decoder: {:?}", e);
                                //TODO: respond with 400
                                return State::Closed;
                            }
                        },
                        Err(::Error::Io(e)) => match e.kind() {
                            io::ErrorKind::WouldBlock |
                            io::ErrorKind::Interrupted => None,
                            _ => {
                                debug!("io error trying to parse {:?}", e);
                                return State::Closed;
                            }
                        },
                        Err(e) => {
                            trace!("parse error: {:?}", e);
                            let _ = http1.handler.on_error(e);
                            return State::Closed;
                        }
                    },
                    Reading::Body(ref mut decoder) => {
                        let wrapped = if !self.buf.is_empty() {
                            super::Trans::Buf(self.buf.wrap(&mut self.transport))
                        } else {
                            super::Trans::Port(&mut self.transport)
                        };

                        Some(http1.handler.on_decode(&mut Decoder::h1(decoder, wrapped)))
                    },
                    _ => {
                        trace!("Conn.on_readable State::Http1(reading = {:?})", http1.reading);
                        None
                    }
                };
                let mut s = State::Http1(http1);
                if let Some(next) = next {
                    s.update(next);
                }
                trace!("Conn.on_readable State::Http1 completed, new state = State::{:?}", s);

                let again = match s {
                    State::Http1(Http1 { reading: Reading::Body(ref encoder), .. }) => encoder.is_eof(),
                    _ => false
                };

                if again {
                    self.read(s)
                } else {
                    s
                }
            },
            State::Closed => {
                trace!("on_readable State::Closed");
                State::Closed
            }
        }
    }

    fn write(&mut self, mut state: State<H::Txn, T>) -> State<H::Txn, T> {
        let next = match state {
            State::Init { interest: Next_::Write, .. } => {
                // this is a Client request, which writes first, so pay
                // attention to the version written here, which will adjust
                // our internal state to Http1 or Http2
                let mut handler = match self.handler.transaction() {
                    Some(handler) => handler,
                    None => {
                        trace!("could not create handler {:?}", self.key);
                        return State::Closed;
                    }
                };
                let mut head = http::MessageHead::default();
                let mut interest = handler.on_outgoing(&mut head);
                if head.version == HttpVersion::Http11 {
                    let mut buf = Vec::new();
                    let keep_alive = self.keep_alive_enabled && head.should_keep_alive();
                    let mut encoder = <<<H as ConnectionHandler<T>>::Txn as TransactionHandler<T>>::Transaction as Http1Transaction>::encode(head, &mut buf);
                    let writing = match interest.interest {
                        // user wants to write some data right away
                        // try to write the headers and the first chunk
                        // together, so they are in the same packet
                        Next_::Write |
                        Next_::ReadWrite => {
                            encoder.prefix(WriteBuf {
                                bytes: buf,
                                pos: 0
                            });
                            interest = handler.on_encode(&mut Encoder::h1(&mut encoder, &mut self.transport));
                            Writing::Ready(encoder)
                        },
                        _ => Writing::Chunk(Chunk {
                            buf: Cow::Owned(buf),
                            pos: 0,
                            next: (encoder, interest.clone())
                        })
                    };
                    state = State::Http1(Http1 {
                        reading: Reading::Init,
                        writing: writing,
                        handler: handler,
                        keep_alive: keep_alive,
                        timeout: interest.timeout,
                        _marker: PhantomData,
                    })
                }
                Some(interest)
            }
            State::Init { .. } => {
                trace!("Conn.on_writable State::{:?}", state);
                None
            }
            State::Http1(Http1 { ref mut handler, ref mut writing, ref mut keep_alive, .. }) => {
                match *writing {
                    Writing::Init => {
                        trace!("Conn.on_writable Http1::Writing::Init");
                        None
                    }
                    Writing::Head => {
                        let mut head = http::MessageHead::default();
                        let mut interest = handler.on_outgoing(&mut head);
                        // if the request wants to close, server cannot stop it
                        if *keep_alive {
                            // if the request wants to stay alive, then it depends
                            // on the server to agree
                            *keep_alive = head.should_keep_alive();
                        }
                        let mut buf = Vec::new();
                        let mut encoder = <<<H as ConnectionHandler<T>>::Txn as TransactionHandler<T>>::Transaction as Http1Transaction>::encode(head, &mut buf);
                        *writing = match interest.interest {
                            // user wants to write some data right away
                            // try to write the headers and the first chunk
                            // together, so they are in the same packet
                            Next_::Write |
                            Next_::ReadWrite => {
                                encoder.prefix(WriteBuf {
                                    bytes: buf,
                                    pos: 0
                                });
                                interest = handler.on_encode(&mut Encoder::h1(&mut encoder, &mut self.transport));
                                Writing::Ready(encoder)
                            },
                            _ => Writing::Chunk(Chunk {
                                buf: Cow::Owned(buf),
                                pos: 0,
                                next: (encoder, interest.clone())
                            })
                        };
                        Some(interest)
                    },
                    Writing::Chunk(ref mut chunk) => {
                        trace!("Http1.Chunk on_writable");
                        match self.transport.write(&chunk.buf.as_ref()[chunk.pos..]) {
                            Ok(n) => {
                                chunk.pos += n;
                                trace!("Http1.Chunk wrote={}, done={}", n, chunk.is_written());
                                if chunk.is_written() {
                                    Some(chunk.next.1.clone())
                                } else {
                                    None
                                }
                            },
                            Err(e) => match e.kind() {
                                io::ErrorKind::WouldBlock |
                                io::ErrorKind::Interrupted => None,
                                _ => {
                                    Some(handler.on_error(e.into()))
                                }
                            }
                        }
                    },
                    Writing::Ready(ref mut encoder) => {
                        trace!("Http1.Ready on_writable");
                        Some(handler.on_encode(&mut Encoder::h1(encoder, &mut self.transport)))
                    },
                    Writing::Wait(..) => {
                        trace!("Conn.on_writable Http1::Writing::Wait");
                        None
                    }
                    Writing::KeepAlive => {
                        trace!("Conn.on_writable Http1::Writing::KeepAlive");
                        None
                    }
                    Writing::Closed => {
                        trace!("on_writable Http1::Writing::Closed");
                        None
                    }
                }
            },
            State::Closed => {
                trace!("on_writable State::Closed");
                None
            }
        };

        if let Some(next) = next {
            state.update(next);
        }
        state
    }
    */

    /*
    fn can_read_more(&self, was_init: bool) -> bool {
        match self.state {
            State::Init { .. } => !was_init && !self.buf.is_empty(),
            _ => !self.buf.is_empty()
        }
    }
    */

    fn tick(&mut self) -> Poll<(), ::error::Void> {
        loop {
            let next_state;
            match self.state {
                State::Init { .. } => {
                    trace!("State::Init tick");
                    let (version, head) = match self.parse() {
                        Ok(Some(head)) => (head.version, Ok(head)),
                        Ok(None) => return Poll::NotReady,
                        Err(e) => (HttpVersion::Http10, Err(e))
                    };

                    match version {
                        HttpVersion::Http10 | HttpVersion::Http11 => {
                            let handler = match self.handler.transaction() {
                                Some(h) => h,
                                None => {
                                    trace!("could not create txn handler, key={:?}", self.key);
                                    self.state = State::Closed;
                                    return Poll::Ok(());
                                }
                            };
                            let res = head.and_then(|head| {
                                let decoder = <<H as ConnectionHandler<T>>::Txn as TransactionHandler<T>>::Transaction::decoder(&head);
                                decoder.map(move |decoder| (head, decoder))
                            });
                            next_state = State::Http1(h1::Txn::incoming(res, handler));
                        },
                        _ => {
                            warn!("unimplemented HTTP Version = {:?}", version);
                            self.state = State::Closed;
                            return Poll::Ok(());
                        }
                    }

                },
                State::Http1(ref mut http1) => {
                    trace!("State::Http1 tick");
                    match http1.tick(&mut self.io) {
                        Poll::NotReady => return Poll::NotReady,
                        Poll::Ok(TxnResult::KeepAlive) => {
                            trace!("Http1 Txn tick complete, keep-alive");
                            //TODO: check if keep-alive is enabled
                            next_state = State::Init {};
                        },
                        Poll::Ok(TxnResult::Close) => {
                            trace!("Http1 Txn tick complete, close");
                            next_state = State::Closed;
                        },
                        Poll::Err(void) => match void {}
                    }
                },
                //State::Http2
                State::Closed => {
                    trace!("State::Closed tick");
                    return Poll::Ok(());
                }
            }

            self.state = next_state;
        }
    }


    pub fn new(key: K, transport: T, handler: H) -> Conn<K, T, H> {
        Conn {
            handler: handler,
            io: Io {
                buf: Buffer::new(),
                transport: transport,
            },
            keep_alive_enabled: true,
            key: key,
            state: State::Init {
            },
        }
    }

    // TODO: leave this in the ConnectionHandler
    pub fn keep_alive(mut self, val: bool) -> Conn<K, T, H> {
        self.keep_alive_enabled = val;
        self
    }

    pub fn poll(&mut self) -> Poll<(), ::error::Void> {
        trace!("Conn::poll >> key={:?}", self.key);
        /*
        let was_init = match self.0.state {
            State::Init { .. } => true,
            _ => false
        };
        */

        let res = self.tick();

        trace!("Conn::poll << key={:?}, result={:?}", self.key, res);

        res

        //TODO: support http1 pipeline
        /*
        match tick {
            Tick::Final => Ok(Tick::Final),
            _ => {
                if self.can_read_more(was_init) {
                    self.ready()
                } else {
                    Ok(Tick::WouldBlock)
                }
            }
        }
        */
    }


    pub fn key(&self) -> &K {
        &self.key
    }


    /*
    pub fn is_idle(&self) -> bool {
        if let State::Init { interest: Next_::Wait, .. } = self.state {
            true
        } else {
            false
        }
    }
    */
}

impl<K: Key, T: Transport, H: ConnectionHandler<T>> fmt::Debug for Conn<K, T, H> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Conn")
            .field("keep_alive_enabled", &self.keep_alive_enabled)
            .field("state", &self.state)
            .field("io", &self.io)
            .finish()
    }
}

enum State<H: TransactionHandler<T>, T: Transport> {
    Init {
    },
    /// Http1 will only ever use a connection to send and receive a single
    /// message at a time. Once a H1 status has been determined, we will either
    /// be reading or writing an H1 message, and optionally multiple if
    /// keep-alive is true.
    Http1(h1::Txn<H, T>),
    /// Http2 allows multiplexing streams over a single connection. So even
    /// when we've identified a certain message, we must always parse frame
    /// head to determine if the incoming frame is part of a current message,
    /// or a new one. This also means we could have multiple messages at once.
    //Http2 {},
    Closed,
}


/*
impl<H: TransactionHandler<T>, T: Transport> State<H, T> {
    fn timeout(&self) -> Option<Duration> {
        match *self {
            State::Init { timeout, .. } => timeout,
            State::Http1(ref http1) => http1.timeout,
            State::Closed => None,
        }
    }
}
*/

impl<H: TransactionHandler<T>, T: Transport> fmt::Debug for State<H, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            State::Init { .. } => f.debug_struct("Init")
                .finish(),
            State::Http1(ref h1) => f.debug_tuple("Http1")
                .field(h1)
                .finish(),
            State::Closed => f.write_str("Closed")
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TxnResult {
    KeepAlive,
    Close
}

/*
impl<H: TransactionHandler<T>, T: Transport> State<H, T> {
    fn update(&mut self, next: Next) {
            let timeout = next.timeout;
            let state = mem::replace(self, State::Closed);
            match (state, next.interest) {
                (_, Next_::Remove) |
                (State::Closed, _) => return, // Keep State::Closed.
                (State::Init { .. }, e) => {
                    mem::replace(self,
                                 State::Init {
                                     interest: e,
                                     timeout: timeout,
                                 });
                }
                (State::Http1(mut http1), next_) => {
                    match next_ {
                        Next_::Remove => unreachable!(), // Covered in (_, Next_::Remove) case above.
                        Next_::End => {
                            let reading = match http1.reading {
                                Reading::Body(ref decoder) |
                                Reading::Wait(ref decoder) if decoder.is_eof() => {
                                    if http1.keep_alive {
                                        Reading::KeepAlive
                                    } else {
                                        Reading::Closed
                                    }
                                }
                                Reading::KeepAlive => http1.reading,
                                _ => Reading::Closed,
                            };
                            let mut writing = Writing::Closed;
                            let encoder = match http1.writing {
                                Writing::Wait(enc) |
                                Writing::Ready(enc) => Some(enc),
                                Writing::Chunk(mut chunk) => {
                                    if chunk.is_written() {
                                        Some(chunk.next.0)
                                    } else {
                                        chunk.next.1 = next;
                                        writing = Writing::Chunk(chunk);
                                        None
                                    }
                                }
                                _ => return, // Keep State::Closed.
                            };
                            if let Some(encoder) = encoder {
                                if encoder.is_eof() {
                                    if http1.keep_alive {
                                        writing = Writing::KeepAlive
                                    }
                                } else if let Some(buf) = encoder.finish() {
                                    writing = Writing::Chunk(Chunk {
                                        buf: buf.bytes,
                                        pos: buf.pos,
                                        next: (h1::Encoder::length(0), Next::end()),
                                    })
                                }
                            };

                            match (reading, writing) {
                                (Reading::KeepAlive, Writing::KeepAlive) => {
                                    let next = Next::read(); /*TODO: factory.keep_alive_interest();*/
                                    mem::replace(self,
                                                 State::Init {
                                                     interest: next.interest,
                                                     timeout: next.timeout,
                                                 });
                                    return;
                                }
                                (reading, Writing::Chunk(chunk)) => {
                                    http1.reading = reading;
                                    http1.writing = Writing::Chunk(chunk);
                                }
                                _ => return, // Keep State::Closed.
                            }
                        }
                        Next_::Read => {
                            http1.reading = match http1.reading {
                                Reading::Init => Reading::Parse,
                                Reading::Wait(decoder) => Reading::Body(decoder),
                                same => same,
                            };

                            http1.writing = match http1.writing {
                                Writing::Ready(encoder) => {
                                    if encoder.is_eof() {
                                        if http1.keep_alive {
                                            Writing::KeepAlive
                                        } else {
                                            Writing::Closed
                                        }
                                    } else if encoder.is_closed() {
                                        if let Some(buf) = encoder.finish() {
                                            Writing::Chunk(Chunk {
                                                buf: buf.bytes,
                                                pos: buf.pos,
                                                next: (h1::Encoder::length(0), Next::wait()),
                                            })
                                        } else {
                                            Writing::Closed
                                        }
                                    } else {
                                        Writing::Wait(encoder)
                                    }
                                }
                                Writing::Chunk(chunk) => {
                                    if chunk.is_written() {
                                        Writing::Wait(chunk.next.0)
                                    } else {
                                        Writing::Chunk(chunk)
                                    }
                                }
                                same => same,
                            };
                        }
                        Next_::Write => {
                            http1.writing = match http1.writing {
                                Writing::Wait(encoder) => Writing::Ready(encoder),
                                Writing::Init => Writing::Head,
                                Writing::Chunk(chunk) => {
                                    if chunk.is_written() {
                                        Writing::Ready(chunk.next.0)
                                    } else {
                                        Writing::Chunk(chunk)
                                    }
                                }
                                same => same,
                            };

                            http1.reading = match http1.reading {
                                Reading::Body(decoder) => {
                                    if decoder.is_eof() {
                                        if http1.keep_alive {
                                            Reading::KeepAlive
                                        } else {
                                            Reading::Closed
                                        }
                                    } else {
                                        Reading::Wait(decoder)
                                    }
                                }
                                same => same,
                            };
                        }
                        Next_::ReadWrite => {
                            http1.reading = match http1.reading {
                                Reading::Init => Reading::Parse,
                                Reading::Wait(decoder) => Reading::Body(decoder),
                                same => same,
                            };
                            http1.writing = match http1.writing {
                                Writing::Wait(encoder) => Writing::Ready(encoder),
                                Writing::Init => Writing::Head,
                                Writing::Chunk(chunk) => {
                                    if chunk.is_written() {
                                        Writing::Ready(chunk.next.0)
                                    } else {
                                        Writing::Chunk(chunk)
                                    }
                                }
                                same => same,
                            };
                        }
                        Next_::Wait => {
                            http1.reading = match http1.reading {
                                Reading::Body(decoder) => Reading::Wait(decoder),
                                same => same,
                            };

                            http1.writing = match http1.writing {
                                Writing::Ready(encoder) => Writing::Wait(encoder),
                                Writing::Chunk(chunk) => {
                                    if chunk.is_written() {
                                        Writing::Wait(chunk.next.0)
                                    } else {
                                        Writing::Chunk(chunk)
                                    }
                                }
                                same => same,
                            };
                        }
                    }
                    http1.timeout = timeout;
                    mem::replace(self, State::Http1(http1));
                }
            };
        }
}
*/

pub trait TransactionHandler<T: Transport> {
    type Transaction: Http1Transaction;

    fn ready(&mut self, txn: &mut http::Transaction<T, Self::Transaction>);
}

pub trait ConnectionHandler<T: Transport> {
    type Txn: TransactionHandler<T>;
    fn transaction(&mut self) -> Option<Self::Txn>;
    //fn keep_alive_interest(&self) -> Next;
}

pub trait ConnectionHandlerFactory<K: Key, T: Transport> {
    type Output: ConnectionHandler<T>;
    fn create(&mut self, seed: Seed<K>) -> Option<Self::Output>;
}

pub trait Key: Eq + Hash + Clone + fmt::Debug {}
impl<T: Eq + Hash + Clone + fmt::Debug> Key for T {}

pub struct Seed<'a, K: Key + 'a>(&'a K);

impl<'a, K: Key + 'a> Seed<'a, K> {
    pub fn key(&self) -> &K {
        self.0
    }
}


#[cfg(test)]
mod tests {
}
