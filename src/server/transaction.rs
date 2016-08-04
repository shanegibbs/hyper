use std::marker::PhantomData;


use http::{self, Next};
use net::Transport;

use super::{Handler, request, response};

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

impl<H: Handler<T>, T: Transport> http::TransactionHandler for Handle<H, T> {
    type Transaction = http::ServerTransaction;

    fn ready(&mut self, txn: &mut http::ServerTransaction<T>) {
        self.handler.ready(txn)
    }
}

pub struct Transaction<'a, T: 'a> {
    inner: &'a mut http::ServerTransaction<T>,
}

impl<'a, T: Transport + 'a> Transaction<'a, T> {
    pub fn request(&mut self) -> ::Result<Request> {
        self.inner.incoming().unwrap()
    }

    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }

    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        self.inner.try_read(buf)
    }

    pub fn response(&mut self) -> &mut Response {
        self.inner.outgoing_mut()
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.inner.write(data)
    }

    pub fn try_write(&mut self, data: &[u8]) -> io::Result<Option<usize>> {
        self.inner.try_write(data)
    }
}
/*
impl<H: Handler<T>, T: Transport> http::TransactionHandler<T> for Transaction<H, T> {
    type Transaction = http::ServerTransaction;

    fn on_incoming(&mut self, head: http::RequestHead, transport: &T) -> Next {
        trace!("on_incoming {:?}", head);
        let req = request::new(head, transport);
        self.handler.on_request(req)
    }

    fn on_decode(&mut self, transport: &mut http::Decoder<T>) -> Next {
        self.handler.on_request_readable(transport)
    }

    fn on_outgoing(&mut self, head: &mut http::MessageHead<::status::StatusCode>) -> Next {
        let mut res = response::new(head);
        self.handler.on_response(&mut res)
    }

    fn on_encode(&mut self, transport: &mut http::Encoder<T>) -> Next {
        self.handler.on_response_writable(transport)
    }

    fn on_error(&mut self, error: ::Error) -> Next {
        self.handler.on_error(error)
    }

    fn on_remove(self, transport: T) {
        self.handler.on_remove(transport);
    }
}
*/
