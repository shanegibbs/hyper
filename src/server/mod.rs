//! HTTP Server
//!
//! A `Server` is created to listen on a port, parse HTTP requests, and hand
//! them off to a `Handler`.
use std::cell::RefCell;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::task::{Task, Tick};

pub use self::request::Request;
pub use self::response::Response;

use http::{self, Next};

pub use net::{Accept, HttpListener};
use net::{HttpStream, Transport};
/*
pub use net::{Accept, HttpListener, HttpsListener};
use net::{SslServer, Transport};
*/


mod request;
mod response;
mod transaction;

/// A configured `Server` ready to run.
pub struct ServerLoop<A, H> where A: Accept, H: HandlerFactory<A::Output> {
    inner: ::std::marker::PhantomData<(A, H)>
    //inner: Option<(rotor::Loop<ServerFsm<A, H>>, Context<H>)>,
}

impl<A: Accept, H: HandlerFactory<A::Output>> fmt::Debug for ServerLoop<A, H> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad("ServerLoop")
    }
}

/// A Server that can accept incoming network requests.
#[derive(Debug)]
pub struct Server<A> {
    lead_listener: A,
    other_listeners: Vec<A>,
    keep_alive: bool,
    idle_timeout: Option<Duration>,
    max_sockets: usize,
}

impl<A: Accept> Server<A> {
    /// Creates a new Server from one or more Listeners.
    ///
    /// Panics if listeners is an empty iterator.
    pub fn new(listener: A) -> Server<A> {
    /*
    pub fn new<I: IntoIterator<Item = A>>(listeners: I) -> Server<A> {
        let mut listeners = listeners.into_iter();
        let lead_listener = listeners.next().expect("Server::new requires at least 1 listener");
        let other_listeners = listeners.collect::<Vec<_>>();
    */
        let lead_listener = listener;
        let other_listeners = Vec::new();

        Server {
            lead_listener: lead_listener,
            other_listeners: other_listeners,
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
        use ::mio;
        mio::tcp::TcpListener::bind(addr)
            .map(HttpListener)
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
    pub fn handle<H>(self, factory: H) -> ::Result<(Listening, ServerLoop<HttpListener, H>)>
    where H: HandlerFactory<HttpStream/*A::Output*/> + Send + 'static {
        use std::rc::Rc;
        use std::cell::RefCell;
        use tokio::reactor::{self, Reactor};
        use tokio::task::Tick;
        let reactor = try!(Reactor::default());

        let handle = reactor.handle();
        handle.oneshot(move || {
            let listener = ::tokio::tcp::TcpListener::watch(self.lead_listener.0).unwrap();
            let keep_alive = self.keep_alive;
            let idle_timeout = self.idle_timeout;

            let factory = Rc::new(RefCell::new(Context {
                factory: factory,
                idle_timeout: idle_timeout,
                keep_alive: keep_alive
            }));

            reactor::schedule(move || {
                while let Some(socket) = try!(listener.accept()) {
                    let socket = try!(::tokio::tcp::TcpStream::watch(socket));
                    let conn = http::Conn::new((), socket, ConstFactory(factory.clone()), Next::read());
                    let conn = Conn {
                        inner: conn,
                    };
                    reactor::schedule(conn);
                }

                Ok(Tick::WouldBlock)
            });
        });

        reactor.run();
        unimplemented!()
    }
}


impl<A: Accept, H: HandlerFactory<A::Output>> ServerLoop<A, H> {
    /// Runs the server forever in this loop.
    ///
    /// This will block the current thread.
    pub fn run(self) {
        // drop will take care of it.
    }
}

impl<A: Accept, H: HandlerFactory<A::Output>> Drop for ServerLoop<A, H> {
    fn drop(&mut self) {
        /*
        self.inner.take().map(|(loop_, ctx)| {
            let _ = loop_.run(ctx);
        });
        */
    }
}

struct Context<F> {
    factory: F,
    idle_timeout: Option<Duration>,
    keep_alive: bool,
}

struct ConstFactory<F>(Rc<RefCell<Context<F>>>);

impl<F: HandlerFactory<T>, T: Transport> http::ConnectionHandler<T> for ConstFactory<F> {
    type Txn = transaction::Handle<F::Output, T>;

    fn transaction(&mut self) -> Option<Self::Txn> {
        Some(transaction::Handle::new(self.0.borrow_mut().factory.create()))
    }

    fn keep_alive_interest(&self) -> Next {
        if let Some(dur) = self.0.borrow().idle_timeout {
            Next::read().timeout(dur)
        } else {
            Next::read()
        }
    }
}


struct Conn<T, F> where T: Transport, F: HandlerFactory<T> {
    inner: http::Conn<(), T, ConstFactory<F>>
}

impl<T, F> Task for Conn<T, F>
where T: Transport,
      F: HandlerFactory<T> {
    fn tick(&mut self) -> io::Result<Tick> {
        self.inner.ready()
    }
}

/// A handle of the running server.
pub struct Listening {
    addrs: Vec<SocketAddr>,
    shutdown: (Arc<AtomicBool>, /*rotor::Notifier*/),
}

impl fmt::Debug for Listening {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Listening")
            .field("addrs", &self.addrs)
            .field("closed", &self.shutdown.0.load(Ordering::Relaxed))
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
        self.shutdown.0.store(true, Ordering::Release);
        //self.shutdown.1.wakeup().unwrap();
    }
}

/// dox
pub trait Handler<T: Transport> {
    /// dox
    fn ready(&mut self, txn: &mut Transaction<T>);
}

/*
/// A trait to react to server events that happen for each transaction.
///
/// Each event handler returns its desired `Next` action.
pub trait Handler<T: Transport> {
    /// This event occurs first, triggering when a `Request` has been parsed.
    fn on_request(&mut self, request: Request<T>) -> Next;
    /// This event occurs each time the `Request` is ready to be read from.
    fn on_request_readable(&mut self, request: &mut http::Decoder<T>) -> Next;
    /// This event occurs after the first time this handled signals `Next::write()`.
    fn on_response(&mut self, response: &mut Response) -> Next;
    /// This event occurs each time the `Response` is ready to be written to.
    fn on_response_writable(&mut self, response: &mut http::Encoder<T>) -> Next;

    /// This event occurs whenever an `Error` occurs outside of the other events.
    ///
    /// This could IO errors while waiting for events, or a timeout, etc.
    fn on_error(&mut self, err: ::Error) -> Next where Self: Sized {
        debug!("default Handler.on_error({:?})", err);
        http::Next::remove()
    }

    /// This event occurs when this Handler has requested to remove the Transport.
    fn on_remove(self, _transport: T) where Self: Sized {
        debug!("default Handler.on_remove");
    }
}
*/


/// Used to create a `Handler` when a new transaction is received by the server.
pub trait HandlerFactory<T: Transport> {
    /// The `Handler` to use for the incoming transaction.
    type Output: Handler<T>;
    /// Creates the associated `Handler`.
    fn create(&mut self) -> Self::Output;
}

impl<F, H, T> HandlerFactory<T> for F
where F: FnMut() -> H, H: Handler<T>, T: Transport {
    type Output = H;
    fn create(&mut self) -> H {
        self()
    }
}
