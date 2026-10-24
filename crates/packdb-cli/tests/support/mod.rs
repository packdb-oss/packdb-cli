// Copyright 2025 PackDB Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Fixtures shared by the integration tests.

#![allow(clippy::expect_used)]

use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};

/// An endpoint that refuses connections. Its port is the local end of an open
/// loopback connection: nothing listens on it, and while this is held the
/// system does not assign the port to another socket bound to port 0, as every
/// test server here is. The refusal is checked here, not assumed.
pub struct Refusing {
    pub endpoint: String,
    _held: (TcpListener, TcpStream, TcpStream),
}

pub fn refusing_endpoint() -> Refusing {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let client =
        TcpStream::connect(listener.local_addr().expect("listener addr")).expect("connect");
    let (server, _) = listener.accept().expect("accept");
    let addr = client.local_addr().expect("local addr");
    let refused = TcpStream::connect(addr).expect_err("nothing listens");
    assert_eq!(refused.kind(), ErrorKind::ConnectionRefused, "{refused}");
    Refusing {
        endpoint: format!("http://{addr}"),
        _held: (listener, client, server),
    }
}
