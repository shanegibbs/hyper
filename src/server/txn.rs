use std::marker::PhantomData;
use std::io;


use http;
use net::Transport;

use super::{Handler, request, response, Request, Response};

/// A `TransactionHandler` for a Server.
///
/// This should be really thin glue between `http::TransactionHandler` and
/// `server::Handler`, but largely just providing the proper types one
/// would expect in a Server Handler.
pub struct Handle<H: Handler<T>, T: Transport> {
    handler: H,
    _marker: PhantomData<T>
}

impl<H: Handler<T>, T: Transport> Handle<H, T> {
    pub fn new(handler: H) -> Handle<H, T> {
        Handle {
            handler: handler,
            _marker: PhantomData,
        }
    }
}

impl<H: Handler<T>, T: Transport> http::TransactionHandler<T> for Handle<H, T> {
    type Transaction = http::ServerTransaction;

    #[inline]
    fn ready(&mut self, txn: &mut http::Transaction<T, Self::Transaction>) {
        let mut outer = Transaction { inner: txn };
        self.handler.ready(&mut outer)
    }
}

pub struct Transaction<'a: 'b, 'b, T: Transport + 'a> {
    inner: &'b mut http::Transaction<'a, T, http::ServerTransaction>,
}

impl<'a: 'b, 'b, T: Transport + 'a> Transaction<'a, 'b, T> {
    /*
    #[inline]
    pub fn request(&mut self) -> ::Result<Request> {
        self.inner.incoming().map(request::new)
    }
    */

    #[inline]
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }

    #[inline]
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.inner.try_read(buf)
    }

    #[inline]
    pub fn response(&mut self) -> Response {
        response::new(self.inner.outgoing())
    }

    #[inline]
    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.inner.write(data)
    }

    #[inline]
    pub fn try_write(&mut self, data: &[u8]) -> io::Result<Option<usize>> {
        self.inner.try_write(data)
    }

    #[inline]
    pub fn end(&mut self) {
        self.inner.end();
    }
}
