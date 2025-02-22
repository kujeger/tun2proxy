use crate::tun2proxy::{
    Connection, ConnectionManager, IncomingDataEvent, IncomingDirection, OutgoingDataEvent,
    OutgoingDirection, ProxyError, TcpProxy,
};
use std::collections::VecDeque;
use std::net::SocketAddr;

#[derive(Eq, PartialEq, Debug)]
#[allow(dead_code)]
enum HttpState {
    SendRequest,
    ExpectStatusCode,
    ExpectResponse,
    Established,
}

pub struct HttpConnection {
    state: HttpState,
    client_inbuf: VecDeque<u8>,
    server_inbuf: VecDeque<u8>,
    client_outbuf: VecDeque<u8>,
    server_outbuf: VecDeque<u8>,
    data_buf: VecDeque<u8>,
    crlf_state: u8,
}

impl HttpConnection {
    fn new(connection: &Connection) -> Self {
        let mut result = Self {
            state: HttpState::ExpectStatusCode,
            client_inbuf: Default::default(),
            server_inbuf: Default::default(),
            client_outbuf: Default::default(),
            server_outbuf: Default::default(),
            data_buf: Default::default(),
            crlf_state: Default::default(),
        };

        result.server_outbuf.extend(b"CONNECT ".iter());
        result
            .server_outbuf
            .extend(connection.dst.to_string().as_bytes());
        result.server_outbuf.extend(b" HTTP/1.1\r\nHost: ".iter());
        result
            .server_outbuf
            .extend(connection.dst.to_string().as_bytes());
        result.server_outbuf.extend(b"\r\n\r\n".iter());

        result
    }

    fn state_change(&mut self) -> Result<(), ProxyError> {
        match self.state {
            HttpState::ExpectStatusCode if self.server_inbuf.len() >= "HTTP/1.1 200 ".len() => {
                let status_line: Vec<u8> = self
                    .server_inbuf
                    .range(0.."HTTP/1.1 200 ".len())
                    .copied()
                    .collect();
                let slice = &status_line.as_slice()[0.."HTTP/1.1 2".len()];
                if slice != b"HTTP/1.1 2" && slice != b"HTTP/1.0 2"
                    || self.server_inbuf["HTTP/1.1 200 ".len() - 1] != b' '
                {
                    let status_str =
                        String::from_utf8_lossy(&status_line.as_slice()[0.."HTTP/1.1 200".len()]);
                    return Err(ProxyError::new(
                        "Expected success status code. Server replied with ".to_owned()
                            + &*status_str
                            + ".",
                    ));
                }
                self.state = HttpState::ExpectResponse;
                return self.state_change();
            }
            HttpState::ExpectResponse => {
                let mut counter = 0usize;
                for b_ref in self.server_inbuf.iter() {
                    let b = *b_ref;
                    if b == b'\n' {
                        self.crlf_state += 1;
                    } else if b != b'\r' {
                        self.crlf_state = 0;
                    }
                    counter += 1;

                    if self.crlf_state == 2 {
                        self.server_inbuf.drain(0..counter);

                        self.server_outbuf.append(&mut self.data_buf);
                        self.data_buf.clear();

                        self.state = HttpState::Established;
                        return self.state_change();
                    }
                }

                self.server_inbuf.drain(0..counter);
            }
            HttpState::Established => {
                self.client_outbuf.extend(self.server_inbuf.iter());
                self.server_outbuf.extend(self.client_inbuf.iter());
                self.server_inbuf.clear();
                self.client_inbuf.clear();
            }
            _ => {}
        }
        Ok(())
    }
}

impl TcpProxy for HttpConnection {
    fn push_data(&mut self, event: IncomingDataEvent<'_>) -> Result<(), ProxyError> {
        let direction = event.direction;
        let buffer = event.buffer;
        match direction {
            IncomingDirection::FromServer => {
                self.server_inbuf.extend(buffer.iter());
            }
            IncomingDirection::FromClient => {
                if self.state == HttpState::Established {
                    self.client_inbuf.extend(buffer.iter());
                } else {
                    self.data_buf.extend(buffer.iter());
                }
            }
        }

        self.state_change()
    }

    fn consume_data(&mut self, dir: OutgoingDirection, size: usize) {
        let buffer = if dir == OutgoingDirection::ToServer {
            &mut self.server_outbuf
        } else {
            &mut self.client_outbuf
        };
        buffer.drain(0..size);
    }

    fn peek_data(&mut self, dir: OutgoingDirection) -> OutgoingDataEvent {
        let buffer = if dir == OutgoingDirection::ToServer {
            &mut self.server_outbuf
        } else {
            &mut self.client_outbuf
        };
        OutgoingDataEvent {
            direction: dir,
            buffer: buffer.make_contiguous(),
        }
    }

    fn connection_established(&self) -> bool {
        self.state == HttpState::Established
    }
}

pub struct HttpManager {
    server: std::net::SocketAddr,
}

impl ConnectionManager for HttpManager {
    fn handles_connection(&self, connection: &Connection) -> bool {
        connection.proto == smoltcp::wire::IpProtocol::Tcp.into()
    }

    fn new_connection(&mut self, connection: &Connection) -> Option<std::boxed::Box<dyn TcpProxy>> {
        if connection.proto != smoltcp::wire::IpProtocol::Tcp.into() {
            return None;
        }
        Some(std::boxed::Box::new(HttpConnection::new(connection)))
    }

    fn close_connection(&mut self, _: &Connection) {}

    fn get_server(&self) -> SocketAddr {
        self.server
    }
}

impl HttpManager {
    pub fn new(server: SocketAddr) -> Self {
        Self { server }
    }
}
