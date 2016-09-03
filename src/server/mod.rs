//! HTTP Server
//!
//! A `Server` is created to listen on a port, parse HTTP requests, and hand
//! them off to a `Handler`.
use std::cell::RefCell;
use std::fmt;
use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{Future, Poll};
use futures::stream::{Stream, MergedItem};
use tokio::{Loop, Sender};

pub use self::request::Request;
pub use self::response::Response;
pub use self::txn::Transaction;

use http;

pub use net::{Accept, HttpListener};
use net::{HttpStream, Transport};
/*
pub use net::{Accept, HttpListener, HttpsListener};
use net::{SslServer, Transport};
*/


mod request;
mod response;
mod txn;

/// A Server that can accept incoming network requests.
#[derive(Debug)]
pub struct Server<A> {
    listeners: Vec<A>,
    keep_alive: bool,
    idle_timeout: Option<Duration>,
    max_sockets: usize,
}

impl<A: Accept> Server<A> {
    /// Creates a new Server from one or more Listeners.
    ///
    /// Panics if listeners is an empty iterator.
    pub fn new<I: IntoIterator<Item = A>>(listeners: I) -> Server<A> {
        let listeners = listeners.into_iter().collect();

        Server {
            listeners: listeners,
            keep_alive: true,
            idle_timeout: Some(Duration::from_secs(10)),
            max_sockets: 4096,
        }
    }

    /// Enables or disables HTTP keep-alive.
    ///
    /// Default is true.
    pub fn keep_alive(mut self, val: bool) -> Server<A> {
        self.keep_alive = val;
        self
    }

    /// Sets how long an idle connection will be kept before closing.
    ///
    /// Default is 10 seconds.
    pub fn idle_timeout(mut self, val: Option<Duration>) -> Server<A> {
        self.idle_timeout = val;
        self
    }

    /// Sets the maximum open sockets for this Server.
    ///
    /// Default is 4096, but most servers can handle much more than this.
    pub fn max_sockets(mut self, val: usize) -> Server<A> {
        self.max_sockets = val;
        self
    }
}

impl Server<HttpListener> { //<H: HandlerFactory<<HttpListener as Accept>::Output>> Server<HttpListener, H> {
    /// Creates a new HTTP server config listening on the provided address.
    pub fn http(addr: &SocketAddr) -> ::Result<Server<HttpListener>> {
        HttpListener::bind(addr)
            .map(Server::new)
            .map_err(From::from)
    }
}


/*
impl<S: SslServer> Server<HttpsListener<S>> {
    /// Creates a new server config that will handle `HttpStream`s over SSL.
    ///
    /// You can use any SSL implementation, as long as it implements `hyper::net::Ssl`.
    pub fn https(addr: &SocketAddr, ssl: S) -> ::Result<Server<HttpsListener<S>>> {
        HttpsListener::new(addr, ssl)
            .map(Server::new)
            .map_err(From::from)
    }
}
*/


impl/*<A: Accept>*/ Server<HttpListener> {
    /// Binds to a socket and starts handling connections.
    pub fn handle<H>(mut self, factory: H) -> ::Result<(Listening, ServerLoop<H>)>
    where H: HandlerFactory<HttpStream/*A::Output*/> + Send + 'static {
        use std::rc::Rc;
        use std::cell::RefCell;
        use tokio::{TcpListener};
        trace!("handle server = {:?}", self);

        let mut reactor = try!(Loop::new());

        let listener = self.listeners.remove(0).0;
        let addr = try!(listener.local_addr());
        let handle = reactor.handle();

        debug!("adding Listener({})", addr);

        let listener = TcpListener::from_listener(listener, &addr, handle);
        let keep_alive = self.keep_alive;
        let idle_timeout = self.idle_timeout;


        let pin = reactor.pin();
        let (shutdown_tx, shutdown_rx) = pin.handle().clone().channel();
        let work = shutdown_rx.join(listener).and_then(move |(shutdown_rx, listener)| {

            let factory = Rc::new(RefCell::new(Context {
                factory: factory,
                idle_timeout: idle_timeout,
                keep_alive: keep_alive
            }));
            listener.incoming().merge(shutdown_rx).for_each(move |merged| {
                match merged {
                    MergedItem::First((sock, addr)) => {
                        let socket = HttpStream(sock);
                        let conn = http::Conn::new(addr, socket, ConstFactory(factory.clone()));
                        pin.add_loop_data(Conn {
                            inner: conn,
                        }).forget();
                        Ok(())
                    },
                    _ => {
                        // Second or Both means shutdown was received
                        debug!("ServerLoop shutdown received");
                        Err(io::Error::new(io::ErrorKind::Other, "shutdown"))
                    }
                }
            })
        }).map_err(|_| ());

        Ok((
            Listening {
                addrs: vec![addr],
                shutdown: shutdown_tx,
            },
            ServerLoop {
                inner: Some((reactor, Box::new(work))),
                _marker: PhantomData,
            }
        ))
        //reactor.run(work);
    }
}

/// A configured `Server` ready to run.
pub struct ServerLoop<H> {
    inner: Option<(Loop, Box<Future<Item=(), Error=()>>)>,
    _marker: PhantomData<H>
}

impl<H> fmt::Debug for ServerLoop<H> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad("ServerLoop")
    }
}

// If the Handler is Send, we can assume the whole thing is Send.
//
// It's not by default, because the ServerLoop holds on to an Rc,
// which makes it not Send. However, this is safe since we never gave
// any copies away, and we move the Loop and the Future together.
unsafe impl<H: Send> Send for ServerLoop<H> {}

impl<H> ServerLoop<H> {
    /// Runs the server forever in this loop.
    ///
    /// This will block the current thread.
    pub fn run(self) {
        // drop will take care of it.
    }
}

impl<H> Drop for ServerLoop<H> {
    fn drop(&mut self) {
        self.inner.take().map(|(mut loop_, work)| {
            let _ = loop_.run(work);
        });
    }
}

struct Context<F> {
    factory: F,
    idle_timeout: Option<Duration>,
    keep_alive: bool,
}

struct ConstFactory<F>(Rc<RefCell<Context<F>>>);

impl<F: HandlerFactory<T>, T: Transport> http::ConnectionHandler<T> for ConstFactory<F> {
    type Txn = txn::Handle<ConstFactory<F>, T>;

    fn transaction(&mut self) -> Option<Self::Txn> {
        Some(txn::Handle::new(ConstFactory(self.0.clone())))
    }

    /*
    fn keep_alive_interest(&self) -> Next {
        if let Some(dur) = self.0.borrow().idle_timeout {
            Next::read().timeout(dur)
        } else {
            Next::read()
        }
    }
    */
}

impl<F: HandlerFactory<T>, T: Transport> HandlerFactory<T> for ConstFactory<F> {
    type Output = F::Output;

    fn create(&mut self, incoming: ::Result<Request>) -> ::Result<F::Output> {
        self.0.borrow_mut().factory.create(incoming)
    }
}

struct Conn<T, F> where T: Transport, F: HandlerFactory<T> {
    inner: http::Conn<SocketAddr, T, ConstFactory<F>>
}

impl<T, F> Future for Conn<T, F>
where T: Transport,
      F: HandlerFactory<T> {
    type Item = ();
    type Error = ::error::Void;
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner.poll()
    }
}

/// A handle of the running server.
pub struct Listening {
    addrs: Vec<SocketAddr>,
    shutdown: Sender<()>,
}

impl fmt::Debug for Listening {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Listening")
            .field("addrs", &self.addrs)
            .finish()
    }
}

impl fmt::Display for Listening {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (i, addr) in self.addrs().iter().enumerate() {
            if i > 1 {
                try!(f.write_str(", "));
            }
            try!(fmt::Display::fmt(addr, f));
        }
        Ok(())
    }
}

impl Listening {
    /// The addresses this server is listening on.
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// Stop the server from listening to its socket address.
    pub fn close(self) {
        debug!("closing server {}", self);
        let _ = self.shutdown.send(());
    }
}

struct Closing {
    _inner: (),
}

impl Future for Closing {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        unimplemented!("Closing::poll()")
    }
}

/// dox
pub trait Handler<T: Transport> {
    /// dox
    fn ready(&mut self, txn: &mut Transaction<T>);
}

/// Used to create a `Handler` when a new transaction is received by the server.
pub trait HandlerFactory<T: Transport> {
    /// The `Handler` to use for the incoming transaction.
    type Output: Handler<T>;
    /// Creates the associated `Handler`.
    fn create(&mut self, incoming: ::Result<Request>) -> ::Result<Self::Output>;
}

impl<F, H, T> HandlerFactory<T> for F
where F: FnMut(::Result<Request>) -> ::Result<H>, H: Handler<T>, T: Transport {
    type Output = H;
    fn create(&mut self, incoming: ::Result<Request>) -> ::Result<H> {
        self(incoming)
    }
}
