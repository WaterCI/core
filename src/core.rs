use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind::{Interrupted, WouldBlock, WriteZero};
use std::io::{Read, Write};
use std::sync::mpsc::Receiver;
use std::time::Duration;
use std::{io, thread};

use anyhow::{Error, Result};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Poll, Registry, Token};
use tracing::{debug, error, info, trace};
use tracing::{instrument, warn};
use waterlib::net::ExecutorMessage::{BuildRequest, ExecutorRegisterResponse, ExecutorStatusQuery};
use waterlib::net::ExecutorStatus::{Available, YetToRegister};
use waterlib::net::{BuildRequestFromRepo, ExecutorMessage, ExecutorStatus, IncomingMessage};
use waterlib::utils::gen_uuid;

use crate::Config;

const INCOMING_SERVER: Token = Token(0);
const EXECUTOR_SERVER: Token = Token(1);

#[instrument]
fn next(current: &mut Token) -> Token {
    let next = current.0;
    current.0 += 1;
    Token(next)
}

#[derive(Debug)]
struct Executor {
    id: Option<String>,
    status: ExecutorStatus,
    stream: TcpStream,
    token: Token,
    pending_msgs: VecDeque<ExecutorMessage>,
}

impl Executor {
    fn queue_message(&mut self, message: ExecutorMessage, registry: &Registry) -> Result<()> {
        if let BuildRequest(_) = message {
            self.pending_msgs.push_back(ExecutorStatusQuery);
        }
        self.pending_msgs.push_back(message);
        registry.reregister(&mut self.stream, self.token, Interest::WRITABLE)?;
        Ok(())
    }
    fn send_pending_message(&mut self) -> Result<()> {
        if let Some(msg) = self.pending_msgs.pop_front() {
            msg.write(&self.stream)?;
        }
        Ok(())
    }
    fn reregister(&mut self, registry: &Registry, interests: Interest) -> Result<()> {
        registry.reregister(&mut self.stream, self.token, interests)?;
        Ok(())
    }
}

type JobQueue = VecDeque<BuildRequestFromRepo>;

#[instrument]
pub fn run(config: &Config, should_stop_rx: Receiver<bool>) -> Result<()> {
    let mut queue = JobQueue::with_capacity(128);
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(128);
    let mut incoming_listener = TcpListener::bind(
        format!(
            "{}:{}",
            config.incoming_bind_host, config.incoming_bind_port
        )
        .parse()?,
    )?;
    poll.registry()
        .register(&mut incoming_listener, INCOMING_SERVER, Interest::READABLE)?;
    let mut executor_listener = TcpListener::bind(
        format!(
            "{}:{}",
            config.executor_bind_host, config.executor_bind_port
        )
        .parse()?,
    )?;
    poll.registry()
        .register(&mut executor_listener, EXECUTOR_SERVER, Interest::READABLE)?;
    let mut incoming_connections = HashMap::new();
    let mut executor_connections = HashMap::new();
    let mut unique_token = Token(EXECUTOR_SERVER.0 + 1);
    loop {
        // Check if we should quit
        if let Ok(should_stop) = should_stop_rx.try_recv() {
            if should_stop {
                break;
            }
        }
        // blocks until events are available or 100ms are reached
        debug!(
            "Waiting for events… executors={} queue={}",
            executor_connections.len(),
            queue.len()
        );
        poll.poll(&mut events, Some(Duration::from_millis(500)))?;
        for event in events.iter() {
            debug!("Got an event: {event:?}");
            match event.token() {
                INCOMING_SERVER => {
                    loop {
                        let (mut connection, address) = match incoming_listener.accept() {
                            Ok((conn, addr)) => (conn, addr),
                            Err(e) if e.kind() == WouldBlock => {
                                // means we don't have any more events, let's continue polling
                                break;
                            }
                            Err(e) => {
                                error!("Got an unexpected error on listener.accept(): {e}");
                                break;
                            }
                        };
                        info!("Accepted incoming connection from {:?}", address);
                        let token = next(&mut unique_token);
                        poll.registry()
                            .register(&mut connection, token, Interest::READABLE)?;
                        incoming_connections.insert(token, connection);
                    }
                }
                EXECUTOR_SERVER => loop {
                    let (mut connection, address) = match executor_listener.accept() {
                        Ok((conn, addr)) => (conn, addr),
                        Err(e) if e.kind() == WouldBlock => {
                            // means we don't have any more events, let's continue polling
                            break;
                        }
                        Err(e) => {
                            error!("Got an unexpected error on listener.accept(): {e}");
                            break;
                        }
                    };
                    info!("Accepted executor connection from {address}");
                    let token = next(&mut unique_token);
                    poll.registry()
                        .register(&mut connection, token, Interest::READABLE)?;
                    executor_connections.insert(
                        token,
                        Executor {
                            id: None,
                            status: YetToRegister,
                            stream: connection,
                            token: token,
                            pending_msgs: VecDeque::new(),
                        },
                    );
                },
                token => {
                    let mut has_closed_connection = false;
                    if let Some(connection) = incoming_connections.get_mut(&token) {
                        // Here, handle the incoming message event
                        if event.is_readable() {
                            match handle_incoming_read_event(connection) {
                                Ok(Some(msg)) => {
                                    // here, handle the message
                                    match msg {
                                        IncomingMessage::BuildRequestFromRepo(build_request) => {
                                            info!("Recieved incoming {build_request:?}");
                                            queue.push_back(build_request);
                                        }
                                    }
                                }
                                Ok(None) => {
                                    // here, msg is none, we should disconnect
                                    has_closed_connection = true;
                                }
                                Err(e) => {
                                    error!("{e}");
                                    todo!("Handle error");
                                }
                            };

                            //todo!("Handle incoming message event");
                        }
                    } else if let Some(executor) = executor_connections.get_mut(&token) {
                        if event.is_writable() && executor.pending_msgs.len() > 0 {
                            debug!(
                                "executor {:?} is writable and has items to send",
                                executor.id
                            );

                            let msg = executor.pending_msgs.pop_front().unwrap();
                            match msg {
                                ExecutorMessage::BuildRequest(_) => {
                                    if executor.status != Available {
                                        // then we need to push it back in front of the queue
                                        if let BuildRequest(brfr) = msg {
                                            queue.push_front(brfr);
                                            continue;
                                        }
                                    }
                                    match handle_write_executor_message(&mut executor.stream, &msg)
                                    {
                                        Ok(_) => {
                                            executor.queue_message(
                                                ExecutorStatusQuery,
                                                poll.registry(),
                                            )?;
                                        }
                                        Err(e) => {
                                            error!("{e}");
                                            todo!("Handle error");
                                        }
                                    };
                                }
                                ExecutorRegisterResponse { .. } => {
                                    match handle_write_executor_message(&mut executor.stream, &msg)
                                    {
                                        Ok(_) => {}
                                        Err(e) => {
                                            error!("{e}");
                                            todo!("Handle error");
                                        }
                                    };
                                }
                                ExecutorStatusQuery => {
                                    match handle_write_executor_message(&mut executor.stream, &msg)
                                    {
                                        Ok(_) => {
                                            executor
                                                .reregister(poll.registry(), Interest::READABLE)?;
                                        }
                                        Err(e) => {
                                            error!("{e}");
                                            todo!("Handle error");
                                        }
                                    };
                                }
                                ExecutorMessage::CloseConnection(_) => {
                                    match handle_write_executor_message(&mut executor.stream, &msg)
                                    {
                                        Ok(_) => {
                                            has_closed_connection = true;
                                        }
                                        Err(e) => {
                                            error!("{e}");
                                            todo!("Handle error");
                                        }
                                    };
                                }
                                _ => {
                                    panic!("Invalid message was asked to be sent: {msg:?}");
                                }
                            };
                        }
                        if event.is_readable() {
                            match handle_executor_read_event(&mut executor.stream) {
                                Ok(Some(msg)) => match msg {
                                    ExecutorMessage::JobResult(job_result) => {
                                        info!("Got back {:?}", job_result);
                                        warn!("For now, all JobResults are discarded");
                                        poll.registry().reregister(
                                            &mut executor.stream,
                                            token,
                                            Interest::READABLE,
                                        )?;
                                    }
                                    ExecutorMessage::ExecutorRegister => {
                                        if executor.status == YetToRegister {
                                            // then, schedule the response to be sent
                                            let id = gen_uuid();
                                            executor.queue_message(
                                                ExecutorRegisterResponse { id: id.clone() },
                                                poll.registry(),
                                            )?;
                                            executor.id = Some(id);
                                            executor.status = Available;
                                        }
                                    }
                                    ExecutorMessage::ExecutorStatusResponse(status) => {
                                        executor.status = status;
                                        executor.reregister(poll.registry(), Interest::READABLE)?;
                                    }
                                    ExecutorMessage::CloseConnection(_id) => {
                                        has_closed_connection = true;
                                    }
                                    _ => {
                                        todo!("Handle invalid messages received from executor");
                                    }
                                },
                                Ok(None) => {
                                    has_closed_connection = true;
                                }
                                Err(e) => {
                                    error!("{e}");
                                    todo!("Handle error");
                                }
                            }
                        }
                        // Here, handle the executor message event
                        //todo!("Handle executor message event");
                    } else {
                        warn!("Got sporadic event, should be safe to ignore…");
                    }
                    if has_closed_connection {
                        if let Some(mut connection) = incoming_connections.remove(&token) {
                            poll.registry().deregister(&mut connection)?;

                            info!("Closed incoming connection");
                        } else if let Some(mut executor) = executor_connections.remove(&token) {
                            poll.registry().deregister(&mut executor.stream)?;
                            info!("Removed executor {:?} from active connections", executor.id);
                        }
                    }
                }
            }
            thread::sleep(Duration::from_micros(1_000));
        }
        // Here we will attempt to unqueue the jobs and dispatch them

        for (_token, executor) in executor_connections.iter_mut() {
            if executor.pending_msgs.len() > 0 {
                executor.reregister(poll.registry(), Interest::WRITABLE)?;
            }
            if queue.len() > 0 && executor.status == Available {
                debug!("found available executor {:?}", executor.id);
                if let Some(brfr) = queue.pop_front() {
                    executor.queue_message(BuildRequest(brfr), poll.registry())?;
                }
            }
        }
    }
    Ok(())
}

#[instrument]
/// Returns Some(IncomingMessage) if we could read, None if connection is closed or whatever
fn handle_incoming_read_event(connection: &mut TcpStream) -> Result<Option<IncomingMessage>> {
    let mut received_data = vec![0; 4096];
    let mut bytes_read = 0;
    loop {
        match connection.read(&mut received_data[bytes_read..]) {
            Ok(0) => {
                // Reading 0 bytes means the other side has closed the
                // connection or is done writing, then so are we.
                break;
            }
            Ok(n) => {
                bytes_read += n;
                debug!("Read {bytes_read} bytes for now");
                if bytes_read == received_data.len() {
                    received_data.resize(received_data.len() * 2, 0);
                }
            }
            Err(e) if e.kind() == WouldBlock => {
                trace!("Got WouldBlock");
                break;
            }
            Err(e) if e.kind() == Interrupted => {
                trace!("Got WouldBlock");
                continue;
            }
            Err(e) => {
                return Err(Error::from(e));
            }
        }
    }
    if bytes_read != 0 {
        debug!("read {bytes_read} bytes, attempting construction");
        let received_data = &received_data[..bytes_read];
        if let Ok(msg) = serde_yaml::from_reader(received_data) {
            return Ok(Some(msg));
        }
        error!("Could not parse an incoming message from received data: {received_data:?}");
    }
    // if we haven't been able to parse anything, assume connection is dead
    return Ok(None);
}

#[instrument]
fn handle_executor_read_event(connection: &mut TcpStream) -> Result<Option<ExecutorMessage>> {
    let mut received_data = vec![0; 4096];
    let mut bytes_read = 0;
    loop {
        match connection.read(&mut received_data[bytes_read..]) {
            Ok(0) => {
                // Reading 0 bytes means the other side has closed the
                // connection or is done writing, then so are we.
                break;
            }
            Ok(n) => {
                bytes_read += n;
                if bytes_read == received_data.len() {
                    received_data.resize(received_data.len() * 2, 0);
                }
            }
            Err(e) if e.kind() == WouldBlock => {
                break;
            }
            Err(e) if e.kind() == Interrupted => {
                continue;
            }
            Err(e) => {
                return Err(Error::from(e));
            }
        }
    }
    if bytes_read != 0 {
        let received_data = &received_data[..bytes_read];
        match ExecutorMessage::from(received_data) {
            Ok(msg) => {
                trace!("Got message from executor: {msg:?}");
                return Ok(Some(msg));
            }
            Err(err) => {
                error!("{err:?} while parsing an incoming message from received data: {received_data:?}");
                return Err(Error::from(err));
            }
        }
    }
    // If here we don't have anything to send back, let's assume the connection is closed
    return Ok(None);
}

#[instrument]
fn handle_write_executor_message(stream: &mut TcpStream, msg: &ExecutorMessage) -> Result<()> {
    debug!("We'll write {msg:?} to executor");
    let mut buffer = Vec::new();
    msg.write(&mut buffer)?;
    match stream.write(&buffer) {
        Ok(n) if n < buffer.len() => return Err(Error::from(io::Error::from(WriteZero))),
        Ok(_) => {
            // we have successfully wrote
            // TODO: have a next_message: Some(ExecutorMessage) that we would check in the mainloop to register the socket as writable
            //trace!("Reregistering executor as READABLE…");
        }
        Err(e) if e.kind() == WouldBlock => {
            // stream not really ready to perform I/O
        }
        Err(e) if e.kind() == Interrupted => {
            todo!("we should re-attempt to send it");
        }
        Err(e) => return Err(Error::from(e)),
    }
    Ok(())
}
