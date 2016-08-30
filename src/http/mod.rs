//! Pieces pertaining to the HTTP message protocol.
use std::borrow::Cow;
use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;

use header::Connection;
use header::ConnectionOption::{KeepAlive, Close};
use header::Headers;
use method::Method;
use net::Transport;
use status::StatusCode;
use uri::RequestUri;
use version::HttpVersion;
use version::HttpVersion::{Http10, Http11};

#[cfg(feature = "serde-serialization")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use self::conn::{Conn, TransactionHandler, ConnectionHandler, ConnectionHandlerFactory, Seed, Key};

mod buffer;
mod conn;
mod h1;
//mod h2;

pub struct Transaction<'a, T: Transport + 'a, X: Http1Transaction + 'a> {
    inner: TransactionImpl<'a, T, X>,
}

enum TransactionImpl<'a, T: Transport + 'a, X: Http1Transaction + 'a> {
    H1(h1::txn::TxnIo<'a, T, X>)
}

macro_rules! nonblocking {
    ($e:expr) => ({
        match $e {
            Ok(n) => Ok(Some(n)),
            Err(e) => match e.kind() {
                io::ErrorKind::WouldBlock => Ok(None),
                _ => Err(e)
            }
        }
    });
}

impl<'a, T: Transport + 'a, X: Http1Transaction + 'a> Transaction<'a, T, X> {

    fn h1(txn: h1::txn::TxnIo<'a, T, X>) -> Transaction<'a, T, X> {
        Transaction {
            inner: TransactionImpl::H1(txn),
        }
    }

    pub fn incoming(&mut self) -> ::Result<MessageHead<X::Incoming>> {
        unimplemented!("Txn.incoming")
    }

    #[inline]
    pub fn outgoing(&mut self) -> &mut MessageHead<X::Outgoing> {
        match self.inner {
            TransactionImpl::H1(ref mut txn) => txn.outgoing(),
        }
    }

    #[inline]
    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.inner {
            TransactionImpl::H1(ref mut txn) => txn.write(data),
        }
    }

    #[inline]
    pub fn try_write(&mut self, data: &[u8]) -> io::Result<Option<usize>> {
        nonblocking!(self.write(data))
    }

    #[inline]
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner {
            TransactionImpl::H1(ref mut txn) => txn.read(buf),
        }
    }

    #[inline]
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        nonblocking!(self.read(buf))
    }

    #[inline]
    pub fn end(&mut self) {
        unimplemented!("Txn.end")
    }
}

/// Wraps a `Transport` to provide HTTP decoding when reading.
#[derive(Debug)]
pub struct Decoder<'a, T: Read + 'a>(DecoderImpl<'a, T>);

/// Wraps a `Transport` to provide HTTP encoding when writing.
#[derive(Debug)]
pub struct Encoder<'a, T: Transport + 'a>(EncoderImpl<'a, T>);

#[derive(Debug)]
enum DecoderImpl<'a, T: Read + 'a> {
    H1(&'a mut h1::Decoder, Trans<'a, T>),
}

#[derive(Debug)]
enum Trans<'a, T: Read + 'a> {
    Port(&'a mut T),
    Buf(self::buffer::BufReader<'a, T>)
}

impl<'a, T: Read + 'a> Trans<'a, T> {
    fn get_ref(&self) -> &T {
        match *self {
            Trans::Port(ref t) => &*t,
            Trans::Buf(ref buf) => buf.get_ref()
        }
    }
}

impl<'a, T: Read + 'a> Read for Trans<'a, T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match *self {
            Trans::Port(ref mut t) => t.read(buf),
            Trans::Buf(ref mut b) => b.read(buf)
        }
    }
}

#[derive(Debug)]
enum EncoderImpl<'a, T: Transport + 'a> {
    H1(&'a mut h1::Encoder, &'a mut T),
}

impl<'a, T: Read> Decoder<'a, T> {
    fn h1(decoder: &'a mut h1::Decoder, transport: Trans<'a, T>) -> Decoder<'a, T> {
        Decoder(DecoderImpl::H1(decoder, transport))
    }

    /// Read from the `Transport`.
    #[inline]
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0 {
            DecoderImpl::H1(ref mut decoder, ref mut transport) => {
                decoder.decode(transport, buf)
            }
        }
    }

    /// Try to read from the `Transport`.
    ///
    /// This method looks for the `WouldBlock` error. If the read did not block,
    /// a return value would be `Ok(Some(x))`. If the read would block,
    /// this method would return `Ok(None)`.
    #[inline]
    pub fn try_read(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        match self.read(buf) {
            Ok(n) => Ok(Some(n)),
            Err(e) => match e.kind() {
                io::ErrorKind::WouldBlock => Ok(None),
                _ => Err(e)
            }
        }
    }

    /// Get a reference to the transport.
    pub fn get_ref(&self) -> &T {
        match self.0 {
            DecoderImpl::H1(_, ref transport) => transport.get_ref()
        }
    }
}

impl<'a, T: Transport> Encoder<'a, T> {
    fn h1(encoder: &'a mut h1::Encoder, transport: &'a mut T) -> Encoder<'a, T> {
        Encoder(EncoderImpl::H1(encoder, transport))
    }

    /// Write to the `Transport`.
    #[inline]
    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        match self.0 {
            EncoderImpl::H1(ref mut encoder, ref mut transport) => {
                if encoder.is_closed() {
                    Ok(0)
                } else {
                    encoder.encode(*transport, data)
                }
            }
        }
    }

    /// Try to write to the `Transport`.
    ///
    /// This method looks for the `WouldBlock` error. If the write did not block,
    /// a return value would be `Ok(Some(x))`. If the write would block,
    /// this method would return `Ok(None)`.
    #[inline]
    pub fn try_write(&mut self, data: &[u8]) -> io::Result<Option<usize>> {
        match self.write(data) {
            Ok(n) => Ok(Some(n)),
            Err(e) => match e.kind() {
                io::ErrorKind::WouldBlock => Ok(None),
                _ => Err(e)
            }
        }
    }

    /// Closes an encoder, signaling that no more writing will occur.
    ///
    /// This is needed for encodings that don't know the length of the content
    /// beforehand. Most common instance would be usage of
    /// `Transfer-Enciding: chunked`. You would call `close()` to signal
    /// the `Encoder` should write the end chunk, or `0\r\n\r\n`.
    pub fn close(&mut self) {
        match self.0 {
            EncoderImpl::H1(ref mut encoder, _) => encoder.close()
        }
    }

    /// Get a reference to the transport.
    pub fn get_ref(&self) -> &T {
        match self.0 {
            EncoderImpl::H1(_, ref transport) => &*transport
        }
    }
}

impl<'a, T: Read> Read for Decoder<'a, T> {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read(buf)
    }
}

impl<'a, T: Transport> Write for Encoder<'a, T> {
    #[inline]
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.write(data)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        match self.0 {
            EncoderImpl::H1(_, ref mut transport) => {
                transport.flush()
            }
        }
    }
}

/*
/// Because privacy rules. Reasons.
/// https://github.com/rust-lang/rust/issues/30905
mod internal {
    use std::io::{self, Write};
    */

#[derive(Debug, Clone)]
pub struct WriteBuf<T: AsRef<[u8]>> {
    pub bytes: T,
    pub pos: usize,
}

impl<T: AsRef<[u8]>> WriteBuf<T> {
    pub fn new(bytes: T) -> WriteBuf<T> {
        WriteBuf {
            bytes: bytes,
            pos: 0,
        }
    }
}

pub trait AtomicWrite {
    fn write_atomic(&mut self, data: &[&[u8]]) -> io::Result<usize>;
}

/*
#[cfg(not(windows))]
impl<T: Write + ::vecio::Writev> AtomicWrite for T {

    fn write_atomic(&mut self, bufs: &[&[u8]]) -> io::Result<usize> {
        self.writev(bufs)
    }

}

#[cfg(windows)]
*/
impl<T: Write> AtomicWrite for T {
    fn write_atomic(&mut self, bufs: &[&[u8]]) -> io::Result<usize> {
        if cfg!(not(windows)) {
            warn!("write_atomic not using writev");
        }
        let vec = bufs.concat();
        self.write(&vec)
    }
}
//}

pub struct Io<T> {
    buf: self::buffer::Buffer,
    transport: T,
}

impl<T> fmt::Debug for Io<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Io")
            .field("buf", &self.buf)
            .finish()
    }
}

const MAX_BUFFER_SIZE: usize = 8192 + 4096 * 100;

impl<T: Transport> Io<T> {
    fn parse<S: Http1Transaction>(&mut self) -> ::Result<Option<MessageHead<S::Incoming>>> {
        match self.buf.read_from(&mut self.transport) {
            Ok(0) => {
                trace!("parse eof");
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "parse eof").into());
            }
            Ok(_) => {},
            Err(e) => match e.kind() {
                io::ErrorKind::WouldBlock => {},
                _ => return Err(e.into())
            }
        }
        match try!(parse::<S, _>(self.buf.bytes())) {
            Some((head, len)) => {
                trace!("parsed {} bytes out of {}", len, self.buf.len());
                self.buf.consume(len);
                Ok(Some(head))
            },
            None => {
                if self.buf.len() >= MAX_BUFFER_SIZE {
                    debug!("MAX_BUFFER_SIZE reached, closing");
                    Err(::Error::TooLarge)
                } else {
                    Ok(None)
                }
            },
        }
    }
}

/// An Incoming Message head. Includes request/status line, and headers.
#[derive(Debug, Default)]
pub struct MessageHead<S> {
    /// HTTP version of the message.
    pub version: HttpVersion,
    /// Subject (request line or status line) of Incoming message.
    pub subject: S,
    /// Headers of the Incoming message.
    pub headers: Headers
}

/// An incoming request message.
pub type RequestHead = MessageHead<RequestLine>;

#[derive(Debug, Default)]
pub struct RequestLine(pub Method, pub RequestUri);

impl fmt::Display for RequestLine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {}", self.0, self.1)
    }
}

/// An incoming response message.
pub type ResponseHead = MessageHead<RawStatus>;

impl<S> MessageHead<S> {
    pub fn should_keep_alive(&self) -> bool {
        should_keep_alive(self.version, &self.headers)
    }
}

/// The raw status code and reason-phrase.
#[derive(Clone, PartialEq, Debug)]
pub struct RawStatus(pub u16, pub Cow<'static, str>);

impl fmt::Display for RawStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {}", self.0, self.1)
    }
}

impl From<StatusCode> for RawStatus {
    fn from(status: StatusCode) -> RawStatus {
        RawStatus(status.to_u16(), Cow::Borrowed(status.canonical_reason().unwrap_or("")))
    }
}

impl Default for RawStatus {
    fn default() -> RawStatus {
        RawStatus(200, Cow::Borrowed("OK"))
    }
}

#[cfg(feature = "serde-serialization")]
impl Serialize for RawStatus {
    fn serialize<S>(&self, serializer: &mut S) -> Result<(), S::Error> where S: Serializer {
        (self.0, &self.1).serialize(serializer)
    }
}

#[cfg(feature = "serde-serialization")]
impl Deserialize for RawStatus {
    fn deserialize<D>(deserializer: &mut D) -> Result<RawStatus, D::Error> where D: Deserializer {
        let representation: (u16, String) = try!(Deserialize::deserialize(deserializer));
        Ok(RawStatus(representation.0, Cow::Owned(representation.1)))
    }
}

/// Checks if a connection should be kept alive.
#[inline]
pub fn should_keep_alive(version: HttpVersion, headers: &Headers) -> bool {
    let ret = match (version, headers.get::<Connection>()) {
        (Http10, None) => false,
        (Http10, Some(conn)) if !conn.contains(&KeepAlive) => false,
        (Http11, Some(conn)) if conn.contains(&Close)  => false,
        _ => true
    };
    trace!("should_keep_alive(version={:?}, header={:?}) = {:?}", version, headers.get::<Connection>(), ret);
    ret
}

pub type ParseResult<T> = ::Result<Option<(MessageHead<T>, usize)>>;

pub fn parse<T: Http1Transaction<Incoming=I>, I>(rdr: &[u8]) -> ParseResult<I> {
    h1::parse::<T, I>(rdr)
}

#[derive(Debug)]
pub enum ServerTransaction {}

#[derive(Debug)]
pub enum ClientTransaction {}

pub trait Http1Transaction {
    type Incoming;
    type Outgoing: Default;
    fn parse(bytes: &[u8]) -> ParseResult<Self::Incoming>;
    fn decoder(head: &MessageHead<Self::Incoming>) -> ::Result<h1::Decoder>;
    fn encode(head: &mut MessageHead<Self::Outgoing>, dst: &mut Vec<u8>) -> h1::Encoder;
}


#[test]
fn test_should_keep_alive() {
    let mut headers = Headers::new();

    assert!(!should_keep_alive(Http10, &headers));
    assert!(should_keep_alive(Http11, &headers));

    headers.set(Connection::close());
    assert!(!should_keep_alive(Http10, &headers));
    assert!(!should_keep_alive(Http11, &headers));

    headers.set(Connection::keep_alive());
    assert!(should_keep_alive(Http10, &headers));
    assert!(should_keep_alive(Http11, &headers));
}
