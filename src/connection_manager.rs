// Copyright 2015 MaidSafe.net limited
// This MaidSafe Software is licensed to you under (1) the MaidSafe.net Commercial License,
// version 1.0 or later, or (2) The General Public License (GPL), version 3, depending on which
// licence you accepted on initial access to the Software (the "Licences").
// By contributing code to the MaidSafe Software, or to this project generally, you agree to be
// bound by the terms of the MaidSafe Contributor Agreement, versicant_sendon 1.0, found in the root
// directory of this project at LICENSE, COPYING and CONTRIBUTOR respectively and also
// available at: http://www.maidsafe.net/licenses
// Unless required by applicable law or agreed to in writing, the MaidSafe Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS
// OF ANY KIND, either express or implied.
// See the Licences for the specific language governing permissions and limitations relating to
// use of the MaidSafe
// Software.

use std::net::{SocketAddr};
use std::io::Error as IoError;
use std::io;
use std::collections::{HashMap};
use std::thread::spawn;
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::sync::mpsc;
use tcp_connections::{listen, connect_tcp, TcpReader, TcpWriter, upgrade_tcp};

pub type Bytes   = Vec<u8>;

pub type IoResult<T> = Result<T, IoError>;

pub type IoReceiver<T> = Receiver<T>;
pub type IoSender<T>   = Sender<T>;

type SocketReader = TcpReader<Bytes>;
type SocketWriter = TcpWriter<Bytes>;

type WeakState = Weak<Mutex<State>>;

/// A structure representing a connection manager
pub struct ConnectionManager {
    state: Arc<Mutex<State>>,
}

/// Enum representing endpoint of supported protocols
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum Endpoint {
    Tcp(SocketAddr),
}

#[derive(Debug, Clone)]
pub enum PortAndProtocol {
    Tcp(u16),
}

/// Enum representing different events that will be sent over the asynchronous channel to the user
/// of this module.
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum Event {
    NewMessage(Endpoint, Bytes),
    NewConnection(Endpoint),
    LostConnection(Endpoint),
    FailedToConnect(Vec<Endpoint>)
}

struct Connection {
    writer_channel: mpsc::Sender<Bytes>,
}

struct State {
    event_pipe: IoSender<Event>,
    connections: HashMap<Endpoint, Connection>,
}

impl ConnectionManager {
    /// Constructs a connection manager.
    /// User needs to create an asynchronous channel, and provide the sender half to this method.
    /// Receiver half will recieve all the events `Event` from this library.
    pub fn new(event_pipe: IoSender<Event>) -> ConnectionManager {
        let connections: HashMap<Endpoint, Connection> = HashMap::new();
        let state = Arc::new(Mutex::new(State{ event_pipe: event_pipe,
                                               connections: connections }));
        ConnectionManager { state: state }
    }

    /// Starts listening on all supported protocols. Specified hint will be tried first,
    /// if it fails to start on these, it defaults to random / OS provided endpoints for each
    /// supported protocol. The actual endpoints used will be returned on which it started listening
    /// for each protocol.
    pub fn start_listening(&self, hint: Vec<PortAndProtocol>) -> IoResult<Vec<Endpoint>> {
        let weak_state = self.state.downgrade();
        let (event_receiver, listener) = try!(listen());

        // FIXME: Try listen on ports given by the `hint` parameter.
        let local_addr = try!(listener.local_addr());

        spawn(move || {
            for x in event_receiver.iter() {
                let (connection, _) = x;
                let s = weak_state.clone();
                let his_ep = Endpoint::Tcp(connection.peer_addr().unwrap());
                spawn(move || {
                    let _ =
                        upgrade_tcp(connection)
                        .and_then(|(i, o)| { handle_accept(s, his_ep, i, o) });
                });
            }
        });

        Ok(vec![Endpoint::Tcp(local_addr)])
    }

    /// This method tries to connect (bootstrap to exisiting network) to the default or provided
    /// list of bootstrap nodes. If the bootstrap list is `Some`, the method will try to connect to
    /// all of the endpoints specified in `bootstrap_list`. It will return once connection with any of the
    /// endpoint is established with Ok(Endpoint) and it will drop all other ongoing attempts.
    /// Returns Err if if fails to connect to any of the
    /// endpoints specified.
    /// If `bootstrap_list` is `None`, it will use default methods to bootstrap to the existing network.
    /// Default methods includes beacon system for finding nodes on a local network
    /// and bootstrap handler which will attempt to reconnect to any previous "direct connected" nodes.
    /// In both cases, this method blocks until it gets one successful connection or all the endpoints
    /// are tried and failed.
    pub fn bootstrap(&self, bootstrap_list: Option<Vec<Endpoint>>) -> IoResult<Endpoint> {
        unimplemented!()
    }


    /// Opens a connection to a remote peer. `endpoints` is a vector of addresses of the remote peer.
    /// All the endpoints will be tried. As soon as one of the connection is established,
    /// it will drop all other ongoing attempt. On success `Event::NewConnection` with connected `Endpoint`
    /// will be sent to the event channel. On failure to connect to any of the provided endpoints,
    /// `Event::FailedToConnect` will be sent to the event channel.
    pub fn connect(&self, endpoints: Vec<Endpoint>) {
        let ws = self.state.downgrade();

        // FIXME: Handle situations where endpoints.len() < or > then 1
        assert!(endpoints.len() == 1, "TODO");

        let endpoint = endpoints[0].clone();

        match endpoint.clone() {
            Endpoint::Tcp(addr) => {
                spawn(move || {
                    println!("Connecting");
                    let _ = connect_tcp(addr.clone())
                            .and_then(|(i, o)| { handle_connect(ws, endpoint, i, o) });
                });
            },
        }
    }

    /// Sends a message to specified address (endpoint). Returns Ok(()) if the sending might succeed, and returns an
    /// Err if the address is not connected. Return value of Ok does not mean that the data will be
    /// received. It is possible for the corresponding connection to hang up immediately after this
    /// function returns Ok.
    pub fn send(&self, endpoint: Endpoint, message: Bytes)-> IoResult<()> {
        let ws = self.state.downgrade();

        let writer_channel = try!(lock_state(&ws, |s| {
            match s.connections.get(&endpoint) {
                Some(c) =>  Ok(c.writer_channel.clone()),
                None => Err(io::Error::new(io::ErrorKind::NotConnected, "?"))
            }
        }));

        let send_result = writer_channel.send(message);
        let cant_send = io::Error::new(io::ErrorKind::BrokenPipe, "?");
        send_result.map_err(|_|cant_send)
    }

    /// Closes connection with the specified endpoint
    pub fn drop_node(&self, endpoint: Endpoint) {
        let mut ws = self.state.downgrade();
        let _ = lock_mut_state(&mut ws, |s: &mut State| {
            s.connections.remove(&endpoint);
            Ok(())
        });
    }

}

fn lock_state<T, F: Fn(&State) -> IoResult<T>>(state: &WeakState, f: F) -> IoResult<T> {
    state.upgrade().ok_or(io::Error::new(io::ErrorKind::Interrupted,
                                         "Can't dereference weak"))
    .and_then(|arc_state| {
        let opt_state = arc_state.lock();
        match opt_state {
            Ok(s)  => f(&s),
            Err(e) => Err(io::Error::new(io::ErrorKind::Interrupted, "?"))
        }
    })
}

fn lock_mut_state<T, F: FnOnce(&mut State) -> IoResult<T>>(state: &WeakState, f: F) -> IoResult<T> {
    state.upgrade().ok_or(io::Error::new(io::ErrorKind::Interrupted,
                                         "Can't dereference weak"))
    .and_then(move |arc_state| {
        let opt_state = arc_state.lock();
        match opt_state {
            Ok(mut s)  => f(&mut s),
            Err(e) => Err(io::Error::new(io::ErrorKind::Interrupted, "?"))
        }
    })
}

fn handle_accept(mut state: WeakState,
                 his_ep: Endpoint,
                 i: SocketReader,
                 o: SocketWriter) -> IoResult<()> {
    register_connection(&mut state, his_ep.clone(), i, o, Event::NewConnection(his_ep))
}

fn handle_connect(mut state: WeakState,
                  his_ep: Endpoint,
                  i: SocketReader,
                  o: SocketWriter) -> IoResult<()> {
    println!("handle_connect");
    register_connection(&mut state, his_ep.clone(), i, o, Event::NewConnection(his_ep))
}

fn register_connection( state: &mut WeakState
                      , his_ep: Endpoint
                      , i: SocketReader
                      , o: SocketWriter
                      , event_to_user: Event
                      ) -> IoResult<()> {

    let state2 = state.clone();

    lock_mut_state(state, move |s: &mut State| {
        println!("register_connection");
        let (tx, rx) = mpsc::channel();
        start_writing_thread(state2.clone(), o, his_ep.clone(), rx);
        start_reading_thread(state2, i, his_ep.clone(), s.event_pipe.clone());
        s.connections.insert(his_ep, Connection{writer_channel: tx});
        let _ = s.event_pipe.send(event_to_user);
        Ok(())
    })
}

fn unregister_connection(state: WeakState, his_ep: Endpoint) {
    println!("unregister_connection");
    let _ = lock_mut_state(&state, |s| {
        if s.connections.remove(&his_ep).is_some() {
            // Only send the event if the connection was there
            // to avoid duplicate events.
            let _ = s.event_pipe.send(Event::LostConnection(his_ep));
        }
        Ok(())
    });
}

// pushing events out to event_pipe
fn start_reading_thread(state: WeakState,
                        i: SocketReader,
                        his_ep: Endpoint,
                        sink: IoSender<Event>) {
    spawn(move || {
        for msg in i.iter() {
            if sink.send(Event::NewMessage(his_ep.clone(), msg)).is_err() {
                break;
            }
        }
        unregister_connection(state, his_ep);
    });
}

// pushing messges out to socket
fn start_writing_thread(state: WeakState,
                        mut o: SocketWriter,
                        his_ep: Endpoint,
                        writer_channel: mpsc::Receiver<Bytes>) {
    spawn(move || {
        for msg in writer_channel.iter() {
            if o.send(&msg).is_err() {
                break;
            }
        }
        unregister_connection(state, his_ep);
        });
}

// FIXME need timer
//fn exchange(socket_input:  SocketReader, socket_output: SocketWriter, data: Bytes)
//            -> IoResult<(SocketReader, SocketWriter, Bytes)>
//{
//    let (output, input) = mpsc::channel();
//
//    spawn(move || {
//        let mut s = socket_output;
//        if s.send(&data).is_err() {
//            return;
//        }
//        let _ = output.send(s);
//    });
//
//    let opt_result = socket_input.recv();
//    let opt_send_result = input.recv();
//
//    let cant_send = io::Error::new(io::ErrorKind::Other,
//                                   "Can't exchage (send error)");
//    let cant_recv = io::Error::new(io::ErrorKind::Other,
//                                   "Can't exchage (send error)");
//
//    let socket_output = try!(opt_send_result.map_err(|_|cant_send));
//    let result = try!(opt_result.map_err(|_|cant_recv));
//
//    Ok((socket_input, socket_output, result))
//}

#[cfg(test)]
mod test {
    use super::*;
    use std::thread::spawn;
    use std::sync::mpsc::{Receiver, channel};
    use rustc_serialize::{Decodable, Encodable};
    use cbor::{Encoder, Decoder};

    fn encode<T>(value: &T) -> Bytes where T: Encodable
    {
        let mut enc = Encoder::from_memory();
        let _ = enc.encode(&[value]);
        enc.into_bytes()
    }

    fn decode<T>(bytes: Bytes) -> T where T: Decodable {
        let mut dec = Decoder::from_bytes(&bytes[..]);
        dec.decode().next().unwrap().unwrap()
    }

#[test]
    fn connection_manager() {
        let run_cm = |cm: ConnectionManager, o: Receiver<Event>| {
            spawn(move || {
                for i in o.iter() {
                    match i {
                        Event::NewConnection(other_ep) => {
                            println!("Connected {:?}", other_ep);
                            cm.send(other_ep.clone(), encode(&"hello world".to_string()));
                        },
                        Event::NewMessage(from_ep, data) => {
                            println!("New message from {:?} data:{:?}",
                                     from_ep, decode::<String>(data));
                            break;
                        },
                        Event::LostConnection(other_ep) => {
                            println!("Lost connection to {:?}", other_ep);
                        },
                        _ => println!("unhandled"),
                    }
                }
                println!("done");
            })
        };

        let (cm1_i, cm1_o) = channel();
        let cm1 = ConnectionManager::new(cm1_i);
        let cm1_eps = cm1.start_listening(Vec::new()).unwrap();

        let (cm2_i, cm2_o) = channel();
        let cm2 = ConnectionManager::new(cm2_i);
        let cm2_eps = cm2.start_listening(Vec::new()).unwrap();
        cm2.connect(cm1_eps.clone());

        let runner1 = run_cm(cm1, cm1_o);
        let runner2 = run_cm(cm2, cm2_o);

        assert!(runner1.join().is_ok());
        assert!(runner2.join().is_ok());
    }
}
