use std::{
    collections::{BTreeMap, VecDeque},
    ffi::{OsStr, OsString},
    os::fd::OwnedFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::workspace_authority::WorkspaceAuthority;

use super::{
    config::LspServerConfig,
    framing::{FramingError, MessageDecoder, encode_message},
    protocol::{
        LspOperation, LspPosition, LspResult, ProtocolError, normalize_result,
        validate_capabilities,
    },
};
use crate::tools::process::{
    PluginCleanup, PluginEmergencyHandle, PluginIo, PluginLeaderState, PluginProcess,
    PluginProcessError, ProcessRunner,
};

const QUEUE_CAPACITY: usize = 2;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CANCEL_GRACE: Duration = Duration::from_millis(500);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const IO_CHUNK_BYTES: usize = 8 * 1024;
const MAX_CHILD_ENVIRONMENT_ENTRIES: usize = 24;
const MAX_CHILD_ENVIRONMENT_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LspStop {
    Cancelled,
    Timeout,
}

#[derive(Debug)]
pub(super) enum LspActorOutcome {
    Success(LspResult),
    Unsupported,
    MalformedResponse,
    Protocol,
    Process,
    Busy,
    Stopped(LspStop),
}

pub(super) struct LspActorQuery {
    pub(super) operation: LspOperation,
    pub(super) position: LspPosition,
    pub(super) language_id: String,
    pub(super) uri: String,
    pub(super) text: String,
    pub(super) cancellation: CancellationToken,
    pub(super) deadline: Instant,
}

pub(super) struct LspActor {
    sender: SyncSender<ActorCommand>,
    shutdown: Arc<AtomicBool>,
    actor_thread: Thread,
    join: Mutex<Option<JoinHandle<ActorExit>>>,
    completion: watch::Receiver<Option<ActorExit>>,
    emergency: Arc<Mutex<Option<PluginEmergencyHandle>>>,
}

impl std::fmt::Debug for LspActor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspActor")
            .field("shutdown", &self.shutdown.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl LspActor {
    pub(super) fn start(
        config: LspServerConfig,
        runner: ProcessRunner,
        authority: WorkspaceAuthority,
        base_environment: &[(OsString, OsString)],
    ) -> Result<Arc<Self>, ()> {
        let environment = merge_environment(base_environment, config.environment())?;
        let id = config.id().to_owned();
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let emergency = Arc::new(Mutex::new(None));
        let (completion_sender, completion) = watch::channel(None);
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_emergency = Arc::clone(&emergency);
        let join = thread::Builder::new()
            .name(format!("dsh-lsp-{id}"))
            .spawn(move || {
                let exit = run_actor(
                    config,
                    runner,
                    authority,
                    environment,
                    receiver,
                    thread_shutdown,
                    thread_emergency,
                );
                completion_sender.send_replace(Some(exit));
                exit
            })
            .map_err(|_| ())?;
        let actor_thread = join.thread().clone();
        Ok(Arc::new(Self {
            sender,
            shutdown,
            actor_thread,
            join: Mutex::new(Some(join)),
            completion,
            emergency,
        }))
    }

    pub(super) async fn query(&self, query: LspActorQuery) -> LspActorOutcome {
        if self.shutdown.load(Ordering::Acquire) {
            return LspActorOutcome::Process;
        }
        let (response, receive_response) = oneshot::channel();
        match self.sender.try_send(ActorCommand { query, response }) {
            Ok(()) => self.actor_thread.unpark(),
            Err(TrySendError::Full(_)) => return LspActorOutcome::Busy,
            Err(TrySendError::Disconnected(_)) => return LspActorOutcome::Process,
        }
        receive_response.await.unwrap_or(LspActorOutcome::Process)
    }

    pub(super) fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.actor_thread.unpark();
    }

    pub(super) async fn shutdown(&self) -> bool {
        self.request_shutdown();
        let mut completion = self.completion.clone();
        loop {
            if let Some(exit) = *completion.borrow() {
                let join = self
                    .join
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                let joined =
                    join.map_or(exit, |join| join.join().unwrap_or(ActorExit::OwnershipLost));
                return joined != ActorExit::OwnershipLost;
            }
            if completion.changed().await.is_err() {
                return false;
            }
        }
    }

    pub(super) fn shutdown_blocking(&self) -> bool {
        self.request_shutdown();
        let join = self
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        join.is_none_or(|join| {
            join.join().unwrap_or(ActorExit::OwnershipLost) != ActorExit::OwnershipLost
        })
    }

    fn emergency_shutdown(&self) {
        self.request_shutdown();
        if let Some(handle) = self
            .emergency
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            handle.kill_group();
        }
    }
}

impl Drop for LspActor {
    fn drop(&mut self) {
        self.emergency_shutdown();
    }
}

struct ActorCommand {
    query: LspActorQuery,
    response: oneshot::Sender<LspActorOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActorExit {
    Clean,
    OwnershipLost,
}

fn run_actor(
    config: LspServerConfig,
    runner: ProcessRunner,
    authority: WorkspaceAuthority,
    environment: Vec<(OsString, OsString)>,
    receiver: mpsc::Receiver<ActorCommand>,
    shutdown: Arc<AtomicBool>,
    emergency: Arc<Mutex<Option<PluginEmergencyHandle>>>,
) -> ActorExit {
    let mut connection = None;
    loop {
        if shutdown.load(Ordering::Acquire) {
            reject_queued(&receiver);
            return close_connection(connection, &emergency);
        }
        match receiver.try_recv() {
            Ok(command) => {
                let outcome = run_query(
                    &config,
                    &runner,
                    &authority,
                    &environment,
                    &shutdown,
                    &emergency,
                    &mut connection,
                    &command.query,
                );
                let _ = command.response.send(outcome);
            }
            Err(TryRecvError::Empty) => {
                if let Some(active) = connection.as_mut() {
                    if active.pump_idle().is_err() {
                        let owned = terminate_connection(connection.take(), &emergency);
                        if !owned {
                            reject_queued(&receiver);
                            return ActorExit::OwnershipLost;
                        }
                    }
                }
                thread::park_timeout(POLL_INTERVAL);
            }
            Err(TryRecvError::Disconnected) => return close_connection(connection, &emergency),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_query(
    config: &LspServerConfig,
    runner: &ProcessRunner,
    authority: &WorkspaceAuthority,
    environment: &[(OsString, OsString)],
    shutdown: &AtomicBool,
    emergency: &Mutex<Option<PluginEmergencyHandle>>,
    connection: &mut Option<LspConnection>,
    query: &LspActorQuery,
) -> LspActorOutcome {
    let control = DriveControl {
        cancellation: Some(&query.cancellation),
        shutdown: Some(shutdown),
        deadline: query.deadline,
    };
    if let Some(stop) = control.stop() {
        return LspActorOutcome::Stopped(stop);
    }
    for attempt in 0..2 {
        if connection.is_none() {
            match LspConnection::start(config, runner, authority, environment, &control) {
                Ok(started) => {
                    *emergency.lock().unwrap_or_else(|error| error.into_inner()) =
                        Some(started.process.emergency_handle());
                    *connection = Some(started);
                }
                Err(ConnectionFailure::Stopped(stop)) => {
                    return LspActorOutcome::Stopped(stop);
                }
                Err(ConnectionFailure::OwnershipLost) => return LspActorOutcome::Process,
                Err(ConnectionFailure::Transport | ConnectionFailure::Protocol) if attempt == 0 => {
                    continue;
                }
                Err(_) => return LspActorOutcome::Process,
            }
        }
        let outcome = connection
            .as_mut()
            .map_or(Err(ConnectionFailure::Transport), |connection| {
                connection.query(query, &control)
            });
        match outcome {
            Ok(result) => return LspActorOutcome::Success(result),
            Err(ConnectionFailure::Unsupported) => return LspActorOutcome::Unsupported,
            Err(ConnectionFailure::MalformedResponse) => {
                return LspActorOutcome::MalformedResponse;
            }
            Err(ConnectionFailure::Server) => return LspActorOutcome::Protocol,
            Err(ConnectionFailure::Stopped(stop)) => {
                let _ = terminate_connection(connection.take(), emergency);
                return LspActorOutcome::Stopped(stop);
            }
            Err(ConnectionFailure::OwnershipLost) => {
                let _ = connection.take();
                clear_emergency(emergency);
                return LspActorOutcome::Process;
            }
            Err(ConnectionFailure::Transport | ConnectionFailure::Protocol) => {
                let owned = terminate_connection(connection.take(), emergency);
                if !owned {
                    return LspActorOutcome::Process;
                }
                if attempt == 1 {
                    return LspActorOutcome::Process;
                }
            }
        }
    }
    LspActorOutcome::Process
}

fn reject_queued(receiver: &mpsc::Receiver<ActorCommand>) {
    while let Ok(command) = receiver.try_recv() {
        let _ = command
            .response
            .send(LspActorOutcome::Stopped(LspStop::Cancelled));
    }
}

fn close_connection(
    connection: Option<LspConnection>,
    emergency: &Mutex<Option<PluginEmergencyHandle>>,
) -> ActorExit {
    let result = connection.is_none_or(LspConnection::shutdown);
    clear_emergency(emergency);
    if result {
        ActorExit::Clean
    } else {
        ActorExit::OwnershipLost
    }
}

fn terminate_connection(
    connection: Option<LspConnection>,
    emergency: &Mutex<Option<PluginEmergencyHandle>>,
) -> bool {
    let result = connection.is_none_or(LspConnection::terminate);
    clear_emergency(emergency);
    result
}

fn clear_emergency(emergency: &Mutex<Option<PluginEmergencyHandle>>) {
    *emergency.lock().unwrap_or_else(|error| error.into_inner()) = None;
}

struct DriveControl<'a> {
    cancellation: Option<&'a CancellationToken>,
    shutdown: Option<&'a AtomicBool>,
    deadline: Instant,
}

impl DriveControl<'_> {
    fn stop(&self) -> Option<LspStop> {
        if self
            .shutdown
            .is_some_and(|shutdown| shutdown.load(Ordering::Acquire))
            || self
                .cancellation
                .is_some_and(CancellationToken::is_cancelled)
        {
            Some(LspStop::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(LspStop::Timeout)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionFailure {
    Transport,
    Protocol,
    Server,
    Unsupported,
    MalformedResponse,
    Stopped(LspStop),
    OwnershipLost,
}

struct LspConnection {
    process: PluginProcess,
    decoder: MessageDecoder,
    inbox: VecDeque<Value>,
    next_id: u64,
    capabilities: Value,
    configuration: Value,
}

impl LspConnection {
    fn start(
        config: &LspServerConfig,
        runner: &ProcessRunner,
        authority: &WorkspaceAuthority,
        environment: &[(OsString, OsString)],
        control: &DriveControl<'_>,
    ) -> Result<Self, ConnectionFailure> {
        if let Some(stop) = control.stop() {
            return Err(ConnectionFailure::Stopped(stop));
        }
        config
            .program()
            .revalidate()
            .map_err(|_| ConnectionFailure::Transport)?;
        let workdir = clone_workdir(authority).map_err(|_| ConnectionFailure::Transport)?;
        let cancellation = control
            .cancellation
            .cloned()
            .unwrap_or_else(CancellationToken::new);
        let process = PluginProcess::spawn(
            runner,
            config.program().path(),
            config.program().arguments(),
            workdir,
            environment,
            &cancellation,
        )
        .map_err(map_process_start)?;
        let mut connection = Self {
            process,
            decoder: MessageDecoder::default(),
            inbox: VecDeque::new(),
            next_id: 1,
            capabilities: Value::Null,
            configuration: config.configuration().as_value().clone(),
        };
        let Some(workspace_uri) = super::render::file_uri(authority.canonical_path()) else {
            let owned = connection.terminate();
            return Err(if owned {
                ConnectionFailure::Protocol
            } else {
                ConnectionFailure::OwnershipLost
            });
        };
        let initialize = json!({
            "processId": null,
            "rootUri": workspace_uri,
            "workspaceFolders": [{"uri": workspace_uri, "name": "workspace"}],
            "capabilities": {
                "general": {"positionEncodings": ["utf-16"]},
                "workspace": {"workspaceFolders": true, "configuration": true},
                "textDocument": {
                    "synchronization": {"dynamicRegistration": false},
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {"linkSupport": true},
                    "implementation": {"linkSupport": true},
                    "references": {}
                }
            },
            "initializationOptions": config.initialization_options().as_value()
        });
        let response = match connection.request("initialize", initialize, control) {
            Ok(response) => response,
            Err(error) => {
                let owned = connection.terminate();
                return Err(if owned {
                    error
                } else {
                    ConnectionFailure::OwnershipLost
                });
            }
        };
        connection.capabilities = response;
        if let Err(error) = connection.notify("initialized", json!({}), control) {
            let owned = connection.terminate();
            return Err(if owned {
                error
            } else {
                ConnectionFailure::OwnershipLost
            });
        }
        Ok(connection)
    }

    fn query(
        &mut self,
        query: &LspActorQuery,
        control: &DriveControl<'_>,
    ) -> Result<LspResult, ConnectionFailure> {
        validate_capabilities(&self.capabilities, query.operation).map_err(map_protocol_error)?;
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": query.uri,
                "languageId": query.language_id,
                "version": 1,
                "text": query.text
            }}),
            control,
        )?;
        let mut params = json!({
            "textDocument": {"uri": query.uri},
            "position": {"line": query.position.line, "character": query.position.character}
        });
        if query.operation == LspOperation::FindReferences {
            params["context"] = json!({"includeDeclaration": true});
        }
        let result = self.request(query.operation.method(), params, control);
        if !matches!(
            result,
            Err(ConnectionFailure::Transport
                | ConnectionFailure::OwnershipLost
                | ConnectionFailure::Stopped(_))
        ) {
            let _ = self.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": query.uri}}),
                &DriveControl {
                    cancellation: None,
                    shutdown: None,
                    deadline: Instant::now() + CANCEL_GRACE,
                },
            );
        }
        let result = result?;
        normalize_result(query.operation, &result).map_err(map_protocol_error)
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        control: &DriveControl<'_>,
    ) -> Result<Value, ConnectionFailure> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ConnectionFailure::Protocol)?;
        self.write(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            control,
        )?;
        self.wait_response(id, control)
    }

    fn notify(
        &mut self,
        method: &str,
        params: Value,
        control: &DriveControl<'_>,
    ) -> Result<(), ConnectionFailure> {
        self.write(
            &json!({"jsonrpc":"2.0","method":method,"params":params}),
            control,
        )
    }

    fn write(
        &mut self,
        value: &Value,
        control: &DriveControl<'_>,
    ) -> Result<(), ConnectionFailure> {
        let encoded = encode_message(value).map_err(|_| ConnectionFailure::Protocol)?;
        let mut offset = 0_usize;
        while offset < encoded.len() {
            if let Some(stop) = control.stop() {
                return Err(ConnectionFailure::Stopped(stop));
            }
            match self.process.try_write(&encoded[offset..]) {
                Ok(PluginIo::Bytes(count)) => offset = offset.saturating_add(count),
                Ok(PluginIo::WouldBlock) => {
                    self.pump()?;
                    thread::park_timeout(POLL_INTERVAL);
                }
                Ok(PluginIo::Eof | PluginIo::LimitExceeded) | Err(_) => {
                    return Err(ConnectionFailure::Transport);
                }
            }
        }
        Ok(())
    }

    fn wait_response(
        &mut self,
        id: u64,
        control: &DriveControl<'_>,
    ) -> Result<Value, ConnectionFailure> {
        loop {
            if let Some(response) = self.take_response(id)? {
                return response;
            }
            if let Some(stop) = control.stop() {
                let cancel = json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}});
                let grace = DriveControl {
                    cancellation: None,
                    shutdown: None,
                    deadline: Instant::now() + CANCEL_GRACE,
                };
                let _ = self.write(&cancel, &grace);
                while grace.stop().is_none() {
                    if self.take_response(id)?.is_some() {
                        return Err(ConnectionFailure::Stopped(stop));
                    }
                    self.pump()?;
                    thread::park_timeout(POLL_INTERVAL);
                }
                return Err(ConnectionFailure::Stopped(stop));
            }
            self.pump()?;
            thread::park_timeout(POLL_INTERVAL);
        }
    }

    fn take_response(
        &mut self,
        expected_id: u64,
    ) -> Result<Option<Result<Value, ConnectionFailure>>, ConnectionFailure> {
        while let Some(message) = self.inbox.pop_front() {
            let Some(fields) = message.as_object() else {
                continue;
            };
            if let (Some(method), Some(id)) = (
                fields.get("method").and_then(Value::as_str),
                fields.get("id"),
            ) {
                self.answer_server_request(id.clone(), method, fields.get("params").cloned())?;
                continue;
            }
            let Some(id) = fields.get("id").and_then(Value::as_u64) else {
                continue;
            };
            if id != expected_id {
                continue;
            }
            if fields.get("error").is_some_and(|value| !value.is_null()) {
                return Ok(Some(Err(ConnectionFailure::Server)));
            }
            let result = fields
                .get("result")
                .cloned()
                .ok_or(ConnectionFailure::Protocol)?;
            return Ok(Some(Ok(result)));
        }
        Ok(None)
    }

    fn answer_server_request(
        &mut self,
        id: Value,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), ConnectionFailure> {
        let response = if method == "workspace/configuration" {
            let count = params
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|fields| fields.get("items"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            json!({"jsonrpc":"2.0","id":id,"result":vec![self.configuration.clone(); count]})
        } else if matches!(
            method,
            "window/workDoneProgress/create"
                | "client/registerCapability"
                | "client/unregisterCapability"
        ) {
            json!({"jsonrpc":"2.0","id":id,"result":null})
        } else {
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not supported"}})
        };
        self.write(
            &response,
            &DriveControl {
                cancellation: None,
                shutdown: None,
                deadline: Instant::now() + CANCEL_GRACE,
            },
        )
    }

    fn pump(&mut self) -> Result<(), ConnectionFailure> {
        let mut scratch = [0_u8; IO_CHUNK_BYTES];
        loop {
            match self.process.try_read_stdout(&mut scratch) {
                Ok(PluginIo::Bytes(count)) => {
                    let messages = self
                        .decoder
                        .push(&scratch[..count])
                        .map_err(map_framing_error)?;
                    self.inbox.extend(messages);
                }
                Ok(PluginIo::WouldBlock) => break,
                Ok(PluginIo::Eof) => return Err(ConnectionFailure::Transport),
                Ok(PluginIo::LimitExceeded) | Err(_) => return Err(ConnectionFailure::Transport),
            }
        }
        loop {
            match self.process.try_read_stderr(&mut scratch) {
                Ok(PluginIo::Bytes(_)) => {}
                Ok(PluginIo::WouldBlock | PluginIo::Eof) => break,
                Ok(PluginIo::LimitExceeded) | Err(_) => return Err(ConnectionFailure::Transport),
            }
        }
        match self.process.leader_state() {
            PluginLeaderState::Running => Ok(()),
            PluginLeaderState::Exited(_) if !self.inbox.is_empty() => Ok(()),
            PluginLeaderState::Exited(_) => Err(ConnectionFailure::Transport),
            PluginLeaderState::OwnershipLost => Err(ConnectionFailure::OwnershipLost),
        }
    }

    fn pump_idle(&mut self) -> Result<(), ConnectionFailure> {
        self.pump()?;
        while let Some(message) = self.inbox.pop_front() {
            let Some(fields) = message.as_object() else {
                continue;
            };
            if let (Some(method), Some(id)) = (
                fields.get("method").and_then(Value::as_str),
                fields.get("id"),
            ) {
                self.answer_server_request(id.clone(), method, fields.get("params").cloned())?;
            }
        }
        Ok(())
    }

    fn shutdown(mut self) -> bool {
        let control = DriveControl {
            cancellation: None,
            shutdown: None,
            deadline: Instant::now() + SHUTDOWN_TIMEOUT,
        };
        if self.request("shutdown", Value::Null, &control).is_ok() {
            let _ = self.notify("exit", Value::Null, &control);
        }
        matches!(self.process.cleanup().state(), PluginCleanup::Quiescent(_))
    }

    fn terminate(self) -> bool {
        matches!(
            self.process.terminate().state(),
            PluginCleanup::Quiescent(_)
        )
    }
}

fn clone_workdir(authority: &WorkspaceAuthority) -> Result<OwnedFd, ()> {
    authority
        .root()
        .try_clone()
        .map(|directory| OwnedFd::from(directory.into_std_file()))
        .map_err(|_| ())
}

fn map_process_start(error: PluginProcessError) -> ConnectionFailure {
    match error {
        PluginProcessError::OwnershipLost => ConnectionFailure::OwnershipLost,
        PluginProcessError::ObserverUnavailable
        | PluginProcessError::Cancelled
        | PluginProcessError::Spawn
        | PluginProcessError::Pipes => ConnectionFailure::Transport,
    }
}

fn map_framing_error(error: FramingError) -> ConnectionFailure {
    match error {
        FramingError::Capacity
        | FramingError::Header
        | FramingError::MessageTooLarge
        | FramingError::Json => ConnectionFailure::Protocol,
    }
}

fn map_protocol_error(error: ProtocolError) -> ConnectionFailure {
    match error {
        ProtocolError::UnsupportedOperation
        | ProtocolError::UnsupportedEncoding
        | ProtocolError::UnsupportedSynchronization => ConnectionFailure::Unsupported,
        ProtocolError::MalformedResponse => ConnectionFailure::MalformedResponse,
    }
}

fn merge_environment(
    base: &[(OsString, OsString)],
    overrides: &[(OsString, OsString)],
) -> Result<Vec<(OsString, OsString)>, ()> {
    let mut values = BTreeMap::<OsString, OsString>::new();
    for (name, value) in base.iter().chain(overrides) {
        values.insert(name.clone(), value.clone());
    }
    if values.len() > MAX_CHILD_ENVIRONMENT_ENTRIES {
        return Err(());
    }
    let mut bytes = 0_usize;
    for (name, value) in &values {
        bytes = bytes
            .checked_add(os_bytes(name))
            .and_then(|total| total.checked_add(os_bytes(value)))
            .ok_or(())?;
        if bytes > MAX_CHILD_ENVIRONMENT_BYTES {
            return Err(());
        }
    }
    Ok(values.into_iter().collect())
}

fn os_bytes(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().len()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::merge_environment;

    #[test]
    fn environment_overrides_replace_without_exposing_values() {
        let merged = merge_environment(
            &[(OsString::from("PATH"), OsString::from("base"))],
            &[(OsString::from("PATH"), OsString::from("override"))],
        )
        .unwrap();
        assert_eq!(
            merged,
            [(OsString::from("PATH"), OsString::from("override"))]
        );
    }
}
