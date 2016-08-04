pub struct Transaction<I, O> {
    incoming: Option<::Result<I>>,
    outgoing: O,
    reading: Decoder,
    writing: Encoder,
}

enum State {
    Ready,
    End
}

pub type ServerTxn = Transaction<RequestLine, StatusCode>;
pub type ClientTxn = Transaction<RawStatus, RequestLine>;
