//! Signed-in dashboard sessions on the core.
//!
//! A scoped dashboard session chats through the core's session methods.
//! Each chat socket opens an RPC connection of its own through the daemon's
//! in-process connector, presenting the token the session signed in with,
//! so the core sees that principal, applies that principal's grants, and
//! keeps the conversation under that principal. The gateway translates the
//! dashboard's routes and decides nothing the core does not; where it
//! narrows (the agent list), it only removes. An administrator keeps the
//! in-process chat and its history.
//!
//! Each socket is one conversation (`CoreChat`): created when the socket
//! opens, and closed when it closes, when the session ends (sign-out,
//! expiry, revocation) or with the daemon generation. No other socket
//! reopens it, so nothing about a conversation is shared between sockets:
//! not its connection, not the TUI identity the core holds it under, not
//! its approvals. A dashboard session may have `MAX_LIVE_CONVERSATIONS`
//! chats open at once (`CoreBinding`).
//!
//! A conversation the core finishes creating long after its chat gave up is
//! closed when the core's answer lands. What a chat cannot close (its token
//! no longer verifies) is left to the core, which retires a conversation
//! whose client has stayed away for `wss.orphaned_session_timeout_secs`. A
//! chat's identity is never presented again once the chat is over, so its
//! conversation is such an orphan.
//!
//! Callers without a dashboard session, and every request when this gateway
//! does not run inside the daemon, are served exactly as before.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::jsonrpc::error_codes::{
    AUTH_REQUIRED, FORBIDDEN, SESSION_NOT_FOUND, SESSION_NOT_OWNED,
};
use zeroclaw_rpc_client::{ClientError, ConnectOptions, ConnectionState, Method, RpcClient};
use zeroclaw_rpc_proto::types::{AgentsListResult, SessionCancelResult, SessionNewResult};
use zeroclaw_runtime::rpc::inproc::Served;

use crate::AppState;
use crate::core_rpc::CoreRpc;
use crate::dashboard_session::{DashboardAuth, DashboardCaller, SessionAuth};

/// How many chats, each with its conversation on the core, a dashboard
/// session may have open at once. The core's live session map is small and
/// shared by every client.
pub(crate) const MAX_LIVE_CONVERSATIONS: usize = 4;

/// How many conversations one account may hold on the core in a generation,
/// across all its sign-ins: as many as its sign-ins may hold together.
pub(crate) const ACCOUNT_CONVERSATIONS: usize =
    crate::dashboard_session::MAX_SESSIONS_PER_PRINCIPAL * MAX_LIVE_CONVERSATIONS;

/// How many core connections one account's other requests (the agent list,
/// an abort that must reconnect) may hold at once.
pub(crate) const AUXILIARY_CONNECTIONS_PER_ACCOUNT: usize = 2;

/// How many core connections all accounts' other requests may hold at once.
const AUXILIARY_CONNECTIONS: usize = 16;

/// How long before a dashboard session's token expires its chats end. Once
/// the token has expired the core no longer takes a chat's calls, so the
/// close cannot wait for the session to be swept.
pub(crate) const CLOSE_BEFORE_EXPIRY: Duration = Duration::from_secs(10);

/// How long a chat waits for the core to create its conversation. The core
/// may first wait out each MCP server's start-up, longer than a request's
/// usual deadline. Past this the chat gives up, but the creation's task keeps
/// listening for the core's answer: what the core still creates is closed
/// when it lands, and the account's charge for it is settled then.
const CREATE_CEILING: Duration = Duration::from_secs(10 * 60);

/// How long one close may take: long enough for a turn the close cancels to
/// finish, since the core takes the conversation's permit before it removes
/// it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

/// How many times a chat tries to close its conversation, each after the
/// first on a new connection.
const CLOSE_ATTEMPTS: u32 = 3;

pub(crate) fn conversation_limit_message() -> String {
    format!(
        "This sign-in already has {MAX_LIVE_CONVERSATIONS} conversations open; \
         close one to start another"
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The chats a dashboard session has open. It belongs to the dashboard
/// session, which outlives each chat and each daemon generation.
#[derive(Default)]
pub(crate) struct CoreBinding {
    held: parking_lot::Mutex<Chats>,
    /// The core's last answer to the session's agent list.
    agents: parking_lot::Mutex<Option<KeptAgents>>,
}

/// The core's answer to a session's agent list, when it was asked, and under
/// which policy generation.
struct KeptAgents {
    at: std::time::Instant,
    generation: u64,
    list: AgentList,
}

/// How long the core's answer to a session's agent list is kept, so the list
/// does not cost the identity provider a verification on every request.
const AGENTS_KEPT_FOR: Duration = Duration::from_secs(30);

/// What the core said about the agents a session may see. The account's
/// grants are applied on every request, to a kept answer as to a fresh one.
#[derive(Clone)]
enum AgentList {
    /// The core's agents, by alias, and whether each is enabled.
    Listed(Vec<(String, bool)>),
    /// The account may not read the agent list: it is told only the agents
    /// it was granted by name.
    Forbidden,
}

#[derive(Default)]
struct Chats {
    /// Places taken: open chats, and chats still opening.
    taken: usize,
    /// Each open chat, by its conversation's core id.
    open: HashMap<String, OpenChat>,
}

/// What the session's other routes may know of an open chat.
struct OpenChat {
    agent_alias: String,
    created_at: String,
    /// The connection the chat calls the core on now.
    client: Arc<RpcClient>,
    identity: TuiIdentity,
}

/// The TUI identity the core assigned at a chat's first handshake.
#[derive(Clone)]
struct TuiIdentity {
    id: String,
    /// The core's signature over `id`. Secret: never logged or shown.
    sig: String,
}

/// One of a dashboard session's [`MAX_LIVE_CONVERSATIONS`] places, and one
/// of its account's [`ACCOUNT_CONVERSATIONS`], held from the moment a chat
/// starts opening until its conversation is closed. The open and its
/// creation share it, so an open that goes away (a request timeout, say)
/// leaves the place taken while the creation is still under way. A creation
/// the chat stopped waiting for lets go of the session's place but keeps the
/// account's share until the core's answer settles it.
type Place = Arc<PlaceGuard>;

struct PlaceGuard {
    binding: Arc<CoreBinding>,
    core_id: parking_lot::Mutex<Option<String>>,
    account: parking_lot::Mutex<Option<AccountPlace>>,
}

impl PlaceGuard {
    fn hold(&self, core_id: &str, chat: OpenChat) {
        self.binding
            .held
            .lock()
            .open
            .insert(core_id.to_string(), chat);
        *self.core_id.lock() = Some(core_id.to_string());
    }

    /// The conversation may still be live after a failed close: its account
    /// stays charged for it for the rest of the generation.
    fn keep_charged(&self) {
        if let Some(account) = self.take_account() {
            account.keep();
        }
    }

    /// The account's share, taken out of the place: the session's place is
    /// let go of with the guard, the share only when what took it is.
    fn take_account(&self) -> Option<AccountPlace> {
        self.account.lock().take()
    }
}

impl Drop for PlaceGuard {
    fn drop(&mut self) {
        let mut held = self.binding.held.lock();
        held.taken = held.taken.saturating_sub(1);
        if let Some(core_id) = self.core_id.lock().as_ref() {
            held.open.remove(core_id);
        }
    }
}

/// What each account holds on the core in one generation, across all its
/// sign-ins: a place for every chat that is opening, open or not yet closed,
/// and the connections its other requests hold. Ending a sign-in, by
/// sign-out or eviction, frees nothing here; only letting go of what it held
/// does. A creation the chat stopped waiting for stays charged until the
/// core answers it. A conversation whose close failed, or whose creation's
/// answer never came (a lost connection), stays charged to its account for
/// the rest of the generation, since nothing tells the gateway when the core
/// lets it go. The generation's end drains them all.
#[derive(Clone, Default)]
pub(crate) struct Accounts {
    held: Arc<parking_lot::Mutex<HashMap<String, AccountHeld>>>,
    /// The connections all accounts' other requests may hold at once.
    auxiliary: Arc<AuxiliaryBound>,
    /// A creation ceiling shorter than [`CREATE_CEILING`], for tests.
    #[cfg(test)]
    create_ceiling: Arc<parking_lot::Mutex<Option<Duration>>>,
}

struct AuxiliaryBound(Arc<tokio::sync::Semaphore>);

impl Default for AuxiliaryBound {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::Semaphore::new(AUXILIARY_CONNECTIONS)))
    }
}

/// An account's entry, removed once it holds nothing: no conversation
/// charged, and no request holding or waiting for one of its connections.
struct AccountHeld {
    conversations: usize,
    auxiliary_users: usize,
    auxiliary: Arc<tokio::sync::Semaphore>,
}

impl AccountHeld {
    fn new() -> Self {
        Self {
            conversations: 0,
            auxiliary_users: 0,
            auxiliary: Arc::new(tokio::sync::Semaphore::new(
                AUXILIARY_CONNECTIONS_PER_ACCOUNT,
            )),
        }
    }
}

/// One of the connections an account's other requests may hold. Each core
/// connection the request opens keeps it until the core is done with that
/// connection, not only until the request is: a request that goes away, or a
/// handshake the gateway stops waiting for, may leave the core still
/// verifying the token.
pub(crate) struct Auxiliary {
    _account: tokio::sync::OwnedSemaphorePermit,
    _all: tokio::sync::OwnedSemaphorePermit,
    _user: AuxiliaryUser,
}

/// A request holding or waiting for one of an account's connections: its
/// entry stays while there is one, so every request of the account shares
/// one semaphore.
struct AuxiliaryUser {
    accounts: Accounts,
    principal: String,
}

impl Drop for AuxiliaryUser {
    fn drop(&mut self) {
        let mut held = self.accounts.held.lock();
        if let Some(account) = held.get_mut(&self.principal) {
            account.auxiliary_users = account.auxiliary_users.saturating_sub(1);
        }
        Accounts::reclaim(&mut held, &self.principal);
    }
}

/// One of an account's [`ACCOUNT_CONVERSATIONS`].
struct AccountPlace {
    accounts: Accounts,
    principal: String,
    kept: bool,
}

impl AccountPlace {
    /// Never given back in this generation.
    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for AccountPlace {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let mut held = self.accounts.held.lock();
        if let Some(account) = held.get_mut(&self.principal) {
            account.conversations = account.conversations.saturating_sub(1);
        }
        Accounts::reclaim(&mut held, &self.principal);
    }
}

impl Accounts {
    /// How long a chat waits for the core to create its conversation.
    fn create_ceiling(&self) -> Duration {
        #[cfg(test)]
        if let Some(ceiling) = *self.create_ceiling.lock() {
            return ceiling;
        }
        CREATE_CEILING
    }

    #[cfg(test)]
    pub(crate) fn set_create_ceiling(&self, ceiling: Duration) {
        *self.create_ceiling.lock() = Some(ceiling);
    }

    /// Remove `principal`'s entry if it holds nothing.
    fn reclaim(held: &mut HashMap<String, AccountHeld>, principal: &str) {
        if held
            .get(principal)
            .is_some_and(|account| account.conversations == 0 && account.auxiliary_users == 0)
        {
            held.remove(principal);
        }
    }

    fn take(&self, principal: &str) -> Result<AccountPlace, CoreFailure> {
        let mut held = self.held.lock();
        let account = held
            .entry(principal.to_string())
            .or_insert_with(AccountHeld::new);
        if account.conversations >= ACCOUNT_CONVERSATIONS {
            return Err(CoreFailure::ConversationLimit);
        }
        account.conversations += 1;
        Ok(AccountPlace {
            accounts: self.clone(),
            principal: principal.to_string(),
            kept: false,
        })
    }

    /// One of the connections `principal`'s other requests may hold, and
    /// one of all accounts', waited for while they are all in use.
    async fn auxiliary(&self, principal: &str) -> Result<Auxiliary, CoreFailure> {
        let account = {
            let mut held = self.held.lock();
            let account = held
                .entry(principal.to_string())
                .or_insert_with(AccountHeld::new);
            account.auxiliary_users += 1;
            Arc::clone(&account.auxiliary)
        };
        let user = AuxiliaryUser {
            accounts: self.clone(),
            principal: principal.to_string(),
        };
        let account = account
            .acquire_owned()
            .await
            .map_err(|_| CoreFailure::Unavailable)?;
        let all = Arc::clone(&self.auxiliary.0)
            .acquire_owned()
            .await
            .map_err(|_| CoreFailure::Unavailable)?;
        Ok(Auxiliary {
            _account: account,
            _all: all,
            _user: user,
        })
    }
}

impl CoreBinding {
    fn take_place(self: &Arc<Self>, account: AccountPlace) -> Result<Place, CoreFailure> {
        let mut held = self.held.lock();
        if held.taken >= MAX_LIVE_CONVERSATIONS {
            return Err(CoreFailure::ConversationLimit);
        }
        held.taken += 1;
        Ok(Arc::new(PlaceGuard {
            binding: Arc::clone(self),
            core_id: parking_lot::Mutex::new(None),
            account: parking_lot::Mutex::new(Some(account)),
        }))
    }

    /// The core's answer to the session's agent list, while it is fresh and
    /// the policy it was given under is still the one in force.
    fn kept_agents(&self, generation: u64) -> Option<AgentList> {
        self.agents
            .lock()
            .as_ref()
            .filter(|kept| kept.generation == generation && kept.at.elapsed() < AGENTS_KEPT_FOR)
            .map(|kept| kept.list.clone())
    }

    /// Keep `list`, the answer to a request made under `generation`: a policy
    /// published while it was asked for makes it stale at once.
    fn keep_agents(&self, generation: u64, list: AgentList) {
        *self.agents.lock() = Some(KeptAgents {
            at: std::time::Instant::now(),
            generation,
            list,
        });
    }

    /// The chat on `core_id` now calls the core on `client`.
    fn moved(&self, core_id: &str, client: &Arc<RpcClient>) {
        if let Some(chat) = self.held.lock().open.get_mut(core_id) {
            chat.client = Arc::clone(client);
        }
    }

    /// The open chats, for the session list.
    fn listed(&self) -> Vec<Value> {
        let held = self.held.lock();
        let mut rows: Vec<Value> = held
            .open
            .iter()
            .map(|(core_id, chat)| {
                json!({
                    "session_id": core_id,
                    "agent_alias": chat.agent_alias,
                    "created_at": chat.created_at,
                })
            })
            .collect();
        rows.sort_by(|a, b| a["created_at"].as_str().cmp(&b["created_at"].as_str()));
        rows
    }

    /// An open chat's connection and identity, for a call from outside the
    /// chat.
    fn reach(&self, core_id: &str) -> Option<(Arc<RpcClient>, TuiIdentity)> {
        self.held
            .lock()
            .open
            .get(core_id)
            .map(|chat| (Arc::clone(&chat.client), chat.identity.clone()))
    }

    /// Places taken.
    #[cfg(test)]
    pub(crate) fn places_taken(&self) -> usize {
        self.held.lock().taken
    }
}

/// What a chat presents to the core: the token the session signed in with,
/// and the provider it was issued by. Every connection a chat opens is bound
/// to that provider, so a provider repointed since the sign-in never
/// receives the token.
#[derive(Clone)]
pub(crate) struct CoreCredential {
    pub(crate) bearer: zeroize::Zeroizing<String>,
    pub(crate) pin: zeroclaw_runtime::rpc::auth::OidcBinding,
}

/// One handshake through `connector`, presenting `credential` and, when there
/// is one, the TUI identity, on a connection bound to the credential's
/// provider, and what resolves once the core is done with that connection.
/// The connection keeps `slot`, when there is one, until then.
async fn handshake(
    connector: &zeroclaw_runtime::rpc::inproc::InprocConnector,
    provider: &str,
    credential: &CoreCredential,
    identity: Option<&TuiIdentity>,
    slot: Option<&Arc<Auxiliary>>,
) -> Result<(Result<RpcClient, ClientError>, Served), CoreFailure> {
    let (stream, served) = connector
        .connect_bound(credential.pin.clone())
        .await
        .ok_or(CoreFailure::Unavailable)?;
    if let Some(slot) = slot {
        let (slot, served) = (Arc::clone(slot), served.clone());
        zeroclaw_spawn::spawn!(async move {
            served.wait().await;
            drop(slot);
        });
    }
    let options = ConnectOptions {
        auth_token: Some(credential.bearer.to_string()),
        auth_provider: Some(format!("oidc.{provider}")),
        tui_id: identity.map(|identity| identity.id.clone()),
        tui_sig: identity.map(|identity| identity.sig.clone()),
        ..ConnectOptions::default()
    };
    Ok((RpcClient::connect_over(stream, options).await, served))
}

/// A refusal at the handshake: the core no longer takes this token.
fn refuses_token(outcome: &Result<RpcClient, ClientError>) -> bool {
    matches!(outcome, Err(ClientError::Rpc(error)) if error.code == AUTH_REQUIRED)
}

/// One chat socket's conversation on the core, on a connection of its own.
/// Dropped without [`CoreChat::close`], it closes the conversation in a
/// task of its own.
pub(crate) struct CoreChat {
    caller: CoreCaller,
    pub(crate) core_id: String,
    agent_alias: String,
    cwd: Option<String>,
    /// The connection calls go to. Replaced when the core asks for a new
    /// handshake.
    client: Arc<RpcClient>,
    /// The connection the conversation was created or last rebound on: the
    /// one its turns' updates and approval requests reach.
    bound: Arc<RpcClient>,
    identity: TuiIdentity,
    credential: CoreCredential,
    closing: Option<Closing>,
}

/// What a chat needs to reach the core again on a connection of its own:
/// the session's credential, kept until the chat's conversation is closed
/// since it leaves the session store when the session ends, and the chat's
/// identity, so the conversation is still the chat's there.
#[derive(Clone)]
struct Redial {
    core: CoreRpc,
    provider: String,
    credential: CoreCredential,
    identity: TuiIdentity,
}

impl Redial {
    async fn connect(&self) -> Option<Arc<RpcClient>> {
        let connector = self.core.connector()?;
        match handshake(
            connector,
            &self.provider,
            &self.credential,
            Some(&self.identity),
            None,
        )
        .await
        {
            Ok((Ok(client), _)) => Some(Arc::new(client)),
            _ => None,
        }
    }

    /// Close `core_id`, on `client` first. A lost connection, a timeout or a
    /// demand for a new handshake is tried again on a new connection.
    /// Whether the conversation is gone from the core.
    async fn close(&self, core_id: &str, mut client: Option<Arc<RpcClient>>) -> bool {
        for attempt in 0..CLOSE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200) * attempt).await;
            }
            let current = match client.take() {
                Some(current) if current.state() == ConnectionState::Connected => current,
                _ => match self.connect().await {
                    Some(fresh) => fresh,
                    None => continue,
                },
            };
            match self.close_on(core_id, &current).await {
                CloseAttempt::Closed => return true,
                CloseAttempt::Refused(message) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(json!({ "session_id": core_id, "error": message })),
                        "a dashboard conversation was refused its close; the core retires it once its client has stayed away"
                    );
                    return false;
                }
                CloseAttempt::Retry => current.shutdown(),
            }
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(json!({ "session_id": core_id })),
            "a dashboard conversation could not be closed; the core retires it once its client has stayed away"
        );
        false
    }

    /// One attempt to close `core_id` on `client`.
    async fn close_on(&self, core_id: &str, client: &RpcClient) -> CloseAttempt {
        let closing = client.call::<Value>(Method::SessionClose, json!({ "session_id": core_id }));
        match tokio::time::timeout(CLOSE_TIMEOUT, closing).await {
            Ok(Ok(_)) => CloseAttempt::Closed,
            Ok(Err(ClientError::Rpc(error)))
                if error.code == SESSION_NOT_FOUND || error.code == SESSION_NOT_OWNED =>
            {
                CloseAttempt::Closed
            }
            Ok(Err(ClientError::Rpc(error))) if error.code != AUTH_REQUIRED => {
                if answered_by_core(client).await {
                    CloseAttempt::Refused(error.message)
                } else {
                    CloseAttempt::Retry
                }
            }
            // Lost, timed out, or the core wants a new handshake.
            _ => CloseAttempt::Retry,
        }
    }
}

/// How one attempt to close a conversation came out.
#[derive(Debug, PartialEq, Eq)]
enum CloseAttempt {
    /// The conversation is gone from the core.
    Closed,
    /// The core refused the close, with its message.
    Refused(String),
    /// Try again on a new connection.
    Retry,
}

/// Whether an RPC error on `client` is the core's answer. A send that the
/// closed writer queue refused is reported as an RPC error too, but it never
/// reached the core: by then the connection is marked disconnected, or its
/// outbound lane has closed.
async fn answered_by_core(client: &RpcClient) -> bool {
    client.state() == ConnectionState::Connected
        && tokio::time::timeout(Duration::ZERO, client.closed())
            .await
            .is_err()
}

/// What closing a chat's conversation needs, owned, so it can run after the
/// chat is gone.
struct Closing {
    redial: Redial,
    core_id: String,
    /// Held until the close is done, then dropped: the place stays taken
    /// while the conversation may still be live, and its account stays
    /// charged if the close fails.
    place: Place,
    stopping: Option<tokio::sync::watch::Receiver<bool>>,
    /// Ends the chat's expiry watch.
    done: CancellationToken,
}

impl Closing {
    async fn run(self, clients: Vec<Arc<RpcClient>>) {
        self.done.cancel();
        // A generation that stopped drains its sessions itself, and its
        // connector takes no more connections. A closed channel counts, as
        // in `DashboardAuth::is_retired`.
        let stopped = self
            .stopping
            .as_ref()
            .is_some_and(|stopping| *stopping.borrow() || stopping.has_changed().is_err());
        if !stopped
            && !self
                .redial
                .close(&self.core_id, clients.first().cloned())
                .await
        {
            self.place.keep_charged();
        }
        for client in clients {
            client.shutdown();
        }
    }
}

impl CoreChat {
    /// Every connection the chat holds, once each.
    fn clients(&self) -> Vec<Arc<RpcClient>> {
        let mut clients = vec![Arc::clone(&self.client)];
        if !Arc::ptr_eq(&self.client, &self.bound) {
            clients.push(Arc::clone(&self.bound));
        }
        clients
    }

    /// Close the conversation and let go of the chat's connections and its
    /// place.
    pub(crate) async fn close(mut self) {
        if let Some(closing) = self.closing.take() {
            closing.run(self.clients()).await;
        }
    }

    /// Cancelled when the chat's session ends.
    pub(crate) fn ended(&self) -> CancellationToken {
        self.caller.session.ended.clone()
    }

    /// The working directory the conversation was created with.
    pub(crate) fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// The connection the conversation's turns run on.
    pub(crate) fn bound(&self) -> Arc<RpcClient> {
        Arc::clone(&self.bound)
    }

    /// A new connection in place of one whose binding the core no longer
    /// honours (`AUTH_REQUIRED`), presenting the chat's own identity, so the
    /// conversation stays the chat's. The old one is not cut off: a turn
    /// running on it ends normally. A second refusal ends the session.
    pub(crate) async fn renew(&mut self) -> Result<(), CoreFailure> {
        let fresh = Arc::new(
            self.caller
                .connect(&self.credential, Some(&self.identity), None)
                .await?,
        );
        if !Arc::ptr_eq(&self.client, &self.bound) {
            self.client.shutdown();
        }
        self.client = fresh;
        self.caller.binding.moved(&self.core_id, &self.client);
        Ok(())
    }

    /// Before a turn: make sure the conversation is bound to the connection
    /// calls go to, so the turn's approval requests reach the chat. Only
    /// between turns, since the core would hold the rebind behind a running
    /// turn. A lost connection is replaced first.
    pub(crate) async fn rebind(&mut self) -> Result<(), CoreFailure> {
        if self.client.state() != ConnectionState::Connected {
            self.renew().await?;
        }
        if Arc::ptr_eq(&self.client, &self.bound) {
            return Ok(());
        }
        let mut params = json!({
            "session_id": self.core_id,
            "agent_alias": self.agent_alias,
        });
        if let Some(cwd) = &self.cwd {
            params["cwd"] = Value::String(cwd.clone());
        }
        let _: SessionNewResult = self.call(Method::SessionNew, params).await?;
        let previous = std::mem::replace(&mut self.bound, Arc::clone(&self.client));
        previous.shutdown();
        Ok(())
    }

    /// Call `method` on the chat's connection. A connection whose binding
    /// lapsed is replaced once with the chat's identity; a second refusal
    /// ends the session.
    pub(crate) async fn call<T: DeserializeOwned>(
        &mut self,
        method: Method,
        params: Value,
    ) -> Result<T, CoreFailure> {
        match self.client.call::<T>(method, params.clone()).await {
            Ok(result) => Ok(result),
            Err(ClientError::Rpc(error)) if error.code == AUTH_REQUIRED => {
                self.renew().await?;
                self.client
                    .call::<T>(method, params)
                    .await
                    .map_err(|error| self.caller.failure(error))
            }
            Err(error) => Err(self.caller.failure(error)),
        }
    }

    pub(crate) fn failure(&self, error: ClientError) -> CoreFailure {
        self.caller.failure(error)
    }

    pub(crate) fn end_session(&self) {
        self.caller.end_session();
    }
}

impl Drop for CoreChat {
    fn drop(&mut self) {
        if let Some(closing) = self.closing.take() {
            let clients = self.clients();
            zeroclaw_spawn::spawn!(closing.run(clients));
        }
    }
}

/// How the core's creation of a chat's conversation came out.
enum Created {
    Opened(Box<Unclaimed>),
    /// The core refused it: nothing was created.
    Failed(ClientError),
    /// The outcome is not known yet, or cannot be, and why: the creation's
    /// task owns the connection and settles the account's charge.
    Unsettled(String),
    /// The session ended while the core created it; it is closed.
    SessionEnded,
}

/// What the core's answer to `session/new` says.
enum Answer {
    Opened(SessionNewResult),
    /// A refusal: the core created nothing.
    Refused(ClientError),
    /// The answer does not say what the core did (a lost connection, or an
    /// answer that cannot be read), and why, with the conversation's id when
    /// it names one.
    Unknown(String, Option<String>),
}

impl Answer {
    fn of(answered: Result<Value, ClientError>) -> Self {
        match answered {
            Ok(value) => {
                let named = value
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                match serde_json::from_value::<SessionNewResult>(value) {
                    Ok(opened) => Self::Opened(opened),
                    Err(source) => Self::Unknown(
                        ClientError::Decode {
                            method: Method::SessionNew.wire_name().to_string(),
                            source,
                        }
                        .to_string(),
                        named,
                    ),
                }
            }
            Err(error @ ClientError::Rpc(_)) => Self::Refused(error),
            Err(error) => Self::Unknown(error.to_string(), None),
        }
    }

    /// Settle `account`'s charge for a creation no chat will take: given back
    /// once the core is known to hold no conversation for it, closing the one
    /// it created first; kept for the rest of the generation when that cannot
    /// be known.
    async fn settle(self, client: Arc<RpcClient>, redial: &Redial, account: Option<AccountPlace>) {
        let let_go = match self {
            Self::Refused(_) => true,
            Self::Opened(SessionNewResult { session_id, .. })
            | Self::Unknown(_, Some(session_id)) => {
                redial.close(&session_id, Some(Arc::clone(&client))).await
            }
            Self::Unknown(_, None) => false,
        };
        client.shutdown();
        if let Some(account) = account
            && !let_go
        {
            account.keep();
        }
    }
}

/// A conversation the core created that no chat has taken yet. Dropped
/// untaken, because its opener went away, even after the hand-over was sent,
/// it closes the conversation in a task of its own, and the chat's place
/// stays taken until that close is done.
struct Unclaimed {
    parts: Option<(SessionNewResult, Closing)>,
    client: Arc<RpcClient>,
}

impl Unclaimed {
    fn claim(mut self) -> Option<(SessionNewResult, Closing)> {
        self.parts.take()
    }

    async fn close(mut self) {
        if let Some((_, closing)) = self.parts.take() {
            closing.run(vec![Arc::clone(&self.client)]).await;
        }
    }
}

impl Drop for Unclaimed {
    fn drop(&mut self) {
        if let Some((_, closing)) = self.parts.take() {
            let clients = vec![Arc::clone(&self.client)];
            zeroclaw_spawn::spawn!(closing.run(clients));
        }
    }
}

/// Create a chat's conversation on `client`, in a task of its own that
/// shares the chat's `place`: a request that goes away while the core
/// creates it leaves the place taken, and what the core creates is closed
/// when nobody takes it, as it is when the session ended meanwhile. Past
/// `ceiling` the chat stops waiting and lets go of the session's place, but
/// the task keeps the account's share and listens on for the core's answer,
/// which settles it.
async fn create(
    client: Arc<RpcClient>,
    params: Value,
    session: Arc<SessionAuth>,
    redial: Redial,
    place: Place,
    stopping: Option<tokio::sync::watch::Receiver<bool>>,
    ceiling: Duration,
) -> Created {
    let (handed, taken) = tokio::sync::oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        // No deadline on the request itself: the answer, whenever it comes,
        // says what the core did.
        let answering = {
            let client = Arc::clone(&client);
            async move {
                client
                    .request_with_timeout(Method::SessionNew.wire_name(), params, Duration::MAX)
                    .await
            }
        };
        tokio::pin!(answering);
        let answered = tokio::select! {
            answered = &mut answering => answered,
            () = tokio::time::sleep(ceiling) => {
                let _ = handed.send(Created::Unsettled(
                    ClientError::Timeout {
                        method: Method::SessionNew.wire_name().to_string(),
                        after: ceiling,
                    }
                    .to_string(),
                ));
                let account = place.take_account();
                drop(place);
                Answer::of(answering.await)
                    .settle(client, &redial, account)
                    .await;
                return;
            }
        };
        let answer = Answer::of(answered);
        let opened = match answer {
            Answer::Opened(opened) => opened,
            // The core's definitive answer: it created nothing.
            Answer::Refused(error) => {
                let _ = handed.send(Created::Failed(error));
                return;
            }
            Answer::Unknown(ref why, _) => {
                let _ = handed.send(Created::Unsettled(why.clone()));
                let account = place.take_account();
                drop(place);
                answer.settle(client, &redial, account).await;
                return;
            }
        };
        let closing = Closing {
            redial,
            core_id: opened.session_id.clone(),
            place,
            stopping,
            done: CancellationToken::new(),
        };
        let unclaimed = Unclaimed {
            parts: Some((opened, closing)),
            client: Arc::clone(&client),
        };
        if session.ended.is_cancelled() {
            unclaimed.close().await;
            let _ = handed.send(Created::SessionEnded);
            return;
        }
        // An opener that has gone, or goes before it claims it, drops it,
        // which closes the conversation.
        let _ = handed.send(Created::Opened(Box::new(unclaimed)));
    });
    taken
        .await
        .unwrap_or_else(|_| Created::Unsettled("the creation was interrupted".into()))
}

/// Why a call through the core did not produce a result.
#[derive(Debug)]
pub(crate) enum CoreFailure {
    /// No core to reach: the generation is over, or the gateway is not
    /// inside the daemon.
    Unavailable,
    /// The session ended, or the core no longer accepts its token.
    SessionEnded,
    /// The core refused this account the operation.
    Forbidden(String),
    /// The session does not exist for this account.
    NotFound,
    /// Any other refusal from the core, with its message.
    Refused(String),
    /// The connection failed mid-call.
    Lost(String),
    /// The dashboard session already has as many chats open as it may.
    ConversationLimit,
}

impl CoreFailure {
    fn from_client(error: ClientError) -> Self {
        match error {
            ClientError::Rpc(error) => match error.code {
                AUTH_REQUIRED => Self::SessionEnded,
                FORBIDDEN => Self::Forbidden(error.message),
                SESSION_NOT_FOUND | SESSION_NOT_OWNED => Self::NotFound,
                _ => Self::Refused(error.message),
            },
            other => Self::Lost(other.to_string()),
        }
    }

    pub(crate) fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({
                    "error": "The agent runtime is not reachable from this gateway",
                    "reason": "core_unavailable",
                }),
            ),
            Self::SessionEnded => (
                StatusCode::UNAUTHORIZED,
                json!({
                    "error": "Your session has ended; sign in again",
                    "reason": "session_expired",
                }),
            ),
            Self::Forbidden(message) => (
                StatusCode::FORBIDDEN,
                json!({ "error": message, "reason": "not_permitted" }),
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                json!({ "error": "Session not found" }),
            ),
            Self::Refused(message) => (StatusCode::BAD_REQUEST, json!({ "error": message })),
            Self::ConversationLimit => (
                StatusCode::CONFLICT,
                json!({
                    "error": conversation_limit_message(),
                    "reason": "conversation_limit",
                }),
            ),
            Self::Lost(detail) => {
                // The browser gets the generic answer; the cause stays here.
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(json!({ "error": detail })),
                    "a dashboard call to the core lost its connection"
                );
                (
                    StatusCode::BAD_GATEWAY,
                    json!({
                        "error": "The agent runtime connection failed; try again",
                        "reason": "core_unavailable",
                    }),
                )
            }
        };
        (status, Json(body)).into_response()
    }
}

/// A request from a signed-in dashboard session that the core serves.
#[derive(Clone)]
pub(crate) struct CoreCaller {
    core: CoreRpc,
    auth: Arc<DashboardAuth>,
    pub(crate) session: Arc<SessionAuth>,
    pub(crate) binding: Arc<CoreBinding>,
}

/// Which signed-in sessions a route serves through the core.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// Every dashboard session (the agent list, which only narrows).
    AnySession,
    /// Scoped sessions only; an administrator keeps the in-process route.
    /// The chat and its conversations are served this way.
    ScopedOnly,
}

/// The session grants a chat through the core needs: to open the
/// conversation, to prompt it, to read it (the chat shows its transcript as
/// it runs), and to close it again (when the chat ends). An account without
/// them is not offered the chat, so no conversation is opened that it could
/// not use or close.
pub(crate) fn may_chat(grants: &zeroclaw_api::grants::ResolvedGrants) -> bool {
    use zeroclaw_api::grants::{Resource, Verb};
    [Verb::Create, Verb::Read, Verb::Execute, Verb::Update]
        .into_iter()
        .all(|verb| grants.permits(Resource::Sessions, verb))
}

/// The core caller for this request: a dashboard session with no bearer,
/// on a gateway that runs inside the daemon. `None` means the route answers
/// as it always has.
pub(crate) fn core_caller(
    state: &AppState,
    headers: &HeaderMap,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    reach: Reach,
) -> Option<CoreCaller> {
    if !state.pairing.require_pairing() || crate::api::extract_bearer_token(headers).is_some() {
        return None;
    }
    let DashboardCaller::Session(session) = crate::dashboard_session::current_caller()? else {
        return None;
    };
    if reach == Reach::ScopedOnly && session.is_admin() {
        return None;
    }
    let Extension(core) = core?;
    core.connector()?;
    let Extension(auth) = dashboard?;
    // A session that has just ended gets an empty binding; its chat is
    // refused for want of a credential anyway.
    let binding = auth.core_binding(&session).unwrap_or_default();
    Some(CoreCaller {
        core,
        auth,
        session,
        binding,
    })
}

impl CoreCaller {
    /// A connection of its own presenting `credential`, and `identity` when
    /// there is one. A refusal ends the session, without asking the identity
    /// provider again, unless a policy was published during the handshake:
    /// that fails it closed, as it should, and is no reason to sign anybody
    /// out, so the token is presented once more, as the session's own
    /// re-verification does. Each connection keeps `slot`, when there is
    /// one, until the core is done with it.
    async fn connect(
        &self,
        credential: &CoreCredential,
        identity: Option<&TuiIdentity>,
        slot: Option<&Arc<Auxiliary>>,
    ) -> Result<RpcClient, CoreFailure> {
        let connector = self.core.connector().ok_or(CoreFailure::Unavailable)?;
        let provider = self.session.alias.as_str();
        let published = self.auth.inbound.generation();
        let (mut outcome, served) =
            handshake(connector, provider, credential, identity, slot).await?;
        if refuses_token(&outcome) && self.auth.inbound.generation() != published {
            // One connection at a time: the refused one is over first.
            served.wait().await;
            outcome = handshake(connector, provider, credential, identity, slot)
                .await?
                .0;
        }
        match outcome {
            Ok(client) => Ok(client),
            Err(ClientError::Rpc(error))
                if error.code == AUTH_REQUIRED || error.code == FORBIDDEN =>
            {
                // The core no longer takes the token this session holds.
                self.end_session();
                Err(CoreFailure::SessionEnded)
            }
            Err(error) => Err(CoreFailure::Lost(error.to_string())),
        }
    }

    /// Open a chat: a place among the session's chats, a connection of its
    /// own, and a conversation the core creates on it.
    pub(crate) async fn open_chat(
        &self,
        agent_alias: &str,
        cwd: Option<&str>,
    ) -> Result<CoreChat, CoreFailure> {
        if self.session.ended.is_cancelled() {
            return Err(CoreFailure::SessionEnded);
        }
        let account = self
            .core
            .accounts
            .take(self.session.principal().id.as_str())?;
        let place = self.binding.take_place(account)?;
        let credential = self
            .auth
            .core_credential(&self.session)
            .ok_or(CoreFailure::SessionEnded)?;
        let mut client = Arc::new(self.connect(&credential, None, None).await?);
        let handshake = client.handshake();
        let identity = match (&handshake.tui_id, &handshake.tui_sig) {
            (Some(id), Some(sig)) => TuiIdentity {
                id: id.clone(),
                sig: sig.clone(),
            },
            _ => {
                client.shutdown();
                return Err(CoreFailure::Lost(
                    "the core gave the chat's connection no identity".into(),
                ));
            }
        };
        let redial = Redial {
            core: self.core.clone(),
            provider: self.session.alias.clone(),
            credential: credential.clone(),
            identity: identity.clone(),
        };
        let mut params = json!({ "agent_alias": agent_alias });
        if let Some(cwd) = cwd {
            params["cwd"] = Value::String(cwd.to_string());
        }
        let mut renewed = false;
        let (opened, closing) = loop {
            let created = create(
                Arc::clone(&client),
                params.clone(),
                Arc::clone(&self.session),
                redial.clone(),
                Arc::clone(&place),
                self.auth.generation_stopping(),
                self.core.accounts.create_ceiling(),
            )
            .await;
            match created {
                Created::Opened(unclaimed) => match unclaimed.claim() {
                    Some(parts) => break parts,
                    None => return Err(CoreFailure::Lost("the creation was lost".into())),
                },
                Created::SessionEnded => return Err(CoreFailure::SessionEnded),
                // The connection's binding lapsed while the creation waited
                // for the core (another creation may hold it up): once more on
                // a new connection, with the same identity. The core refuses
                // before it builds anything, so nothing was created.
                Created::Failed(ClientError::Rpc(error))
                    if error.code == AUTH_REQUIRED && !renewed =>
                {
                    renewed = true;
                    client.shutdown();
                    client = Arc::new(self.connect(&credential, Some(&identity), None).await?);
                }
                Created::Failed(error) => {
                    client.shutdown();
                    return Err(self.failure(error));
                }
                // The creation's task keeps the connection and settles it.
                Created::Unsettled(why) => return Err(CoreFailure::Lost(why)),
            }
        };
        place.hold(
            &opened.session_id,
            OpenChat {
                agent_alias: agent_alias.to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                client: Arc::clone(&client),
                identity: identity.clone(),
            },
        );
        self.end_before_expiry(closing.done.clone());
        Ok(CoreChat {
            caller: self.clone(),
            core_id: opened.session_id,
            agent_alias: agent_alias.to_string(),
            cwd: cwd.map(str::to_string),
            client: Arc::clone(&client),
            bound: client,
            identity,
            credential,
            closing: Some(closing),
        })
    }

    /// End the session shortly before its token expires, while that token
    /// can still close the session's conversations; `/api/auth/session`
    /// reports that end. Stops early when the chat is over (`done`).
    fn end_before_expiry(&self, done: CancellationToken) {
        if self.session.is_admin() {
            return;
        }
        let remaining = Duration::from_secs(self.session.expires_at.saturating_sub(unix_now()))
            .saturating_sub(CLOSE_BEFORE_EXPIRY);
        let (auth, session) = (Arc::clone(&self.auth), Arc::clone(&self.session));
        zeroclaw_spawn::spawn!(async move {
            tokio::select! {
                () = done.cancelled() => {}
                () = session.ended.cancelled() => {}
                () = tokio::time::sleep(remaining) => {
                    auth.end(&session);
                }
            }
        });
    }

    pub(crate) fn failure(&self, error: ClientError) -> CoreFailure {
        let failure = CoreFailure::from_client(error);
        if matches!(failure, CoreFailure::SessionEnded) {
            self.end_session();
        }
        failure
    }

    /// End the dashboard session: the core no longer accepts its token.
    pub(crate) fn end_session(&self) {
        self.auth.end(&self.session);
    }

    /// `GET /api/sessions`: the session's open chats. None can be reopened;
    /// a conversation ends with its chat.
    fn sessions_list(&self) -> Response {
        Json(json!({ "sessions": self.binding.listed() })).into_response()
    }

    /// `POST /api/sessions/{id}/abort`: stop an open chat's running turn.
    /// `session/abort` stops it whichever connection started it; an account
    /// without the grant it needs can still stop a turn its own connection
    /// started, through `session/cancel`.
    async fn session_abort(&self, id: &str) -> Response {
        let Some((client, identity)) = self.binding.reach(id) else {
            return Json(json!({ "status": "no_active_response" })).into_response();
        };
        let params = json!({ "session_id": id });
        let outcome = match abort_on(&client, params.clone()).await {
            // The chat's connection lapsed: once more on one of the abort's
            // own, with the chat's identity.
            Err(ClientError::Rpc(error)) if error.code == AUTH_REQUIRED => {
                let Some(credential) = self.auth.core_credential(&self.session) else {
                    return CoreFailure::SessionEnded.into_response();
                };
                let slot = match self
                    .core
                    .accounts
                    .auxiliary(self.session.principal().id.as_str())
                    .await
                {
                    Ok(slot) => Arc::new(slot),
                    Err(failure) => return failure.into_response(),
                };
                match self
                    .connect(&credential, Some(&identity), Some(&slot))
                    .await
                {
                    Ok(fresh) => {
                        let outcome = abort_on(&fresh, params).await;
                        fresh.shutdown();
                        outcome
                    }
                    Err(failure) => return failure.into_response(),
                }
            }
            other => other,
        };
        let status = match outcome {
            Ok(result) if result.cancelled => "aborted",
            Ok(_) => "no_active_response",
            Err(ClientError::Rpc(error))
                if error.code == SESSION_NOT_FOUND || error.code == SESSION_NOT_OWNED =>
            {
                "no_active_response"
            }
            Err(error) => return self.failure(error).into_response(),
        };
        Json(json!({ "status": status })).into_response()
    }

    /// The agents this account may chat with: the core's list, narrowed by
    /// the account's grants, kept for [`AGENTS_KEPT_FOR`] under one policy.
    /// The account's grants as they are now decide on every request, the
    /// read grant included: without `agents:read` the account is told only
    /// the agents it was granted by name, whatever was kept.
    async fn agents_available(&self, state: &AppState) -> Response {
        let grants = &self.session.grants;
        let generation = self.auth.inbound.generation();
        let list = match self.binding.kept_agents(generation) {
            Some(list) => list,
            None => match self.list_agents().await {
                Ok(list) => {
                    self.binding.keep_agents(generation, list.clone());
                    list
                }
                Err(failure) => return failure.into_response(),
            },
        };
        let may_read = grants.permits(
            zeroclaw_api::grants::Resource::Agents,
            zeroclaw_api::grants::Verb::Read,
        );
        let agents: Vec<Value> = match list {
            AgentList::Listed(agents) if may_read => agents
                .into_iter()
                .filter(|(alias, _)| grants.may_use_agent(alias))
                .map(|(alias, enabled)| json!({ "alias": alias, "enabled": enabled }))
                .collect(),
            AgentList::Listed(_) | AgentList::Forbidden => {
                let config = state.config.read();
                let mut named: Vec<Value> = grants
                    .allowed_agents
                    .iter()
                    .map(|alias| alias.as_str())
                    .filter(|alias| *alias != zeroclaw_api::grants::WILDCARD)
                    .filter_map(|alias| {
                        config
                            .agent(alias)
                            .map(|agent| json!({ "alias": alias, "enabled": agent.enabled }))
                    })
                    .collect();
                named.sort_by(|a, b| a["alias"].as_str().cmp(&b["alias"].as_str()));
                named.dedup();
                named
            }
        };
        Json(json!({ "agents": agents })).into_response()
    }

    /// The core's agent list, on a connection of the request's own, which
    /// keeps one of the account's connection slots until the core is done
    /// with it, even when the request goes away first or the handshake
    /// outlives its deadline: the core finishes a verification it started
    /// whether or not anyone still waits.
    async fn list_agents(&self) -> Result<AgentList, CoreFailure> {
        let slot = Arc::new(
            self.core
                .accounts
                .auxiliary(self.session.principal().id.as_str())
                .await?,
        );
        let credential = self
            .auth
            .core_credential(&self.session)
            .ok_or(CoreFailure::SessionEnded)?;
        let client = self.connect(&credential, None, Some(&slot)).await?;
        let listed = client
            .call::<AgentsListResult>(Method::AgentsList, json!({}))
            .await;
        client.shutdown();
        match listed {
            Ok(listed) => Ok(AgentList::Listed(
                listed
                    .agents
                    .into_iter()
                    .map(|agent| (agent.alias, agent.enabled))
                    .collect(),
            )),
            Err(error) => match self.failure(error) {
                CoreFailure::Forbidden(_) => Ok(AgentList::Forbidden),
                failure => Err(failure),
            },
        }
    }
}

/// `session/abort` on `client`, falling back to `session/cancel` for an
/// account the abort is refused.
async fn abort_on(client: &RpcClient, params: Value) -> Result<SessionCancelResult, ClientError> {
    match client
        .call::<SessionCancelResult>(Method::SessionAbort, params.clone())
        .await
    {
        Err(ClientError::Rpc(error)) if error.code == FORBIDDEN => {
            client
                .call::<SessionCancelResult>(Method::SessionCancel, params)
                .await
        }
        other => other,
    }
}

/// `GET /api/status` for a scoped session: what the chat needs (the
/// version, the locale, and the chosen agent's model), never the operator's
/// view of the host. An agent the account may not use is not described.
fn scoped_status(state: &AppState, session: &SessionAuth, agent: Option<&str>) -> Response {
    let config = state.config.read().clone();
    let locale = config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(zeroclaw_runtime::i18n::detect_locale);
    let mut body = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "locale": locale,
    });
    if let Some(alias) = agent.filter(|alias| !alias.trim().is_empty()) {
        let Some(model) = session
            .grants
            .may_use_agent(alias)
            .then(|| crate::api::agent_model(&config, alias))
            .flatten()
        else {
            return agent_not_allowed();
        };
        body["agent_alias"] = Value::String(alias.to_string());
        body["model_provider"] = json!(model.provider_ref);
        body["model"] = Value::String(model.model);
        body["temperature"] = json!(model.temperature);
    }
    Json(body).into_response()
}

/// 403 for an agent this account may not chat with, or that is not
/// configured: the two are not told apart.
/// An account without the grants a chat through the core needs; see
/// [`may_chat`].
pub(crate) fn chat_not_permitted() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "Your account can't chat here",
            "reason": "not_permitted",
        })),
    )
        .into_response()
}

pub(crate) fn agent_not_allowed() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "Your account can't use this agent",
            "reason": "agent_not_allowed",
        })),
    )
        .into_response()
}

// ── Routes ───────────────────────────────────────────────────────────
//
// Each wraps the route's in-process handler: a dashboard session the core
// serves goes to the core, anything else (an administrator's session
// included) to the handler unchanged.

/// `GET /api/sessions`.
pub async fn sessions_list(
    State(state): State<AppState>,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    headers: HeaderMap,
) -> Response {
    match core_caller(&state, &headers, core, dashboard, Reach::ScopedOnly) {
        Some(caller) => caller.sessions_list(),
        None => crate::api::handle_api_sessions_list(State(state), headers)
            .await
            .into_response(),
    }
}

/// `GET /api/sessions/{id}/messages`. A scoped session's conversation ends
/// with its chat and is never reopened, so it has no transcript to load.
pub async fn session_messages(
    State(state): State<AppState>,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    match core_caller(&state, &headers, core, dashboard, Reach::ScopedOnly) {
        Some(_) => CoreFailure::NotFound.into_response(),
        None => crate::api::handle_api_session_messages(State(state), headers, Path(id))
            .await
            .into_response(),
    }
}

/// `PUT /api/sessions/{id}`. Not offered for a scoped session's chats.
pub async fn session_rename(
    State(state): State<AppState>,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    match core_caller(&state, &headers, core, dashboard, Reach::ScopedOnly) {
        Some(_) => CoreFailure::NotFound.into_response(),
        None => crate::api::handle_api_session_rename(State(state), headers, Path(id), Json(body))
            .await
            .into_response(),
    }
}

/// `DELETE /api/sessions/{id}`. Not offered for a scoped session's chats,
/// whose conversations close when the chat does.
pub async fn session_delete(
    State(state): State<AppState>,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    match core_caller(&state, &headers, core, dashboard, Reach::ScopedOnly) {
        Some(_) => CoreFailure::NotFound.into_response(),
        None => crate::api::handle_api_session_delete(State(state), headers, Path(id))
            .await
            .into_response(),
    }
}

/// `POST /api/sessions/{id}/abort`.
pub async fn session_abort(
    State(state): State<AppState>,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    match core_caller(&state, &headers, core, dashboard, Reach::ScopedOnly) {
        Some(caller) => caller.session_abort(&id).await,
        None => crate::api::handle_api_session_abort(State(state), headers, Path(id))
            .await
            .into_response(),
    }
}

/// `GET /api/status`.
pub async fn status(
    State(state): State<AppState>,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    headers: HeaderMap,
    Query(query): Query<crate::api::StatusQuery>,
) -> Response {
    match core_caller(&state, &headers, core, dashboard, Reach::ScopedOnly) {
        Some(caller) => scoped_status(&state, &caller.session, query.agent.as_deref()),
        None => crate::api::handle_api_status(State(state), headers, Query(query))
            .await
            .into_response(),
    }
}

/// `GET /api/agents/available`: the agents the caller may chat with. A
/// dashboard session gets the core's list narrowed by its grants; an
/// operator (paired, or an administrator session on a gateway outside the
/// daemon) gets every configured agent.
pub async fn agents_available(
    State(state): State<AppState>,
    core: Option<Extension<CoreRpc>>,
    dashboard: Option<Extension<Arc<DashboardAuth>>>,
    headers: HeaderMap,
) -> Response {
    if let Some(caller) = core_caller(&state, &headers, core, dashboard, Reach::AnySession) {
        return caller.agents_available(&state).await;
    }
    if let Err(denied) = crate::api::require_auth(&state, &headers) {
        return denied.into_response();
    }
    let config = state.config.read();
    let mut agents: Vec<Value> = config
        .agents
        .iter()
        .map(|(alias, agent)| json!({ "alias": alias, "enabled": agent.enabled }))
        .collect();
    agents.sort_by(|a, b| a["alias"].as_str().cmp(&b["alias"].as_str()));
    Json(json!({ "agents": agents })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use std::net::SocketAddr;
    use std::path::Path as FsPath;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request as HttpRequest;
    use axum::routing::{get, post};
    use futures_util::{SinkExt as _, StreamExt as _};
    use http_body_util::BodyExt as _;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tower::ServiceExt as _;
    use wiremock::matchers::{body_string_contains, method as http_method, path as http_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_config::pairing::{PairingCodePolicy, PairingGuard};
    use zeroclaw_config::schema::{
        AliasedAgentConfig, Config, OidcConfig, OidcValidation, PermissionProfileConfig,
        RiskProfileConfig, WireApi,
    };
    use zeroclaw_infra::session_backend::SessionBackend as _;
    use zeroclaw_runtime::rpc::context::RpcContext;
    use zeroclaw_runtime::rpc::inproc::InprocConnector;
    use zeroclaw_runtime::rpc::session::SessionStore;

    use crate::chat_core::{WS_CLOSE_CONVERSATION_CLOSED, WS_CLOSE_CONVERSATION_LIMIT};
    use crate::dashboard_session::{DashboardLayer, DashboardSessions, ProviderPin};
    use crate::principal_gate::GatewayInboundAuth;

    const HOST: &str = "localhost:42617";
    const ORIGIN: &str = "http://localhost:42617";

    /// An identity provider whose introspection endpoint vouches for
    /// `tok-<name>` as `<name>`: alice and bob are scoped chat users, carol a
    /// chat user who may also read the agent list, root an administrator.
    async fn idp() -> MockServer {
        let server = MockServer::start().await;
        let issuer = server.uri();
        Mock::given(http_method("GET"))
            .and(http_path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "introspection_endpoint": format!("{issuer}/introspect"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        for (name, group) in [
            ("alice", "chatters"),
            ("bob", "chatters"),
            ("carol", "readers"),
            ("dave", "nodelete"),
            ("erin", "noread"),
            ("root", "admins"),
        ] {
            Mock::given(http_method("POST"))
                .and(http_path("/introspect"))
                .and(body_string_contains(format!("token=tok-{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "active": true,
                    "token_type": "Bearer",
                    "client_id": "gw",
                    "iss": issuer,
                    "sub": name,
                    "aud": "zeroclaw",
                    "exp": unix_now() + 600,
                    "groups": [group],
                    "preferred_username": format!("{name}@example.com"),
                })))
                .mount(&server)
                .await;
        }
        server
    }

    /// A model provider that answers every turn with the same text, holding
    /// a turn whose prompt says `slow please` for a few seconds.
    async fn model() -> MockServer {
        let server = MockServer::start().await;
        // "please read the note" calls `file_read`; once the tool's answer
        // (or its refusal) is in the conversation, the turn ends in text.
        Mock::given(http_method("POST"))
            .and(http_path("/chat/completions"))
            .and(body_string_contains("please read the note"))
            .and(body_string_contains("call_fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "read it"}}]
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/chat/completions"))
            .and(body_string_contains("please read the note"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_fixture",
                            "type": "function",
                            "function": {
                                "name": "file_read",
                                "arguments": "{\"path\":\"note.txt\"}",
                            },
                        }],
                    },
                    "finish_reason": "tool_calls",
                }]
            })))
            .with_priority(2)
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/chat/completions"))
            .and(body_string_contains("slow please"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "choices": [{"message": {"role": "assistant", "content": "slowly"}}]
                    }))
                    .set_delay(Duration::from_secs(4)),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(http_method("POST"))
            .and(http_path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": "hello from support"}}]
            })))
            .mount(&server)
            .await;
        server
    }

    /// Dashboard sign-in through `corp`; chat users may use `support` only,
    /// with the session grants a chat needs and nothing else.
    fn config(issuer: &str, model_uri: &str, root: &FsPath) -> Config {
        let mut config = Config {
            data_dir: root.join("data"),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config.gateway.require_pairing = true;
        config.gateway.paired_tokens = vec!["zc_paired".into()];
        config.gateway.dashboard_sign_in.oidc = vec!["corp".into()];
        config.oidc.insert(
            "corp".into(),
            OidcConfig {
                issuer: issuer.to_string(),
                audience: "zeroclaw".into(),
                client_id: "gw".into(),
                client_secret: Some("s3cret".into()),
                validation: OidcValidation::Introspection,
                claim_path: "groups".into(),
                profile_map: Map::from([
                    ("admins".to_string(), "admin".to_string()),
                    ("chatters".to_string(), "chat-user".to_string()),
                    ("readers".to_string(), "chat-reader".to_string()),
                    ("nodelete".to_string(), "chat-no-delete".to_string()),
                    ("noread".to_string(), "chat-no-read".to_string()),
                ]),
                interactive_clients: vec!["gw".into()],
                service_clients: vec!["svc".into()],
                ..OidcConfig::default()
            },
        );
        config.permission_profiles.insert(
            "admin".into(),
            PermissionProfileConfig {
                admin: true,
                ..PermissionProfileConfig::default()
            },
        );
        config.permission_profiles.insert(
            "chat-user".into(),
            PermissionProfileConfig {
                allowed_agents: vec!["support".into()],
                grants: Map::from([(
                    Resource::Sessions,
                    vec![
                        Verb::Create,
                        Verb::Read,
                        Verb::Update,
                        Verb::Execute,
                        Verb::Delete,
                    ],
                )]),
                ..PermissionProfileConfig::default()
            },
        );
        config.permission_profiles.insert(
            "chat-no-read".into(),
            PermissionProfileConfig {
                allowed_agents: vec!["support".into()],
                grants: Map::from([(
                    Resource::Sessions,
                    vec![Verb::Create, Verb::Update, Verb::Execute],
                )]),
                ..PermissionProfileConfig::default()
            },
        );
        config.permission_profiles.insert(
            "chat-no-delete".into(),
            PermissionProfileConfig {
                allowed_agents: vec!["support".into()],
                grants: Map::from([(
                    Resource::Sessions,
                    vec![Verb::Create, Verb::Read, Verb::Update, Verb::Execute],
                )]),
                ..PermissionProfileConfig::default()
            },
        );
        config.permission_profiles.insert(
            "chat-reader".into(),
            PermissionProfileConfig {
                allowed_agents: vec!["support".into()],
                grants: Map::from([
                    (Resource::Sessions, vec![Verb::Create, Verb::Read]),
                    (Resource::Agents, vec![Verb::Read]),
                ]),
                ..PermissionProfileConfig::default()
            },
        );
        {
            let provider = config
                .providers
                .models
                .ensure("openai", "test-provider")
                .expect("`openai` slot must exist");
            provider.api_key = Some("test-key".into());
            provider.model = Some("test-model".into());
            provider.uri = Some(model_uri.to_string());
            provider.wire_api = Some(WireApi::ChatCompletions);
        }
        for alias in ["support", "other"] {
            config.agents.insert(
                alias.into(),
                AliasedAgentConfig {
                    enabled: true,
                    model_provider: "openai.test-provider".into(),
                    risk_profile: "test-profile".into(),
                    ..Default::default()
                },
            );
        }
        config
            .risk_profiles
            .insert("test-profile".into(), RiskProfileConfig::default());
        config
    }

    /// One daemon generation's core: a context on `config` with durable
    /// sessions under its data dir and TUI signing on (the in-process
    /// connection is non-local, and non-local `initialize` needs signing),
    /// behind a bound in-process connector.
    /// Holds a core hook while armed. On session start: after the core has
    /// published the session and before `session/new` answers, the window
    /// in which an open that is cancelled, or races another, can lose track
    /// of a live conversation. On session end: after a close has removed
    /// the session and before `session/close` answers.
    struct HookGate {
        armed: std::sync::atomic::AtomicBool,
        held: std::sync::atomic::AtomicUsize,
        /// Every time the hook ran, armed or not.
        seen: std::sync::atomic::AtomicUsize,
        open: tokio::sync::Semaphore,
    }

    impl HookGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                armed: std::sync::atomic::AtomicBool::new(false),
                held: std::sync::atomic::AtomicUsize::new(0),
                seen: std::sync::atomic::AtomicUsize::new(0),
                open: tokio::sync::Semaphore::new(0),
            })
        }

        fn seen(&self) -> usize {
            self.seen.load(std::sync::atomic::Ordering::SeqCst)
        }

        async fn pass(&self) {
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
                self.held.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Ok(permit) = self.open.acquire().await {
                    permit.forget();
                }
            }
        }

        fn arm(&self) {
            self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn release(&self) {
            self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
            self.open.add_permits(1024);
        }

        fn held(&self) -> usize {
            self.held.load(std::sync::atomic::Ordering::SeqCst)
        }

        async fn wait_held(&self, count: usize) {
            tokio::time::timeout(Duration::from_secs(15), async {
                while self.held() < count {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the core reached the gated hook");
        }
    }

    struct GatedHooks {
        start: Arc<HookGate>,
        end: Arc<HookGate>,
    }

    #[async_trait::async_trait]
    impl zeroclaw_api::hook::HookHandler for GatedHooks {
        fn name(&self) -> &str {
            "gated-hooks"
        }

        async fn on_session_start(&self, _session_id: &str, _channel: &str) {
            self.start.pass().await;
        }

        async fn on_session_end(&self, _session_id: &str, _channel: &str) {
            self.end.pass().await;
        }
    }

    fn core_generation(
        config: &Config,
        gates: (&Arc<HookGate>, &Arc<HookGate>),
    ) -> (
        CancellationToken,
        InprocConnector,
        CoreRpc,
        Arc<SessionStore>,
        Arc<zeroclaw_runtime::rpc::auth::RpcInboundAuth>,
    ) {
        let authority = zeroclaw_runtime::LiveConfigAuthority::new(config.clone());
        let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
            4, 10, 60,
        ));
        let store = Arc::new(SessionStore::new(64, queue));
        let mut ctx = RpcContext::for_authority(&authority, Arc::clone(&store));
        let key_dir = config.data_dir.join("keys");
        std::fs::create_dir_all(&key_dir).unwrap();
        std::fs::write(key_dir.join(".secret_key"), "42".repeat(32)).unwrap();
        {
            let ctx = Arc::get_mut(&mut ctx).expect("a fresh context has one owner");
            ctx.tui_registry = Arc::new(zeroclaw_runtime::rpc::tui_identity::TuiRegistry::new(
                &key_dir,
            ));
            ctx.session_backend = Some(Arc::new(
                zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(&config.data_dir)
                    .expect("sqlite session backend"),
            ));
            let mut hooks = zeroclaw_runtime::hooks::HookRunner::new();
            hooks.register(Box::new(GatedHooks {
                start: Arc::clone(gates.0),
                end: Arc::clone(gates.1),
            }));
            ctx.hooks = Some(Arc::new(hooks));
        }
        let core_auth = Arc::clone(&ctx.auth);
        let generation = CancellationToken::new();
        let connector = InprocConnector::new(generation.clone());
        // As the daemon does. It closes nothing within a test unless the test
        // lowers `wss.orphaned_session_timeout_secs` from its 30 minutes.
        zeroclaw_runtime::rpc::orphans::spawn_orphan_retirement(&ctx, generation.clone());
        connector.bind(ctx);
        let core = CoreRpc::default();
        core.attach_inproc(connector.clone(), ConnectOptions::default());
        (generation, connector, core, store, core_auth)
    }

    struct Fixture {
        _idp: MockServer,
        _model: MockServer,
        _root: tempfile::TempDir,
        config: Config,
        state: AppState,
        auth: Arc<DashboardAuth>,
        generation: CancellationToken,
        connector: InprocConnector,
        core: CoreRpc,
        /// The core's live sessions.
        store: Arc<SessionStore>,
        /// The gateway's policy authority, to publish a changed policy.
        rpc_auth: Arc<zeroclaw_runtime::rpc::auth::RpcInboundAuth>,
        /// The core's own policy authority in this fixture.
        core_auth: Arc<zeroclaw_runtime::rpc::auth::RpcInboundAuth>,
        start_gate: Arc<HookGate>,
        end_gate: Arc<HookGate>,
    }

    impl Fixture {
        async fn new() -> Self {
            Self::with_config(|_| {}).await
        }

        async fn with_config(tweak: impl FnOnce(&mut Config)) -> Self {
            let idp = idp().await;
            let model = model().await;
            let root = tempfile::tempdir().unwrap();
            let mut config = config(&idp.uri(), &model.uri(), root.path());
            tweak(&mut config);
            let (start_gate, end_gate) = (HookGate::new(), HookGate::new());
            let (generation, connector, core, store, core_auth) =
                core_generation(&config, (&start_gate, &end_gate));
            let (state, auth, rpc_auth) = Self::gateway(&config);
            Self {
                _idp: idp,
                _model: model,
                _root: root,
                config,
                state,
                auth,
                generation,
                connector,
                core,
                store,
                rpc_auth,
                core_auth,
                start_gate,
                end_gate,
            }
        }

        fn gateway(
            config: &Config,
        ) -> (
            AppState,
            Arc<DashboardAuth>,
            Arc<zeroclaw_runtime::rpc::auth::RpcInboundAuth>,
        ) {
            let pairing = Arc::new(PairingGuard::new(
                true,
                &config.gateway.paired_tokens,
                PairingCodePolicy::default(),
            ));
            let rpc_auth = Arc::new(
                zeroclaw_runtime::rpc::auth::RpcInboundAuth::from_config(
                    config,
                    Arc::clone(&pairing),
                )
                .expect("inbound auth builds"),
            );
            let inbound = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc_auth)));
            let state = AppState {
                pairing,
                ..crate::api::tests::test_state(config.clone())
            };
            let auth = Arc::new(
                DashboardAuth::new(Arc::new(DashboardSessions::default()), inbound)
                    .for_generation(&state),
            );
            (state, auth, rpc_auth)
        }

        /// Publish the same policy with `group` no longer mapped to a
        /// profile, to the gateway's authority only: the core, which has its
        /// own here, still takes that account's calls, so only the gateway's
        /// own checks can stop them.
        fn revoke_group(&self, group: &str) {
            let mut revoked = self.config.clone();
            if let Some(corp) = revoked.oidc.get_mut("corp") {
                corp.profile_map.remove(group);
            }
            self.rpc_auth
                .publish_accepted(&revoked, self.rpc_auth.accepted_revision() + 1)
                .expect("the revoked policy is accepted");
        }

        /// [`Self::revoke_group`] for the core's authority too, as a policy
        /// change reaches both in a daemon.
        fn revoke_group_everywhere(&self, group: &str) {
            self.revoke_group(group);
            let mut revoked = self.config.clone();
            if let Some(corp) = revoked.oidc.get_mut("corp") {
                corp.profile_map.remove(group);
            }
            self.core_auth
                .publish_accepted(&revoked, self.core_auth.accepted_revision() + 1)
                .expect("the revoked policy is accepted by the core");
        }

        /// Wait until the core holds no live conversation at all.
        async fn assert_nothing_live(&self, why: &str) {
            let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                while !self.store.list_ids().await.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            assert!(
                outcome.is_ok(),
                "{why}: still live: {:?}",
                self.store.list_ids().await
            );
        }

        /// The daemon restarts: the generation drains, a new one starts on
        /// the same data, and every dashboard session is gone.
        async fn restart(&mut self) {
            self.connector.close_admission();
            self.generation.cancel();
            self.connector.drain().await;
            let (generation, connector, core, store, core_auth) =
                core_generation(&self.config, (&self.start_gate, &self.end_gate));
            let (state, auth, rpc_auth) = Self::gateway(&self.config);
            self.generation = generation;
            self.connector = connector;
            self.core = core;
            self.store = store;
            self.core_auth = core_auth;
            self.state = state;
            self.auth = auth;
            self.rpc_auth = rpc_auth;
        }

        /// The daemon reloads: the generation drains and a new one starts on
        /// the same data, while the gateway process, and with it every
        /// dashboard session, lives on.
        async fn reload(&mut self) {
            self.connector.close_admission();
            self.generation.cancel();
            self.connector.drain().await;
            let (generation, connector, core, store, core_auth) =
                core_generation(&self.config, (&self.start_gate, &self.end_gate));
            self.generation = generation;
            self.connector = connector;
            self.core = core;
            self.store = store;
            self.core_auth = core_auth;
        }

        /// A full reload: the old generation is told to stop and drains, and
        /// a new core and gateway start, the gateway over the same
        /// process-wide dashboard sessions, as `run_gateway` builds them.
        async fn reload_gateway(&mut self) {
            let _ = self.state.shutdown_tx.send(true);
            self.reload().await;
            let (state, _, rpc_auth) = Self::gateway(&self.config);
            let inbound = Arc::new(GatewayInboundAuth::from_shared(Arc::clone(&rpc_auth)));
            self.auth = Arc::new(
                DashboardAuth::new(Arc::clone(&self.auth.sessions), inbound).for_generation(&state),
            );
            self.state = state;
            self.rpc_auth = rpc_auth;
        }

        /// The routes O-2 serves, wired as `run_gateway` wires them: the
        /// core handle inside, the session layer outside.
        fn router(&self) -> Router {
            Router::new()
                .merge(crate::api_oidc::routes())
                .route("/api/sessions", get(sessions_list))
                .route("/api/sessions/{id}/messages", get(session_messages))
                .route(
                    "/api/sessions/{id}",
                    axum::routing::delete(session_delete).put(session_rename),
                )
                .route("/api/sessions/{id}/abort", post(session_abort))
                .route("/api/status", get(status))
                .route("/api/agents/available", get(agents_available))
                .route("/ws/chat", get(crate::ws::handle_ws_chat))
                .with_state(self.state.clone())
                .layer(Extension(self.core.clone()))
                .layer(axum::middleware::from_fn_with_state(
                    DashboardLayer {
                        state: self.state.clone(),
                        auth: Arc::clone(&self.auth),
                    },
                    crate::dashboard_session::layer,
                ))
        }

        async fn sign_in(&self, name: &str) -> (String, Arc<SessionAuth>) {
            let provider = ProviderPin::of(&self.config.oidc["corp"]);
            self.auth
                .sign_in("corp", provider, format!("tok-{name}"), 3600)
                .await
                .expect("sign-in succeeds")
        }
    }

    async fn call(
        router: &Router,
        method: &str,
        path: &str,
        session: Option<(&str, &SessionAuth)>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header("host", HOST);
        if let Some((cookie, session)) = session {
            builder = builder.header("cookie", format!("zc_session={cookie}"));
            if method != "GET" {
                builder = builder
                    .header("origin", ORIGIN)
                    .header("x-zeroclaw-csrf", session.csrf.as_str());
            }
        }
        let body = match body {
            Some(body) => {
                builder = builder.header("content-type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let mut request = builder.body(body).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 39999))));
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn listed(body: &Value) -> Vec<String> {
        body["sessions"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["session_id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn serve(router: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        zeroclaw_spawn::spawn!(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        addr
    }

    /// Open the chat socket as the session behind `cookie`.
    async fn open_chat(
        addr: SocketAddr,
        cookie: &str,
        agent: &str,
        session_id: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Error,
    > {
        let mut request = format!("ws://{addr}/ws/chat?agent={agent}&session_id={session_id}")
            .into_client_request()
            .unwrap();
        let headers = request.headers_mut();
        headers.insert("cookie", format!("zc_session={cookie}").parse().unwrap());
        headers.insert("origin", format!("http://{addr}").parse().unwrap());
        connect_async(request).await.map(|(socket, _)| socket)
    }

    /// Chat one message and return the frame that ended the turn.
    async fn chat_once(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        content: &str,
    ) -> Value {
        let message = json!({ "type": "message", "content": content });
        socket
            .send(ClientMessage::Text(message.to_string().into()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let frame = socket.next().await.expect("socket open").expect("frame");
                let Ok(text) = frame.into_text() else {
                    continue;
                };
                let frame: Value = serde_json::from_str(&text).unwrap();
                if matches!(frame["type"].as_str(), Some("done" | "error" | "aborted")) {
                    return frame;
                }
            }
        })
        .await
        .expect("the turn ends")
    }

    type ChatSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// Read the socket until it closes: the JSON frames it sent, and the
    /// close code.
    async fn read_to_close(socket: &mut ChatSocket) -> (Vec<Value>, Option<u16>) {
        let mut frames = Vec::new();
        let code = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    ClientMessage::Text(text) => frames.push(serde_json::from_str(&text).unwrap()),
                    ClientMessage::Close(frame) => return frame.map(|frame| u16::from(frame.code)),
                    _ => {}
                }
            }
            None
        })
        .await
        .expect("the socket closes");
        (frames, code)
    }

    /// Open a chat in the conversation the dashboard calls `dashboard_id`,
    /// chat once, and return the socket with the core's id for its
    /// conversation. Dropping the socket closes the conversation.
    async fn chat_in(addr: SocketAddr, cookie: &str, dashboard_id: &str) -> (ChatSocket, String) {
        let mut socket = open_chat(addr, cookie, "support", dashboard_id)
            .await
            .expect("the account may chat");
        let start = await_frame(&mut socket, "session_start").await;
        assert_eq!(chat_once(&mut socket, "remember me").await["type"], "done");
        let core_id = start["session_id"]
            .as_str()
            .expect("the core's id")
            .to_string();
        (socket, core_id)
    }

    /// A core connection of `name`'s own, as a chat opens one: the other
    /// account in the isolation tests, calling the core directly.
    async fn core_connection(fixture: &Fixture, name: &str) -> RpcClient {
        let stream = fixture
            .connector
            .connect()
            .await
            .expect("the connector is bound");
        RpcClient::connect_over(
            stream,
            ConnectOptions {
                auth_token: Some(format!("tok-{name}")),
                auth_provider: Some("oidc.corp".into()),
                ..ConnectOptions::default()
            },
        )
        .await
        .expect("the handshake succeeds")
    }

    /// Wait until the core no longer holds `core_id` live, then show its
    /// transcript is still there: closed, not deleted.
    async fn assert_closed_not_deleted(fixture: &Fixture, core_id: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            while fixture
                .store
                .list_ids()
                .await
                .iter()
                .any(|id| id == core_id)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the conversation is closed");
        let durable =
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(&fixture.config.data_dir)
                .expect("sqlite session backend");
        let transcript = durable.load(&format!("rpc_{core_id}"));
        assert!(
            transcript
                .iter()
                .any(|message| message.content.contains("remember me")),
            "closed, not deleted: {transcript:?}"
        );
    }

    #[tokio::test]
    async fn a_scoped_chat_runs_as_its_principal_and_only_they_see_it() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (bob, bob_session) = fixture.sign_in("bob").await;

        let mut socket = open_chat(addr, &alice, "support", "alice-chat")
            .await
            .expect("alice may chat with support");
        let start = await_frame(&mut socket, "session_start").await;
        let done = chat_once(&mut socket, "hi there").await;
        assert_eq!(done["type"], "done", "{done}");
        assert_eq!(done["full_response"], "hello from support");
        let core_id = start["session_id"]
            .as_str()
            .expect("the core's id")
            .to_string();

        // The core ran the chat as alice: on disk, where it outlives every
        // connection, the conversation is hers.
        let durable =
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(&fixture.config.data_dir)
                .expect("sqlite session backend");
        assert_eq!(
            durable
                .get_session_metadata(&format!("rpc_{core_id}"))
                .and_then(|metadata| metadata.principal_id)
                .as_deref(),
            Some(alice_session.principal().id.as_str()),
            "the conversation belongs to the signed-in principal"
        );

        let (status, body) = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(listed(&body), vec![core_id.clone()], "{body}");
        let (status, body) = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&bob, &bob_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            listed(&body).is_empty(),
            "bob has no conversations and must not see alice's: {body}"
        );

        // Bob names alice's real core id while her conversation is live, on a
        // core connection of his own, so only the core's ownership checks can
        // refuse him.
        let bobs = core_connection(&fixture, "bob").await;
        let read = bobs
            .call::<Value>(Method::SessionMessages, json!({ "session_id": core_id }))
            .await;
        assert!(
            read.is_err(),
            "bob must not read alice's live conversation: {read:?}"
        );
        let (status, body) = call(
            &router,
            "GET",
            &format!("/api/sessions/{core_id}/messages"),
            Some((&bob, &bob_session)),
            None,
        )
        .await;
        assert_ne!(status, StatusCode::OK, "{body}");
        assert!(!body.to_string().contains("hi there"), "{body}");
    }

    #[tokio::test]
    async fn isolation_holds_across_a_daemon_restart() {
        let mut fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let (socket, core_id) = chat_in(addr, &alice, "kept-chat").await;
        assert_ne!(core_id, "kept-chat", "the core assigns the id");
        drop(socket);

        fixture.restart().await;
        let router = fixture.router();
        let (bob, bob_session) = fixture.sign_in("bob").await;
        let (_, body) = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&bob, &bob_session)),
            None,
        )
        .await;
        assert!(
            !listed(&body).contains(&core_id),
            "a restart must not hand alice's conversation to bob: {body}"
        );
        let bobs = core_connection(&fixture, "bob").await;
        let read = bobs
            .call::<Value>(Method::SessionMessages, json!({ "session_id": core_id }))
            .await;
        assert!(
            read.is_err(),
            "bob must not read alice's transcript: {read:?}"
        );
        assert!(!format!("{read:?}").contains("remember me"));

        // A scoped account's conversation ends with its chat; a restart
        // leaves the new sign-in nothing.
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (_, body) = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert!(listed(&body).is_empty(), "{body}");
    }

    /// A reload ends every chat: the gateway tells each socket to reconnect,
    /// and the old generation drains its conversations. The sign-in lives
    /// on, and the next socket starts a conversation of its own.
    #[tokio::test]
    async fn a_daemon_reload_ends_the_conversations_but_not_the_sign_in() {
        let mut fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (mut socket, first) = chat_in(addr, &alice, "web-1").await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");

        fixture.reload_gateway().await;
        let (_, code) = read_to_close(&mut socket).await;
        assert_eq!(code, Some(1001), "the socket is told to reconnect");
        tokio::time::timeout(Duration::from_secs(5), async {
            while binding.places_taken() != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the chat let go of its place");

        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (status, body) = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the sign-in survives the reload: {body}"
        );
        assert!(listed(&body).is_empty(), "{body}");
        let (_socket, second) = chat_in(addr, &alice, "web-1").await;
        assert_ne!(second, first, "a conversation of its own");
    }

    #[tokio::test]
    async fn an_administrator_keeps_the_in_process_chat_and_conversations() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (root, root_session) = fixture.sign_in("root").await;
        let _ = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&root, &root_session)),
            None,
        )
        .await;
        let _ = call(
            &router,
            "GET",
            "/api/sessions/any/messages",
            Some((&root, &root_session)),
            None,
        )
        .await;
        drop(open_chat(addr, &root, "support", "root-chat").await);
        assert!(
            fixture.store.list_ids().await.is_empty(),
            "an administrator's chat and conversations stay in-process"
        );
        let binding = fixture.auth.core_binding(&root_session).expect("signed in");
        assert_eq!(binding.places_taken(), 0);
    }

    #[tokio::test]
    async fn an_agent_the_account_was_not_granted_is_refused_before_the_upgrade() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let refused = open_chat(addr, &alice, "other", "nope").await;
        let Err(tokio_tungstenite::tungstenite::Error::Http(response)) = refused else {
            panic!("the upgrade must be refused");
        };
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body: Value =
            serde_json::from_slice(response.body().as_deref().unwrap_or_default()).unwrap();
        assert_eq!(body["reason"], "agent_not_allowed", "{body}");
    }

    #[tokio::test]
    async fn agents_and_status_are_narrowed_to_the_account() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (root, root_session) = fixture.sign_in("root").await;

        let (status, body) = call(
            &router,
            "GET",
            "/api/agents/available",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({"agents": [{"alias": "support", "enabled": true}]})
        );
        // An account that may read the agent list gets the core's list,
        // narrowed to the agents it was granted.
        let (carol, carol_session) = fixture.sign_in("carol").await;
        let (status, body) = call(
            &router,
            "GET",
            "/api/agents/available",
            Some((&carol, &carol_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({"agents": [{"alias": "support", "enabled": true}]})
        );
        let (_, body) = call(
            &router,
            "GET",
            "/api/agents/available",
            Some((&root, &root_session)),
            None,
        )
        .await;
        let aliases: Vec<&str> = body["agents"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|agent| agent["alias"].as_str())
            .collect();
        assert!(
            aliases.contains(&"support") && aliases.contains(&"other"),
            "{body}"
        );

        let (status, body) = call(
            &router,
            "GET",
            "/api/status?agent=support",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "agent_alias",
                "locale",
                "model",
                "model_provider",
                "temperature",
                "version"
            ],
            "a scoped status carries the chat's needs and nothing of the host: {body}"
        );
        assert_eq!(body["model"], "test-model");
        let (status, body) = call(
            &router,
            "GET",
            "/api/status?agent=other",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["reason"], "agent_not_allowed");
        let (_, body) = call(
            &router,
            "GET",
            "/api/status",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["locale", "version"], "{body}");
    }

    #[tokio::test]
    async fn chat_is_offered_to_scoped_accounts_only_where_the_core_is_reachable() {
        let fixture = Fixture::new().await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (_, body) = call(
            &fixture.router(),
            "GET",
            "/api/auth/session",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(body["features"]["chat"], true, "{body}");
        assert_eq!(body["features"]["core_sessions"], true, "{body}");
        assert_eq!(body["features"]["operator_pages"], false, "{body}");

        // An account that may open conversations but not prompt or close
        // them sees its core session pages, and no chat.
        let (carol, carol_session) = fixture.sign_in("carol").await;
        let (_, body) = call(
            &fixture.router(),
            "GET",
            "/api/auth/session",
            Some((&carol, &carol_session)),
            None,
        )
        .await;
        assert_eq!(body["features"]["chat"], false, "{body}");
        assert_eq!(body["features"]["core_sessions"], true, "{body}");

        // An administrator keeps the in-process chat.
        let (root, root_session) = fixture.sign_in("root").await;
        let (_, body) = call(
            &fixture.router(),
            "GET",
            "/api/auth/session",
            Some((&root, &root_session)),
            None,
        )
        .await;
        assert_eq!(body["features"]["chat"], true, "{body}");
        assert_eq!(body["features"]["core_sessions"], false, "{body}");
        assert_eq!(body["features"]["operator_pages"], true, "{body}");

        // The same gateway outside the daemon: no connector, no core chat.
        let outside = Router::new()
            .merge(crate::api_oidc::routes())
            .with_state(fixture.state.clone())
            .layer(Extension(CoreRpc::default()))
            .layer(axum::middleware::from_fn_with_state(
                DashboardLayer {
                    state: fixture.state.clone(),
                    auth: Arc::clone(&fixture.auth),
                },
                crate::dashboard_session::layer,
            ));
        let (_, body) = call(
            &outside,
            "GET",
            "/api/auth/session",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(body["features"]["chat"], false, "{body}");
        assert_eq!(body["features"]["core_sessions"], false, "{body}");
    }

    #[tokio::test]
    async fn a_chats_core_connection_is_its_own_and_ends_with_it() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (mut socket, core_id) = chat_in(addr, &alice, "web-1").await;
        let (_second, other_id) = chat_in(addr, &alice, "web-2").await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        let (client, _) = binding.reach(&core_id).expect("the chat is open");
        let (other, _) = binding.reach(&other_id).expect("the chat is open");
        assert_eq!(
            client.handshake().principal_id.as_deref(),
            Some(alice_session.principal().id.as_str()),
            "the core sees the signed-in principal, not the gateway or the operator"
        );
        assert!(!Arc::ptr_eq(&client, &other), "a connection per chat");
        assert_ne!(
            client.handshake().tui_id,
            other.handshake().tui_id,
            "an identity per chat"
        );

        assert!(fixture.auth.end(&alice_session), "sign-out");
        let (_, code) = read_to_close(&mut socket).await;
        assert_eq!(code, Some(crate::dashboard_session::WS_CLOSE_SESSION_ENDED));
        tokio::time::timeout(Duration::from_secs(5), client.closed())
            .await
            .expect("signing out closes the chat's core connection");
        fixture
            .assert_nothing_live("signing out closes every chat")
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while binding.places_taken() != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the chats let go of their places");
        let (status, body) = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    }

    #[tokio::test]
    async fn the_dashboard_connection_is_remote_for_every_local_only_check() {
        let fixture = Fixture::new().await;
        let client = core_connection(&fixture, "root").await;

        // Chunked upload is for local IPC only, whoever the caller is.
        let refused = client
            .request(Method::FileUploadBegin, json!({ "upload_id": "u-1" }))
            .await
            .expect_err("chunked upload is refused");
        let ClientError::Rpc(error) = refused else {
            panic!("an RPC refusal: {refused:?}");
        };
        assert_eq!(error.code, FORBIDDEN, "{error:?}");

        // A path source would have the daemon read a file on its host for a
        // remote browser: refused as on WSS, even for an administrator.
        let opened = client
            .call::<Value>(Method::SessionNew, json!({ "agent_alias": "support" }))
            .await
            .expect("session opens");
        let session_id = opened["session_id"]
            .as_str()
            .expect("the core assigns the id")
            .to_string();
        let host_file = fixture.config.data_dir.join("host-only.txt");
        std::fs::write(&host_file, "daemon host only").unwrap();
        let refused = client
            .request(
                Method::FileAttach,
                json!({
                    "session_id": session_id,
                    "files": [{ "path": host_file.canonicalize().unwrap() }],
                }),
            )
            .await
            .expect_err("a path attachment is refused");
        let ClientError::Rpc(error) = refused else {
            panic!("an RPC refusal: {refused:?}");
        };
        assert!(
            error.message.contains("Path mode is not available"),
            "{error:?}"
        );

        // Renaming a SOP is for local IPC only, checked before its params.
        let refused = client
            .request(Method::SopsRename, json!({}))
            .await
            .expect_err("sops/rename is refused");
        let ClientError::Rpc(error) = refused else {
            panic!("an RPC refusal: {refused:?}");
        };
        assert!(
            error.message.contains("is not served over remote"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn signing_out_closes_the_conversations_and_keeps_their_transcripts() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (_socket, core_id) = chat_in(addr, &alice, "web-1").await;
        assert!(fixture.store.list_ids().await.contains(&core_id));

        assert!(fixture.auth.end(&alice_session), "sign-out");
        assert_closed_not_deleted(&fixture, &core_id).await;
    }

    #[tokio::test]
    async fn the_approach_of_expiry_ends_the_session_and_closes_its_conversations() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let provider = ProviderPin::of(&fixture.config.oidc["corp"]);
        let (alice, alice_session) = fixture
            .auth
            .sign_in("corp", provider, "tok-alice".into(), 16)
            .await
            .expect("sign-in succeeds");
        let (_socket, core_id) = chat_in(addr, &alice, "web-1").await;

        assert_closed_not_deleted(&fixture, &core_id).await;
        assert!(
            alice_session.ended.is_cancelled(),
            "the session ends with them"
        );
    }

    #[tokio::test]
    async fn an_evicted_session_closes_its_conversations() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (_socket, core_id) = chat_in(addr, &alice, "web-1").await;

        // Sessions are aged in whole seconds: make this one strictly the
        // oldest, so it is the one evicted.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        for _ in 0..crate::dashboard_session::MAX_SESSIONS_PER_PRINCIPAL {
            fixture.sign_in("alice").await;
        }
        assert!(
            alice_session.ended.is_cancelled(),
            "alice's oldest session was evicted"
        );
        assert_closed_not_deleted(&fixture, &core_id).await;
    }

    #[tokio::test]
    async fn turning_sign_in_off_closes_every_sessions_conversations() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (_socket, core_id) = chat_in(addr, &alice, "web-1").await;

        // Sign-in is turned off: the first request the gateway serves under
        // the new configuration ends every session.
        let mut off = fixture.config.clone();
        off.gateway.dashboard_sign_in.oidc.clear();
        let off_state = AppState {
            pairing: Arc::clone(&fixture.state.pairing),
            ..crate::api::tests::test_state(off)
        };
        let off_router = Router::new()
            .merge(crate::api_oidc::routes())
            .with_state(off_state.clone())
            .layer(axum::middleware::from_fn_with_state(
                DashboardLayer {
                    state: off_state,
                    auth: Arc::clone(&fixture.auth),
                },
                crate::dashboard_session::layer,
            ));
        let _ = call(&off_router, "GET", "/api/auth/session", None, None).await;
        assert!(alice_session.ended.is_cancelled());
        assert_closed_not_deleted(&fixture, &core_id).await;
    }

    #[tokio::test]
    async fn a_conversation_closed_under_its_socket_says_so() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let (mut socket, core_id) = chat_in(addr, &alice, "web-1").await;
        // The core lets it go under the socket, as an operator's close does.
        assert!(fixture.store.remove(&core_id).await);

        let message = json!({ "type": "message", "content": "still there?" });
        socket
            .send(ClientMessage::Text(message.to_string().into()))
            .await
            .unwrap();
        let (frames, code) = read_to_close(&mut socket).await;
        assert_eq!(code, Some(WS_CLOSE_CONVERSATION_CLOSED), "{frames:?}");
        assert!(
            frames
                .iter()
                .any(|frame| frame["code"] == "CONVERSATION_CLOSED"),
            "{frames:?}"
        );
    }

    #[tokio::test]
    async fn between_turns_a_frame_after_a_lost_grant_closes_the_socket_unacted() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let (mut socket, core_id) = chat_in(addr, &alice, "web-1").await;

        fixture.revoke_group("chatters");
        let message = json!({ "type": "message", "content": "after the revocation" });
        socket
            .send(ClientMessage::Text(message.to_string().into()))
            .await
            .unwrap();
        let (frames, code) = read_to_close(&mut socket).await;
        assert_eq!(
            code,
            Some(crate::dashboard_session::WS_CLOSE_SESSION_ENDED),
            "{frames:?}"
        );
        let durable =
            zeroclaw_infra::session_sqlite::SqliteSessionBackend::new(&fixture.config.data_dir)
                .expect("sqlite session backend");
        let transcript = durable.load(&format!("rpc_{core_id}"));
        assert!(
            !transcript
                .iter()
                .any(|message| message.content.contains("after the revocation")),
            "the frame never reached the conversation: {transcript:?}"
        );
    }

    #[tokio::test]
    async fn mid_turn_a_frame_after_a_lost_grant_closes_the_socket_unacted() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut socket = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = socket.next().await;
        let slow = json!({ "type": "message", "content": "slow please" });
        socket
            .send(ClientMessage::Text(slow.to_string().into()))
            .await
            .unwrap();
        // The provider holds the turn: it is in flight once the request
        // has reached it.
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let reached = fixture
                    ._model
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|request| String::from_utf8_lossy(&request.body).contains("slow please"));
                if reached {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the turn reaches the provider");

        fixture.revoke_group("chatters");
        let message = json!({ "type": "message", "content": "mid-turn" });
        socket
            .send(ClientMessage::Text(message.to_string().into()))
            .await
            .unwrap();
        let (frames, code) = read_to_close(&mut socket).await;
        assert_eq!(
            code,
            Some(crate::dashboard_session::WS_CLOSE_SESSION_ENDED),
            "{frames:?}"
        );
        assert!(
            !frames
                .iter()
                .any(|frame| frame["type"] == "done" || frame["code"] == "CAPABILITY_MISSING"),
            "closed mid-turn, before acting on the frame: {frames:?}"
        );
    }

    // ── Review round 1 (astra) on part 2 ──────────────────────────────

    /// Open the chat for each of `ids` at once, holding every `session/new`
    /// in the core's start hook until all have had the chance to reach it.
    async fn open_concurrently(
        fixture: &Fixture,
        addr: SocketAddr,
        cookie: &str,
        ids: &[String],
    ) -> Vec<ChatSocket> {
        fixture.start_gate.arm();
        let opens: Vec<_> = ids
            .iter()
            .map(|id| {
                let (cookie, id) = (cookie.to_string(), id.clone());
                zeroclaw_spawn::spawn!(
                    async move { open_chat(addr, &cookie, "support", &id).await }
                )
            })
            .collect();
        fixture.start_gate.wait_held(1).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        fixture.start_gate.release();
        let mut sockets = Vec::new();
        for open in opens {
            if let Ok(Ok(socket)) = open.await {
                sockets.push(socket);
            }
        }
        sockets
    }

    #[tokio::test]
    async fn concurrent_opens_never_hold_more_than_the_cap_and_lose_none() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let ids: Vec<String> = (1..=MAX_LIVE_CONVERSATIONS + 2)
            .map(|n| format!("web-{n}"))
            .collect();
        let sockets = open_concurrently(&fixture, addr, &alice, &ids).await;

        let live = fixture.store.list_ids().await;
        assert!(
            live.len() <= MAX_LIVE_CONVERSATIONS,
            "{} live conversations: {live:?}",
            live.len()
        );
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        for id in &live {
            assert!(binding.reach(id).is_some(), "{id} is live but untracked");
        }
        drop(sockets);
        assert!(fixture.auth.end(&alice_session), "sign-out");
        fixture
            .assert_nothing_live("signing out closes every conversation")
            .await;
    }

    #[tokio::test]
    async fn a_create_only_account_cannot_fill_the_core_by_signing_in_again() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        // carol may open conversations but not update or close them. More
        // rounds than the core holds conversations.
        for round in 0..(64 / MAX_LIVE_CONVERSATIONS + 1) {
            let (carol, carol_session) = fixture.sign_in("carol").await;
            let mut sockets = Vec::new();
            for n in 1..=MAX_LIVE_CONVERSATIONS {
                // Refused, or opened and closed again at sign-out: either way
                // nothing may be left live.
                if let Ok(mut socket) =
                    open_chat(addr, &carol, "support", &format!("web-{n}")).await
                {
                    let _ = socket.next().await;
                    sockets.push(socket);
                }
            }
            drop(sockets);
            assert!(fixture.auth.end(&carol_session));
            fixture
                .assert_nothing_live(&format!("round {round}: signing out closes them"))
                .await;
        }
    }

    #[tokio::test]
    async fn signing_out_after_the_core_wants_revalidation_still_closes() {
        let fixture = Fixture::with_config(|config| {
            if let Some(corp) = config.oidc.get_mut("corp") {
                corp.revalidation_secs = 2;
            }
        })
        .await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let _chat = chat_in(addr, &alice, "web-1").await;
        // The core's revalidation deadline for the connection passes.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(fixture.auth.end(&alice_session), "sign-out");
        fixture
            .assert_nothing_live("the close survives a stale core binding")
            .await;
    }

    #[tokio::test]
    async fn a_revoked_account_leaves_nothing_live() {
        // A revoked account's own authority can't close its conversations:
        // the core retires them once their client has stayed away.
        let fixture = Fixture::with_config(|config| {
            config.wss.orphaned_session_timeout_secs = 1;
        })
        .await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let _chat = chat_in(addr, &alice, "web-1").await;
        fixture.revoke_group_everywhere("chatters");
        // The next request finds the session revoked and ends it.
        let _ = call(
            &router,
            "GET",
            "/api/sessions",
            Some((&alice, &alice_session)),
            None,
        )
        .await;
        assert!(
            alice_session.ended.is_cancelled(),
            "the revoked session ended"
        );
        fixture
            .assert_nothing_live("a revoked account's conversations are closed")
            .await;
    }

    #[tokio::test]
    async fn a_lost_core_connection_is_renewed_and_still_closes() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (mut socket, core_id) = chat_in(addr, &alice, "web-1").await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        let lose = |binding: &CoreBinding| {
            let (client, _) = binding.reach(&core_id).expect("the chat is open");
            client.shutdown();
            client
        };

        // Between turns: the next turn renews the connection with the chat's
        // identity, and the conversation is still its own.
        let lost = lose(&binding);
        tokio::time::timeout(Duration::from_secs(5), lost.closed())
            .await
            .expect("the core connection closes");
        assert_eq!(chat_once(&mut socket, "still there").await["type"], "done");
        assert_eq!(fixture.store.list_ids().await, vec![core_id.clone()]);

        // At sign-out: the close renews it too.
        let lost = lose(&binding);
        tokio::time::timeout(Duration::from_secs(5), lost.closed())
            .await
            .expect("the core connection closes");
        assert!(fixture.auth.end(&alice_session), "sign-out");
        fixture
            .assert_nothing_live("the close reconnects with the chat's identity")
            .await;
    }

    #[tokio::test]
    async fn a_cancelled_open_leaves_no_untracked_live_conversation() {
        let fixture = Fixture::new().await;
        let (_, alice_session) = fixture.sign_in("alice").await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        let caller = CoreCaller {
            core: fixture.core.clone(),
            auth: Arc::clone(&fixture.auth),
            session: Arc::clone(&alice_session),
            binding: Arc::clone(&binding),
        };
        // The open itself goes away while the core creates: nobody takes
        // what the core creates, so its creation closes it.
        fixture.start_gate.arm();
        let cancelled =
            tokio::time::timeout(Duration::from_secs(1), caller.open_chat("support", None)).await;
        assert!(cancelled.is_err(), "the open was still waiting on the core");
        assert_eq!(fixture.start_gate.held(), 1, "the core created the session");
        fixture.start_gate.release();
        fixture
            .assert_nothing_live("a conversation nobody took is closed")
            .await;
        assert_eq!(binding.places_taken(), 0, "the open let go of its place");
    }

    /// A chat's connections present the session's token only to the provider
    /// it was issued by. When the provider by that name is repointed at
    /// another issuer (A to B), no chat sends the token to B: not a renewal,
    /// not a close that must reconnect, not a new chat.
    #[tokio::test]
    async fn a_chat_never_sends_its_token_to_a_repointed_provider() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (mut renewing, renewing_id) = chat_in(addr, &alice, "web-1").await;
        let (_closing, closing_id) = chat_in(addr, &alice, "web-2").await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");

        // The core's policy repoints `corp` at B, which would accept the
        // token. The gateway's still names A, so only the chats' own
        // connections can reach B.
        let other = idp().await;
        let mut repointed = fixture.config.clone();
        repointed
            .oidc
            .get_mut("corp")
            .expect("the fixture's provider")
            .issuer = other.uri();
        fixture
            .core_auth
            .publish_accepted(&repointed, fixture.core_auth.accepted_revision() + 1)
            .expect("the core accepts the repointed policy");

        // Each chat's connection drops, so its next one presents the token.
        for core_id in [&renewing_id, &closing_id] {
            let (client, _) = binding.reach(core_id).expect("the chat is open");
            client.shutdown();
            tokio::time::timeout(Duration::from_secs(5), client.closed())
                .await
                .expect("the connection closes");
        }
        // A renewal, for the next turn.
        let again = json!({ "type": "message", "content": "again" });
        renewing
            .send(ClientMessage::Text(again.to_string().into()))
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(15), async {
            while let Some(Ok(frame)) = renewing.next().await {
                match frame {
                    ClientMessage::Text(text) if text.contains("\"done\"") => break,
                    ClientMessage::Text(text) if text.contains("\"error\"") => break,
                    ClientMessage::Close(_) => break,
                    _ => {}
                }
            }
        })
        .await;
        // A close that must reconnect, at the session's end.
        fixture.auth.end(&alice_session);
        tokio::time::sleep(Duration::from_secs(2)).await;
        // A new chat, from another sign-in.
        let (alice_again, _) = fixture.sign_in("alice").await;
        let opened = open_chat(addr, &alice_again, "support", "web-3").await;

        let sent_to_other = other
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| String::from_utf8_lossy(&request.body).contains("token=tok-alice"))
            .count();
        assert_eq!(sent_to_other, 0, "the token went to the repointed provider");
        assert!(
            opened.is_err(),
            "a chat whose provider was repointed is refused"
        );
    }

    #[tokio::test]
    async fn a_chat_dropped_without_closing_still_closes_its_conversation() {
        let fixture = Fixture::new().await;
        let (_, alice_session) = fixture.sign_in("alice").await;
        let caller = CoreCaller {
            core: fixture.core.clone(),
            auth: Arc::clone(&fixture.auth),
            session: Arc::clone(&alice_session),
            binding: fixture
                .auth
                .core_binding(&alice_session)
                .expect("signed in"),
        };
        let chat = caller.open_chat("support", None).await.expect("opens");
        assert!(fixture.store.list_ids().await.contains(&chat.core_id));
        drop(chat);
        fixture
            .assert_nothing_live("a chat dropped without closing closes its conversation")
            .await;
    }

    /// Whichever goes first, the server noticing that the browser left or the
    /// core answering, closes the conversation: the creation task when nobody
    /// takes it, or the chat's drop when the upgrade fails. Each path has a
    /// test of its own above.
    #[tokio::test]
    async fn a_socket_that_never_opens_closes_its_conversation() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        // The browser gives up before the upgrade: the chat handed out is
        // dropped unused, which closes its conversation.
        fixture.start_gate.arm();
        let given_up = tokio::time::timeout(
            Duration::from_secs(1),
            open_chat(addr, &alice, "support", "web-1"),
        )
        .await;
        assert!(given_up.is_err(), "the open was still waiting on the core");
        assert_eq!(fixture.start_gate.held(), 1, "the core created the session");
        fixture.start_gate.release();
        fixture
            .assert_nothing_live("a conversation whose socket never opened is closed")
            .await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        tokio::time::timeout(Duration::from_secs(5), async {
            while binding.places_taken() != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the chat let go of its place");
    }

    #[tokio::test]
    async fn another_account_cannot_reach_a_conversation_by_its_real_ids() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let (mut alice_socket, core_id) = chat_in(addr, &alice, "alice-chat").await;
        let (bob, bob_session) = fixture.sign_in("bob").await;
        // Alice signed in a second time: the same principal, another session.
        let (alice_again, alice_again_session) = fixture.sign_in("alice").await;

        for (who, name, cookie, session) in [
            ("bob", "bob", &bob, &bob_session),
            (
                "alice's other session",
                "alice",
                &alice_again,
                &alice_again_session,
            ),
        ] {
            let (status, body) = call(
                &router,
                "GET",
                "/api/sessions",
                Some((cookie, session)),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{who}: {body}");
            assert!(listed(&body).is_empty(), "{who} lists nothing: {body}");
            for (method, path) in [
                ("GET", format!("/api/sessions/{core_id}/messages")),
                ("PUT", format!("/api/sessions/{core_id}")),
                ("DELETE", format!("/api/sessions/{core_id}")),
            ] {
                let (status, body) = call(
                    &router,
                    method,
                    &path,
                    Some((cookie, session)),
                    Some(json!({ "name": "taken" })),
                )
                .await;
                assert_ne!(status, StatusCode::OK, "{who}: {method} {path}: {body}");
                assert!(!body.to_string().contains("remember me"), "{who}: {body}");
            }
            let (_, body) = call(
                &router,
                "POST",
                &format!("/api/sessions/{core_id}/abort"),
                Some((cookie, session)),
                None,
            )
            .await;
            assert_ne!(body["status"], "aborted", "{who} aborts: {body}");

            // On a core connection of their own, under another TUI identity,
            // the core refuses every call on alice's conversation. The
            // destructive ones come last.
            let theirs = core_connection(&fixture, name).await;
            let on_it = json!({ "session_id": core_id });
            for (method, params) in [
                (Method::SessionMessages, on_it.clone()),
                (
                    Method::SessionApprove,
                    json!({ "session_id": core_id, "request_id": "any", "decision": "allow_once" }),
                ),
                (
                    Method::SessionRename,
                    json!({ "session_id": core_id, "name": "taken" }),
                ),
                (
                    Method::SessionNew,
                    json!({ "session_id": core_id, "agent_alias": "support" }),
                ),
                (Method::SessionDelete, on_it.clone()),
                (Method::SessionClose, on_it.clone()),
            ] {
                let answer = theirs.call::<Value>(method, params).await;
                assert!(answer.is_err(), "{who}: {method:?} on alice's: {answer:?}");
            }
            theirs.shutdown();

            // A chat opened with alice's id as the dashboard's is one's own.
            let mut socket = open_chat(addr, cookie, "support", &core_id)
                .await
                .expect("may open a chat");
            let start = await_frame(&mut socket, "session_start").await;
            assert_eq!(start["resumed"], false, "{who} resumed alice's: {start}");
            assert_ne!(start["session_id"], core_id.as_str(), "{who}: {start}");
        }

        // Alice's conversation is untouched.
        assert!(fixture.store.list_ids().await.contains(&core_id));
        assert_eq!(
            chat_once(&mut alice_socket, "still mine").await["type"],
            "done"
        );
    }

    // ── Review round 1 (Fable) on part 2 ──────────────────────────────

    #[tokio::test]
    async fn an_administrator_session_does_not_end_early_for_the_agent_list() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let provider = ProviderPin::of(&fixture.config.oidc["corp"]);
        let (root, root_session) = fixture
            .auth
            .sign_in("corp", provider, "tok-root".into(), 16)
            .await
            .expect("sign-in succeeds");
        let (status, body) = call(
            &router,
            "GET",
            "/api/agents/available",
            Some((&root, &root_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // Past the point where a scoped session would end early.
        tokio::time::sleep(Duration::from_secs(8)).await;
        assert!(
            !root_session.ended.is_cancelled(),
            "an administrator holds no core conversation, so nothing ends it early"
        );
    }

    #[tokio::test]
    async fn an_account_without_delete_still_stops_its_own_turn() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (dave, dave_session) = fixture.sign_in("dave").await;
        let mut socket = open_chat(addr, &dave, "support", "web-1")
            .await
            .expect("dave may chat");
        let start = await_frame(&mut socket, "session_start").await;
        let core_id = start["session_id"]
            .as_str()
            .expect("the core's id")
            .to_string();
        let slow = json!({ "type": "message", "content": "slow please" });
        socket
            .send(ClientMessage::Text(slow.to_string().into()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let reached = fixture
                    ._model
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|request| String::from_utf8_lossy(&request.body).contains("slow please"));
                if reached {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the turn reaches the provider");

        // `session/abort` needs Delete; the fallback, `session/cancel` on the
        // connection that started the turn, needs Update only.
        let (status, body) = call(
            &router,
            "POST",
            &format!("/api/sessions/{core_id}/abort"),
            Some((&dave, &dave_session)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "aborted", "{body}");
    }

    // ── Review round 2 (astra) on part 2 ──────────────────────────────

    /// Wait until the model provider has a turn whose prompt holds `text`.
    async fn provider_received(fixture: &Fixture, text: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let reached = fixture
                    ._model
                    .received_requests()
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|request| String::from_utf8_lossy(&request.body).contains(text));
                if reached {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the turn reaches the provider");
    }

    /// A request admitted just before sign-out reaches the handler after it,
    /// when the session's own binding is gone: the chat it opens still closes
    /// its conversation, which never depends on a binding or a sweep.
    #[tokio::test]
    async fn a_chat_opened_across_a_sign_out_still_closes_its_conversation() {
        let fixture = Fixture::new().await;
        let (_, alice_session) = fixture.sign_in("alice").await;
        let caller = CoreCaller {
            core: fixture.core.clone(),
            auth: Arc::clone(&fixture.auth),
            session: Arc::clone(&alice_session),
            binding: Arc::default(),
        };

        // Sign-out lands while the core creates the conversation.
        fixture.start_gate.arm();
        let opening = {
            let caller = caller.clone();
            zeroclaw_spawn::spawn!(async move {
                caller
                    .open_chat("support", None)
                    .await
                    .map(|chat| chat.core_id.clone())
            })
        };
        fixture.start_gate.wait_held(1).await;
        assert!(fixture.auth.end(&alice_session), "sign-out");
        fixture.start_gate.release();
        let opened = opening.await.expect("the open joins");
        assert!(
            matches!(opened, Err(CoreFailure::SessionEnded)),
            "no chat is handed out for an ended session: {opened:?}"
        );
        fixture
            .assert_nothing_live("the conversation created across the sign-out is closed")
            .await;

        // Sign-out lands before the handler: there is no token left to open
        // anything with.
        let refused = caller.open_chat("support", None).await;
        assert!(
            matches!(refused, Err(CoreFailure::SessionEnded)),
            "{:?}",
            refused.map(|chat| chat.core_id.clone())
        );
        assert_eq!(
            fixture.start_gate.seen(),
            1,
            "nothing more reached the core"
        );
    }

    #[tokio::test]
    async fn signing_out_during_an_open_leaves_nothing_live() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        fixture.start_gate.arm();
        let cookie = alice.clone();
        let opening = zeroclaw_spawn::spawn!(async move {
            let _ = open_chat(addr, &cookie, "support", "web-1").await;
        });
        fixture.start_gate.wait_held(1).await;
        assert!(
            fixture.auth.end(&alice_session),
            "sign-out while the core creates"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
        fixture.start_gate.release();
        let _ = opening.await;
        fixture
            .assert_nothing_live("a conversation created during sign-out is closed")
            .await;
    }

    #[tokio::test]
    async fn a_refused_handshake_never_takes_another_identity() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (mut socket, core_id) = chat_in(addr, &alice, "web-1").await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        let (client, _) = binding.reach(&core_id).expect("the chat is open");
        // The identity provider turns the token down once. Under an unchanged
        // policy that ends the session; it never takes another identity.
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .and(body_string_contains("token=tok-alice"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "active": false })))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&fixture._idp)
            .await;
        // The chat needs a new connection for its next turn.
        client.shutdown();
        tokio::time::timeout(Duration::from_secs(5), client.closed())
            .await
            .expect("the core connection closes");

        let again = json!({ "type": "message", "content": "again" });
        socket
            .send(ClientMessage::Text(again.to_string().into()))
            .await
            .unwrap();
        let (frames, code) = read_to_close(&mut socket).await;
        assert_eq!(
            code,
            Some(crate::dashboard_session::WS_CLOSE_SESSION_ENDED),
            "{frames:?}"
        );
        assert!(
            alice_session.ended.is_cancelled(),
            "the refusal ends the session"
        );
        // The close reconnects under the chat's own identity, the only one the
        // core lets close the conversation.
        fixture
            .assert_nothing_live("the conversation is closed under the chat's identity")
            .await;
    }

    #[tokio::test]
    async fn chat_needs_reading_its_own_history_too() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (erin, erin_session) = fixture.sign_in("erin").await;
        let (_, body) = call(
            &fixture.router(),
            "GET",
            "/api/auth/session",
            Some((&erin, &erin_session)),
            None,
        )
        .await;
        assert_eq!(body["features"]["chat"], false, "{body}");
        assert!(
            open_chat(addr, &erin, "support", "web-1").await.is_err(),
            "the chat route refuses an account that cannot read its history"
        );
    }

    #[tokio::test]
    async fn another_account_cannot_stop_a_running_turn_by_its_real_id() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut socket = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let start = await_frame(&mut socket, "session_start").await;
        let core_id = start["session_id"]
            .as_str()
            .expect("the core's id")
            .to_string();
        let slow = json!({ "type": "message", "content": "slow please" });
        socket
            .send(ClientMessage::Text(slow.to_string().into()))
            .await
            .unwrap();
        provider_received(&fixture, "slow please").await;

        let (bob, bob_session) = fixture.sign_in("bob").await;
        let (alice_again, alice_again_session) = fixture.sign_in("alice").await;
        for (who, name, cookie, session) in [
            ("bob", "bob", &bob, &bob_session),
            (
                "alice's other session",
                "alice",
                &alice_again,
                &alice_again_session,
            ),
        ] {
            let (_, body) = call(
                &router,
                "POST",
                &format!("/api/sessions/{core_id}/abort"),
                Some((cookie, session)),
                None,
            )
            .await;
            assert_ne!(
                body["status"], "aborted",
                "{who} stopped alice's turn: {body}"
            );
            // Directly, on a core connection of their own.
            let theirs = core_connection(&fixture, name).await;
            for method in [Method::SessionAbort, Method::SessionCancel] {
                let answer = theirs
                    .call::<SessionCancelResult>(method, json!({ "session_id": core_id }))
                    .await;
                assert!(
                    !answer.as_ref().is_ok_and(|result| result.cancelled),
                    "{who}: {method:?} stopped alice's turn: {answer:?}"
                );
            }
            theirs.shutdown();
        }
        let end = turn_end(&mut socket).await;
        assert_eq!(end["type"], "done", "alice's turn ran to its end: {end}");
    }

    /// Read text frames until the turn ends, and return the frame that ends
    /// it.
    async fn turn_end(socket: &mut ChatSocket) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let frame = socket.next().await.expect("socket open").expect("frame");
                let ClientMessage::Text(text) = frame else {
                    continue;
                };
                let frame: Value = serde_json::from_str(&text).unwrap();
                if matches!(frame["type"].as_str(), Some("done" | "error" | "aborted")) {
                    return frame;
                }
            }
        })
        .await
        .expect("the turn ends")
    }

    #[tokio::test]
    async fn another_account_cannot_answer_or_stop_a_victims_pending_approval() {
        // A scoped account runs only the tools it is granted, and this one
        // asks before it runs.
        let fixture = Fixture::with_config(asking_file_read).await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut socket = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let start = await_frame(&mut socket, "session_start").await;
        let core_id = start["session_id"]
            .as_str()
            .expect("the core's id")
            .to_string();
        let request_id = ask_for_approval(&mut socket).await;

        let (bob, bob_session) = fixture.sign_in("bob").await;
        let (alice_again, alice_again_session) = fixture.sign_in("alice").await;
        for (who, name, cookie, session) in [
            ("bob", "bob", &bob, &bob_session),
            (
                "alice's other session",
                "alice",
                &alice_again,
                &alice_again_session,
            ),
        ] {
            let theirs = core_connection(&fixture, name).await;
            let answered = theirs
                .call::<Value>(
                    Method::SessionApprove,
                    json!({
                        "session_id": core_id,
                        "request_id": request_id,
                        "decision": "allow_once",
                    }),
                )
                .await;
            assert!(
                answered.is_err(),
                "{who} answered alice's approval: {answered:?}"
            );
            let stopped = theirs
                .call::<SessionCancelResult>(Method::SessionAbort, json!({ "session_id": core_id }))
                .await;
            assert!(
                !stopped.as_ref().is_ok_and(|result| result.cancelled),
                "{who} stopped alice's turn: {stopped:?}"
            );
            theirs.shutdown();
            let (_, body) = call(
                &router,
                "POST",
                &format!("/api/sessions/{core_id}/abort"),
                Some((cookie, session)),
                None,
            )
            .await;
            assert_ne!(
                body["status"], "aborted",
                "{who} stopped alice's turn: {body}"
            );
        }
        // Nothing they did resolved it: the tool has not run, so the model
        // has had no second request for this turn.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let asked = fixture
            ._model
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| {
                String::from_utf8_lossy(&request.body).contains("please read the note")
            })
            .count();
        assert_eq!(asked, 1, "the approval is still alice's to answer");

        // Alice answers it herself, and her turn ends.
        let end = deny_and_finish(&mut socket, &request_id).await;
        assert_eq!(end["type"], "done", "{end}");
    }

    // ── One conversation per chat socket (D-020) ─────────────────────────

    /// Read text frames until one of type `wanted`. A turn that ends first
    /// fails the test with what came before; keepalive pings are skipped.
    async fn await_frame(socket: &mut ChatSocket, wanted: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut seen = Vec::new();
            loop {
                let frame = socket.next().await.expect("socket open").expect("frame");
                let ClientMessage::Text(text) = frame else {
                    continue;
                };
                let frame: Value = serde_json::from_str(&text).unwrap();
                if frame["type"] == wanted {
                    return frame;
                }
                assert!(
                    !matches!(frame["type"].as_str(), Some("done" | "error" | "aborted")),
                    "the turn ended before `{wanted}`: {seen:?} then {frame}"
                );
                seen.push(frame);
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no `{wanted}` frame within 30 s"))
    }

    /// Send a message whose turn calls `file_read`, and return the approval
    /// request it raises on this socket.
    async fn ask_for_approval(socket: &mut ChatSocket) -> String {
        let ask = json!({ "type": "message", "content": "please read the note" });
        socket
            .send(ClientMessage::Text(ask.to_string().into()))
            .await
            .unwrap();
        await_frame(socket, "approval_request").await["request_id"]
            .as_str()
            .expect("the request names itself")
            .to_string()
    }

    /// Deny `request_id` and return the frame that ends the turn.
    async fn deny_and_finish(socket: &mut ChatSocket, request_id: &str) -> Value {
        let deny =
            json!({ "type": "approval_response", "request_id": request_id, "decision": "deny" });
        socket
            .send(ClientMessage::Text(deny.to_string().into()))
            .await
            .unwrap();
        turn_end(socket).await
    }

    /// The fixture with `file_read` granted to the chat profile and asked
    /// for before it runs, so a turn can be parked on an approval.
    fn asking_file_read(config: &mut Config) {
        let chatters = config
            .permission_profiles
            .get_mut("chat-user")
            .expect("the fixture's chat profile");
        chatters.allowed_tools = vec!["file_read".to_string()];
        chatters.grants.insert(Resource::Tools, vec![Verb::Execute]);
        config
            .risk_profiles
            .get_mut("test-profile")
            .expect("the fixture's risk profile")
            .always_ask = vec!["file_read".to_string()];
    }

    #[tokio::test]
    async fn a_conversation_ends_with_its_socket() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut socket = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = socket.next().await;
        assert_eq!(chat_once(&mut socket, "remember me").await["type"], "done");
        let live = fixture.store.list_ids().await;
        assert_eq!(live.len(), 1, "{live:?}");

        drop(socket);
        assert_closed_not_deleted(&fixture, &live[0]).await;
    }

    #[tokio::test]
    async fn every_socket_starts_a_conversation_of_its_own() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut first = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = first.next().await;
        assert_eq!(chat_once(&mut first, "remember me").await["type"], "done");
        let before = fixture.store.list_ids().await;

        // The same dashboard id again, while the first socket is open.
        let mut second = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let start = await_frame(&mut second, "session_start").await;
        assert_eq!(start["resumed"], false, "{start}");
        assert_eq!(start["message_count"], 0, "{start}");
        assert_eq!(chat_once(&mut second, "hi").await["type"], "done");
        let after = fixture.store.list_ids().await;
        assert_eq!(after.len(), 2, "a conversation per socket: {after:?}");
        assert!(after.contains(&before[0]), "{after:?}");
        assert_ne!(start["session_id"], before[0].as_str(), "{start}");
    }

    #[tokio::test]
    async fn a_fifth_socket_is_refused_and_closes_no_other_tab() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut tabs = Vec::new();
        for n in 1..=MAX_LIVE_CONVERSATIONS {
            let mut socket = open_chat(addr, &alice, "support", &format!("web-{n}"))
                .await
                .expect("alice may chat");
            let _ = socket.next().await;
            assert_eq!(chat_once(&mut socket, "hello").await["type"], "done");
            tabs.push(socket);
        }
        let before = fixture.store.list_ids().await;
        assert_eq!(before.len(), MAX_LIVE_CONVERSATIONS);

        let mut fifth = open_chat(addr, &alice, "support", "web-5")
            .await
            .expect("the upgrade is accepted, to say why");
        let (frames, code) = read_to_close(&mut fifth).await;
        assert_eq!(code, Some(WS_CLOSE_CONVERSATION_LIMIT));
        assert!(
            frames
                .iter()
                .any(|frame| frame["code"] == "CONVERSATION_LIMIT"),
            "{frames:?}"
        );
        let mut after = fixture.store.list_ids().await;
        let mut expected = before;
        after.sort();
        expected.sort();
        assert_eq!(after, expected, "no other tab's conversation was closed");
        assert_eq!(
            chat_once(&mut tabs[0], "still here").await["type"],
            "done",
            "the oldest tab still chats"
        );
    }

    #[tokio::test]
    #[ignore = "slow, and run alone: the core holds the process-wide config writer through \
                session construction, MCP start-up included, so for its minute every other \
                test's session/new in this binary waits; run with --ignored"]
    async fn a_creation_still_running_at_sign_out_is_closed_when_it_lands() {
        // Two MCP servers that never answer: the core waits out each one's
        // initialization (30 s) before it publishes the conversation, longer
        // than one request to the core may take, and than the connection's
        // revalidation window, so the close needs a new handshake.
        let fixture = Fixture::with_config(|config| {
            use zeroclaw_config::schema::{McpBundleConfig, McpServerConfig, McpTransport};
            config.mcp.enabled = true;
            config.mcp.deferred_loading = false;
            config.mcp.servers = ["silent-1", "silent-2"]
                .into_iter()
                .map(|name| McpServerConfig {
                    name: name.into(),
                    transport: McpTransport::Stdio,
                    command: "sleep".into(),
                    args: vec!["300".into()],
                    ..Default::default()
                })
                .collect();
            config.mcp_bundles.insert(
                "silent".into(),
                McpBundleConfig {
                    servers: vec!["silent-1".into(), "silent-2".into()],
                    exclude: vec![],
                },
            );
            config
                .agents
                .get_mut("support")
                .expect("the fixture's agent")
                .mcp_bundles = vec!["silent".into()];
            let chatters = config
                .permission_profiles
                .get_mut("chat-user")
                .expect("the fixture's chat profile");
            chatters.allowed_tools = vec!["*".to_string()];
            chatters.grants.insert(Resource::Tools, vec![Verb::Execute]);
        })
        .await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let started = std::time::Instant::now();
        let cookie = alice.clone();
        let opening = zeroclaw_spawn::spawn!(async move {
            let _ = open_chat(addr, &cookie, "support", "web-1").await;
        });
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(fixture.auth.end(&alice_session), "sign-out mid-creation");

        // The core publishes the conversation only now.
        tokio::time::timeout(Duration::from_secs(150), async {
            while fixture.start_gate.seen() == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the core finishes the conversation");
        assert!(
            started.elapsed() > Duration::from_secs(45),
            "the creation outlasted a request's deadline: {:?}",
            started.elapsed()
        );
        fixture
            .assert_nothing_live("a conversation that lands after sign-out is closed")
            .await;
        let _ = opening.await;
    }

    #[tokio::test]
    async fn a_second_tab_on_another_agent_does_not_stall_the_first() {
        let fixture = Fixture::with_config(|config| {
            asking_file_read(config);
            config
                .permission_profiles
                .get_mut("chat-user")
                .expect("the fixture's chat profile")
                .allowed_agents
                .push("other".into());
        })
        .await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut first = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = first.next().await;
        let request_id = ask_for_approval(&mut first).await;

        // The same dashboard id with another agent, while the first turn
        // waits on its approval.
        let second = tokio::time::timeout(
            Duration::from_secs(10),
            open_chat(addr, &alice, "other", "web-1"),
        )
        .await
        .expect("the second tab opens without waiting on the first's turn")
        .expect("alice may chat with other");
        let end = tokio::time::timeout(
            Duration::from_secs(10),
            deny_and_finish(&mut first, &request_id),
        )
        .await
        .expect("the first tab's approval is answered at once");
        assert_eq!(end["type"], "done", "{end}");
        drop(second);
    }

    #[tokio::test]
    async fn each_tabs_approval_reaches_that_tab() {
        let fixture = Fixture::with_config(asking_file_read).await;
        let addr = serve(fixture.router()).await;
        let (alice, _) = fixture.sign_in("alice").await;
        let mut first = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = first.next().await;
        let first_request = ask_for_approval(&mut first).await;

        let mut second = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = second.next().await;
        let second_request = ask_for_approval(&mut second).await;
        assert_ne!(first_request, second_request);
        assert_eq!(
            deny_and_finish(&mut second, &second_request).await["type"],
            "done"
        );
        assert_eq!(
            deny_and_finish(&mut first, &first_request).await["type"],
            "done"
        );
    }

    #[tokio::test]
    async fn a_revalidation_mid_conversation_keeps_it_and_its_approvals() {
        let fixture = Fixture::with_config(|config| {
            asking_file_read(config);
            if let Some(corp) = config.oidc.get_mut("corp") {
                corp.revalidation_secs = 2;
            }
        })
        .await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let mut socket = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = socket.next().await;
        assert_eq!(chat_once(&mut socket, "hello").await["type"], "done");
        let live = fixture.store.list_ids().await;

        // Past the core's revalidation deadline before the turn, and again
        // while its approval waits.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let request_id = ask_for_approval(&mut socket).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let end = deny_and_finish(&mut socket, &request_id).await;
        assert_eq!(end["type"], "done", "{end}");
        assert_eq!(
            fixture.store.list_ids().await,
            live,
            "the same conversation, and only it"
        );
        assert!(!alice_session.ended.is_cancelled(), "still signed in");
    }

    #[tokio::test]
    async fn the_session_list_shows_only_open_sockets_and_offers_no_reopening() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let addr = serve(router.clone()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let mut socket = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let start = await_frame(&mut socket, "session_start").await;
        assert_eq!(chat_once(&mut socket, "remember me").await["type"], "done");
        let core_id = fixture.store.list_ids().await[0].clone();
        assert_eq!(
            start["session_id"],
            core_id.as_str(),
            "the core's id: {start}"
        );

        let alice_call = Some((alice.as_str(), alice_session.as_ref()));
        let (status, body) = call(&router, "GET", "/api/sessions", alice_call, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(listed(&body), vec![core_id.clone()], "{body}");
        for (method, path) in [
            ("GET", format!("/api/sessions/{core_id}/messages")),
            ("PUT", format!("/api/sessions/{core_id}")),
            ("DELETE", format!("/api/sessions/{core_id}")),
        ] {
            let (status, body) = call(
                &router,
                method,
                &path,
                alice_call,
                Some(json!({ "name": "x" })),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}: {body}");
        }

        drop(socket);
        fixture.assert_nothing_live("the socket closed").await;
        let (_, body) = call(&router, "GET", "/api/sessions", alice_call, None).await;
        assert!(listed(&body).is_empty(), "{body}");
    }

    // ── S review round 1 (astra) ─────────────────────────────────────────

    /// A caller for `session`, as the chat route builds one.
    fn core_caller_for(fixture: &Fixture, session: &Arc<SessionAuth>) -> CoreCaller {
        CoreCaller {
            core: fixture.core.clone(),
            auth: Arc::clone(&fixture.auth),
            session: Arc::clone(session),
            binding: fixture.auth.core_binding(session).expect("signed in"),
        }
    }

    #[tokio::test]
    async fn a_chat_renews_across_a_policy_that_keeps_its_provider() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let (mut socket, core_id) = chat_in(addr, &alice, "web-1").await;
        // A publish that leaves `corp` as it was.
        fixture
            .core_auth
            .publish_accepted(&fixture.config, fixture.core_auth.accepted_revision() + 1)
            .expect("the core accepts the policy");
        // The chat's connection drops; its next turn renews it under the new
        // policy, bound to the same provider.
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        let (client, _) = binding.reach(&core_id).expect("the chat is open");
        client.shutdown();
        tokio::time::timeout(Duration::from_secs(5), client.closed())
            .await
            .expect("the connection closes");
        assert_eq!(chat_once(&mut socket, "still here").await["type"], "done");
        assert_eq!(fixture.store.list_ids().await, vec![core_id]);
    }

    #[tokio::test]
    async fn cancelled_opens_keep_their_places_until_their_creations_are_closed() {
        let fixture = Fixture::new().await;
        let (_, alice_session) = fixture.sign_in("alice").await;
        let caller = core_caller_for(&fixture, &alice_session);
        // Four opens wait on the core, and their callers give up, as the
        // request timeout in front of the chat route does.
        fixture.start_gate.arm();
        for _ in 0..MAX_LIVE_CONVERSATIONS {
            let cancelled = tokio::time::timeout(
                Duration::from_millis(500),
                caller.open_chat("support", None),
            )
            .await;
            assert!(cancelled.is_err(), "the open was still waiting on the core");
        }
        assert_eq!(
            caller.binding.places_taken(),
            MAX_LIVE_CONVERSATIONS,
            "the creations still under way keep their places"
        );
        let fifth =
            tokio::time::timeout(Duration::from_secs(1), caller.open_chat("support", None)).await;
        assert!(
            matches!(fifth, Ok(Err(CoreFailure::ConversationLimit))),
            "a fifth open is refused while the four are under way"
        );
        fixture.start_gate.release();
        fixture
            .assert_nothing_live("every creation nobody took is closed")
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while caller.binding.places_taken() != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the places are let go once the creations are closed");
    }

    #[tokio::test]
    async fn a_chat_stops_its_turn_when_it_ends() {
        let fixture = Fixture::new().await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let slow = json!({ "type": "message", "content": "slow please" });
        let gone_within = |why: &'static str| {
            let store = Arc::clone(&fixture.store);
            async move {
                // The turn would run on for seconds; the close cancels it.
                let closed = tokio::time::timeout(Duration::from_secs(2), async {
                    while !store.list_ids().await.is_empty() {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await;
                assert!(
                    closed.is_ok(),
                    "{why}: still live: {:?}",
                    store.list_ids().await
                );
            }
        };

        // The browser leaves mid-turn.
        let mut socket = open_chat(addr, &alice, "support", "web-1")
            .await
            .expect("alice may chat");
        let _ = await_frame(&mut socket, "session_start").await;
        socket
            .send(ClientMessage::Text(slow.to_string().into()))
            .await
            .unwrap();
        provider_received(&fixture, "slow please").await;
        drop(socket);
        gone_within("the browser left mid-turn").await;

        // The session ends mid-turn.
        let mut socket = open_chat(addr, &alice, "support", "web-2")
            .await
            .expect("alice may chat");
        let _ = await_frame(&mut socket, "session_start").await;
        socket
            .send(ClientMessage::Text(slow.to_string().into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(fixture.auth.end(&alice_session), "sign-out");
        gone_within("the session ended mid-turn").await;
    }

    #[tokio::test]
    async fn sign_in_churn_cannot_take_more_than_the_account_budget() {
        let fixture = Fixture::new().await;
        fixture.start_gate.arm();
        let mut opens = Vec::new();
        // The account signs in, opens its four chats, and signs out again, as
        // many times as it may hold sign-ins, while every creation waits.
        for _ in 0..crate::dashboard_session::MAX_SESSIONS_PER_PRINCIPAL {
            let (_, session) = fixture.sign_in("alice").await;
            let caller = core_caller_for(&fixture, &session);
            for _ in 0..MAX_LIVE_CONVERSATIONS {
                let caller = caller.clone();
                opens.push(zeroclaw_spawn::spawn!(async move {
                    caller
                        .open_chat("support", None)
                        .await
                        .map(|chat| chat.core_id.clone())
                }));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(fixture.auth.end(&session), "sign-out");
        }

        // Every place the account may hold is taken by a creation under way.
        let (_, session) = fixture.sign_in("alice").await;
        let caller = core_caller_for(&fixture, &session);
        let refused =
            tokio::time::timeout(Duration::from_secs(1), caller.open_chat("support", None)).await;
        assert!(
            matches!(refused, Ok(Err(CoreFailure::ConversationLimit))),
            "the account's budget holds across sign-outs"
        );
        // Another account is not held up by it: its open is admitted, and
        // waits on the core like the rest.
        let (_, bob_session) = fixture.sign_in("bob").await;
        let bob = core_caller_for(&fixture, &bob_session);
        let admitted =
            tokio::time::timeout(Duration::from_secs(1), bob.open_chat("support", None)).await;
        assert!(admitted.is_err(), "bob's open reached the core");

        fixture.start_gate.release();
        for open in opens {
            let _ = open.await;
        }
        fixture
            .assert_nothing_live("the ended sign-ins' creations are closed")
            .await;
    }

    /// A reply far larger than the socket's buffers, for a browser that stops
    /// reading: about 7 MB in its chunk frame and again in its `done` frame,
    /// each under the in-process frame limit (8 MB).
    async fn mount_flood(fixture: &Fixture) {
        Mock::given(http_method("POST"))
            .and(http_path("/chat/completions"))
            .and(body_string_contains("flood please"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {
                    "role": "assistant",
                    "content": "x".repeat(7 * 1024 * 1024),
                }}]
            })))
            .with_priority(1)
            .mount(&fixture._model)
            .await;
    }

    /// Open a chat, ask for the flood, and stop reading.
    async fn flooded_chat(fixture: &Fixture, addr: SocketAddr, cookie: &str) -> ChatSocket {
        let mut socket = open_chat(addr, cookie, "support", "web-1")
            .await
            .expect("the account may chat");
        let _ = await_frame(&mut socket, "session_start").await;
        let flood = json!({ "type": "message", "content": "flood please" });
        socket
            .send(ClientMessage::Text(flood.to_string().into()))
            .await
            .unwrap();
        provider_received(fixture, "flood please").await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        socket
    }

    #[tokio::test]
    async fn a_chat_whose_browser_stops_reading_still_ends_at_sign_out() {
        let fixture = Fixture::new().await;
        mount_flood(&fixture).await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let socket = flooded_chat(&fixture, addr, &alice).await;
        assert!(fixture.auth.end(&alice_session), "sign-out");
        fixture
            .assert_nothing_live("a chat stuck writing still closes its conversation")
            .await;
        drop(socket);
    }

    #[tokio::test]
    async fn a_chat_whose_browser_stops_reading_still_ends_at_a_reload() {
        let mut fixture = Fixture::new().await;
        mount_flood(&fixture).await;
        let addr = serve(fixture.router()).await;
        let (alice, alice_session) = fixture.sign_in("alice").await;
        let binding = fixture
            .auth
            .core_binding(&alice_session)
            .expect("signed in");
        let socket = flooded_chat(&fixture, addr, &alice).await;
        fixture.reload_gateway().await;
        tokio::time::timeout(Duration::from_secs(15), async {
            while binding.places_taken() != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a chat stuck writing still ends with its generation");
        drop(socket);
    }

    #[tokio::test]
    async fn an_account_holds_few_core_connections_for_its_requests() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let (alice, alice_session) = fixture.sign_in("alice").await;
        // The identity provider answers slowly, so each agent-list request
        // holds its connection's handshake open.
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .and(body_string_contains("token=tok-alice"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "active": true,
                        "token_type": "Bearer",
                        "client_id": "gw",
                        "iss": fixture.config.oidc["corp"].issuer,
                        "sub": "alice",
                        "aud": "zeroclaw",
                        "exp": unix_now() + 600,
                        "groups": ["chatters"],
                        "preferred_username": "alice@example.com",
                    }))
                    .set_delay(Duration::from_millis(500)),
            )
            .with_priority(1)
            .mount(&fixture._idp)
            .await;
        let requests: Vec<_> = (0..12)
            .map(|_| {
                let (router, alice, session) =
                    (router.clone(), alice.clone(), Arc::clone(&alice_session));
                zeroclaw_spawn::spawn!(async move {
                    call(
                        &router,
                        "GET",
                        "/api/agents/available",
                        Some((&alice, &session)),
                        None,
                    )
                    .await
                })
            })
            .collect();
        let mut most = 0;
        let watch = tokio::time::timeout(Duration::from_secs(30), async {
            while requests.iter().any(|request| !request.is_finished()) {
                most = most.max(fixture.connector.connection_count());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(watch.is_ok(), "the requests finish");
        assert!(
            most <= AUXILIARY_CONNECTIONS_PER_ACCOUNT,
            "{most} core connections at once for one account's requests"
        );
        for request in requests {
            let (status, body) = request.await.expect("joins");
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }

    /// The identity provider's introspection requests for `token`.
    async fn introspections_of(idp: &MockServer, token: &str) -> usize {
        idp.received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| {
                request.url.path() == "/introspect"
                    && String::from_utf8_lossy(&request.body).contains(&format!("token={token}"))
            })
            .count()
    }

    #[tokio::test]
    async fn the_identity_provider_is_not_asked_again_and_again() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let (alice, alice_session) = fixture.sign_in("alice").await;

        // The agent list is verified once and kept for a while.
        let before = introspections_of(&fixture._idp, "tok-alice").await;
        for _ in 0..5 {
            let (status, body) = call(
                &router,
                "GET",
                "/api/agents/available",
                Some((&alice, &alice_session)),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        assert_eq!(
            introspections_of(&fixture._idp, "tok-alice").await - before,
            1,
            "five agent lists, one verification"
        );

        // A token the provider turns down is presented once, not twice.
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .and(body_string_contains("token=tok-alice"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "active": false })))
            .with_priority(1)
            .mount(&fixture._idp)
            .await;
        let before = introspections_of(&fixture._idp, "tok-alice").await;
        let refused = core_caller_for(&fixture, &alice_session)
            .open_chat("support", None)
            .await;
        assert!(
            matches!(refused, Err(CoreFailure::SessionEnded)),
            "{:?}",
            refused.map(|chat| chat.core_id.clone())
        );
        assert_eq!(
            introspections_of(&fixture._idp, "tok-alice").await - before,
            1,
            "a refused token is not presented again"
        );
    }

    // ── S review round 2 (astra) ─────────────────────────────────────────

    #[tokio::test]
    async fn a_creation_whose_outcome_is_unknown_stays_charged() {
        // The orphan backstop is quick here, so what lands after its chat gave
        // up is let go of within the test.
        let fixture = Fixture::with_config(|config| {
            config.wss.orphaned_session_timeout_secs = 1;
        })
        .await;
        fixture
            .core
            .accounts
            .set_create_ceiling(Duration::from_millis(300));
        // The core publishes each conversation, then holds its answer (the
        // session-start hook) past the creation's ceiling.
        fixture.start_gate.arm();
        for _ in 0..crate::dashboard_session::MAX_SESSIONS_PER_PRINCIPAL {
            let (_, session) = fixture.sign_in("alice").await;
            let caller = core_caller_for(&fixture, &session);
            for _ in 0..MAX_LIVE_CONVERSATIONS {
                let opened = caller.open_chat("support", None).await;
                assert!(
                    matches!(opened, Err(CoreFailure::Lost(_))),
                    "the creation outlived its ceiling: {:?}",
                    opened.map(|chat| chat.core_id.clone())
                );
            }
            assert!(fixture.auth.end(&session), "sign-out");
        }

        // All of the account's 32 are held by creations whose outcome is
        // unknown.
        let (_, session) = fixture.sign_in("alice").await;
        let refused = tokio::time::timeout(
            Duration::from_secs(1),
            core_caller_for(&fixture, &session).open_chat("support", None),
        )
        .await;
        assert!(
            matches!(refused, Ok(Err(CoreFailure::ConversationLimit))),
            "a creation with an unknown outcome stays charged"
        );
        fixture.start_gate.release();
        fixture
            .assert_nothing_live("what landed after its chat gave up is closed")
            .await;
    }

    /// The agents a request lists, by alias, sorted.
    fn aliases(body: &Value) -> Vec<String> {
        let mut aliases: Vec<String> = body["agents"]
            .as_array()
            .map(|agents| {
                agents
                    .iter()
                    .filter_map(|agent| agent["alias"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        aliases.sort();
        aliases
    }

    #[tokio::test]
    async fn a_kept_agent_list_follows_the_accounts_grants() {
        let fixture = Fixture::with_config(|config| {
            config
                .permission_profiles
                .get_mut("chat-reader")
                .expect("the fixture's reader profile")
                .allowed_agents = vec!["*".into()];
        })
        .await;
        let router = fixture.router();
        let (carol, carol_session) = fixture.sign_in("carol").await;
        let (root, root_session) = fixture.sign_in("root").await;
        for (cookie, session) in [(&carol, &carol_session), (&root, &root_session)] {
            let (status, body) = call(
                &router,
                "GET",
                "/api/agents/available",
                Some((cookie, session)),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(aliases(&body), ["other", "support"], "{body}");
        }

        // `agents:read` is revoked from the reader profile, its session grants
        // and wildcard kept, and the administrator profile is demoted.
        let mut changed = fixture.config.clone();
        changed
            .permission_profiles
            .get_mut("chat-reader")
            .expect("the fixture's reader profile")
            .grants = Map::from([(Resource::Sessions, vec![Verb::Create, Verb::Read])]);
        changed.permission_profiles.insert(
            "admin".into(),
            PermissionProfileConfig {
                grants: Map::from([(Resource::Sessions, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
        fixture
            .rpc_auth
            .publish_accepted(&changed, fixture.rpc_auth.accepted_revision() + 1)
            .expect("the gateway accepts the policy");
        fixture
            .core_auth
            .publish_accepted(&changed, fixture.core_auth.accepted_revision() + 1)
            .expect("the core accepts the policy");

        // Within the 30 s, each gets what an uncached request would: the
        // agents it was granted by name, and a wildcard names none.
        for (who, cookie, session) in [
            ("carol", &carol, &carol_session),
            ("root", &root, &root_session),
        ] {
            let (status, body) = call(
                &router,
                "GET",
                "/api/agents/available",
                Some((cookie, session)),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{who}: {body}");
            assert!(
                aliases(&body).is_empty(),
                "{who} still sees the kept list: {body}"
            );
        }
    }

    /// The identity provider answers `name`'s next `times` introspections
    /// (in `group`) only after `delay`, ahead of mocks of a lower `priority`
    /// (a higher number).
    async fn slow_introspections(
        fixture: &Fixture,
        name: &str,
        group: &str,
        delay: Duration,
        (times, priority): (u64, u8),
    ) {
        Mock::given(http_method("POST"))
            .and(http_path("/introspect"))
            .and(body_string_contains(format!("token=tok-{name}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "active": true,
                        "token_type": "Bearer",
                        "client_id": "gw",
                        "iss": fixture.config.oidc["corp"].issuer,
                        "sub": name,
                        "aud": "zeroclaw",
                        "exp": unix_now() + 600,
                        "groups": [group],
                        "preferred_username": format!("{name}@example.com"),
                    }))
                    .set_delay(delay),
            )
            .up_to_n_times(times)
            .with_priority(priority)
            .mount(&fixture._idp)
            .await;
    }

    /// Long enough for a connection to be seen while it is open.
    const SEEN: Duration = Duration::from_secs(1);

    /// An agent-list request from `cookie`'s session, in a task of its own.
    fn agent_list_request(
        router: &Router,
        cookie: &str,
        session: &Arc<SessionAuth>,
    ) -> tokio::task::JoinHandle<(StatusCode, Value)> {
        let (router, cookie, session) = (router.clone(), cookie.to_string(), Arc::clone(session));
        zeroclaw_spawn::spawn!(async move {
            call(
                &router,
                "GET",
                "/api/agents/available",
                Some((&cookie, &session)),
                None,
            )
            .await
        })
    }

    /// The most core connections open at once until every one of `requests`
    /// has finished and the core has let go of every connection.
    async fn most_connections_until_done(
        fixture: &Fixture,
        requests: &[tokio::task::JoinHandle<(StatusCode, Value)>],
    ) -> usize {
        let mut most = 0;
        let watch = tokio::time::timeout(Duration::from_secs(60), async {
            while requests.iter().any(|request| !request.is_finished())
                || fixture.connector.connection_count() != 0
            {
                most = most.max(fixture.connector.connection_count());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(watch.is_ok(), "the requests finish and the core lets go");
        most
    }

    /// Past the handshake's deadline the gateway has given up on the
    /// connection, but the core may still be verifying its token.
    const PAST_THE_HANDSHAKE_DEADLINE: Duration =
        Duration::from_secs(zeroclaw_rpc_client::DEFAULT_HANDSHAKE_TIMEOUT.as_secs() + 2);

    #[tokio::test]
    async fn an_accounts_connection_bound_holds_past_the_handshake_deadline() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        let (alice, alice_session) = fixture.sign_in("alice").await;
        // The core's first two verifications take longer than the gateway
        // waits for a handshake; the next ones take long enough to be seen.
        let slots = AUXILIARY_CONNECTIONS_PER_ACCOUNT as u64;
        slow_introspections(
            &fixture,
            "alice",
            "chatters",
            PAST_THE_HANDSHAKE_DEADLINE,
            (slots, 1),
        )
        .await;
        slow_introspections(&fixture, "alice", "chatters", SEEN, (slots, 2)).await;
        let mut requests: Vec<_> = (0..AUXILIARY_CONNECTIONS_PER_ACCOUNT)
            .map(|_| agent_list_request(&router, &alice, &alice_session))
            .collect();
        tokio::time::sleep(Duration::from_millis(300)).await;
        // One request goes away; the other gives up at the deadline.
        requests[0].abort();
        // Two more, while both verifications are still under way in the core.
        let replacements: Vec<_> = (0..AUXILIARY_CONNECTIONS_PER_ACCOUNT)
            .map(|_| agent_list_request(&router, &alice, &alice_session))
            .collect();
        requests.extend(replacements);
        let most = most_connections_until_done(&fixture, &requests).await;
        assert!(
            most <= AUXILIARY_CONNECTIONS_PER_ACCOUNT,
            "{most} core connections at once for one account's requests"
        );
        for replacement in requests.split_off(AUXILIARY_CONNECTIONS_PER_ACCOUNT) {
            let (status, body) = replacement.await.expect("joins");
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }

    #[tokio::test]
    async fn the_global_connection_bound_holds_past_the_handshake_deadline() {
        let fixture = Fixture::new().await;
        let router = fixture.router();
        // Enough accounts to fill the global bound with their own, and one
        // more.
        let filling = AUXILIARY_CONNECTIONS / AUXILIARY_CONNECTIONS_PER_ACCOUNT;
        let mut signed_in = Vec::new();
        for n in 0..=filling {
            let name = format!("user-{n}");
            Mock::given(http_method("POST"))
                .and(http_path("/introspect"))
                .and(body_string_contains(format!("token=tok-{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "active": true,
                    "token_type": "Bearer",
                    "client_id": "gw",
                    "iss": fixture.config.oidc["corp"].issuer,
                    "sub": name,
                    "aud": "zeroclaw",
                    "exp": unix_now() + 600,
                    "groups": ["chatters"],
                    "preferred_username": format!("{name}@example.com"),
                })))
                .mount(&fixture._idp)
                .await;
            signed_in.push(fixture.sign_in(&name).await);
        }
        let slots = AUXILIARY_CONNECTIONS_PER_ACCOUNT as u64;
        slow_introspections(
            &fixture,
            &format!("user-{filling}"),
            "chatters",
            SEEN,
            (slots, 1),
        )
        .await;
        let mut requests = Vec::new();
        for (n, (cookie, session)) in signed_in.iter().take(filling).enumerate() {
            slow_introspections(
                &fixture,
                &format!("user-{n}"),
                "chatters",
                PAST_THE_HANDSHAKE_DEADLINE,
                (slots, 1),
            )
            .await;
            for _ in 0..AUXILIARY_CONNECTIONS_PER_ACCOUNT {
                requests.push(agent_list_request(&router, cookie, session));
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        // The last account's requests wait for the global bound, while the
        // others' verifications are still under way in the core.
        let (cookie, session) = &signed_in[filling];
        let last: Vec<_> = (0..AUXILIARY_CONNECTIONS_PER_ACCOUNT)
            .map(|_| agent_list_request(&router, cookie, session))
            .collect();
        requests.extend(last);
        let most = most_connections_until_done(&fixture, &requests).await;
        assert!(
            most <= AUXILIARY_CONNECTIONS,
            "{most} core connections at once for all accounts' requests"
        );
        for request in requests.split_off(filling * AUXILIARY_CONNECTIONS_PER_ACCOUNT) {
            let (status, body) = request.await.expect("joins");
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }

    #[tokio::test]
    async fn account_entries_go_when_nothing_holds_them() {
        let accounts = Accounts::default();
        for n in 0..50 {
            let principal = format!("account-{n}");
            let place = accounts.take(&principal).expect("room");
            let auxiliary = accounts.auxiliary(&principal).await.expect("a permit");
            drop((place, auxiliary));
        }
        assert_eq!(accounts.held.lock().len(), 0, "empty entries are reclaimed");

        // A charge kept after a failed close keeps its entry.
        accounts.take("kept").expect("room").keep();
        assert_eq!(accounts.held.lock().len(), 1);

        // Requests waiting for a permit share one semaphore with those holding
        // one: the entry is not reclaimed under them.
        let mut holding = Vec::new();
        for _ in 0..AUXILIARY_CONNECTIONS_PER_ACCOUNT {
            holding.push(accounts.auxiliary("busy").await.expect("a permit"));
        }
        let waiting = {
            let accounts = accounts.clone();
            zeroclaw_spawn::spawn!(async move { accounts.auxiliary("busy").await.is_ok() })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the third waits for a permit");
        drop(holding);
        assert!(waiting.await.expect("joins"), "the waiter gets its permit");
        assert_eq!(
            accounts.held.lock().len(),
            1,
            "only the kept charge is left"
        );
    }

    // ── S review round 3 (astra) ─────────────────────────────────────────

    #[tokio::test]
    async fn a_late_answer_settles_a_creation_the_chat_stopped_waiting_for() {
        // With the orphan backstop off, nothing but the gateway closes what
        // lands after its chat stopped waiting.
        let fixture = Fixture::with_config(|config| {
            config.wss.orphaned_session_timeout_secs = 0;
        })
        .await;
        fixture
            .core
            .accounts
            .set_create_ceiling(Duration::from_millis(300));
        // The core publishes each conversation, then holds its answer (the
        // session-start hook) past the creation's ceiling.
        fixture.start_gate.arm();
        let (_, session) = fixture.sign_in("alice").await;
        let principal = session.principal().id.clone();
        let caller = core_caller_for(&fixture, &session);
        for _ in 0..MAX_LIVE_CONVERSATIONS {
            let opened = caller.open_chat("support", None).await;
            assert!(
                matches!(opened, Err(CoreFailure::Lost(_))),
                "the creation outlived its ceiling: {:?}",
                opened.map(|chat| chat.core_id.clone())
            );
        }
        let charged = |accounts: &Accounts| {
            accounts
                .held
                .lock()
                .get(principal.as_str())
                .map_or(0, |account| account.conversations)
        };
        assert_eq!(
            charged(&fixture.core.accounts),
            MAX_LIVE_CONVERSATIONS,
            "unanswered creations stay charged"
        );

        // The core answers at last: the gateway closes each conversation and
        // gives the account its share back.
        fixture.start_gate.release();
        fixture
            .assert_nothing_live(
                "the gateway closes what the core created after its chat stopped waiting",
            )
            .await;
        let settled = tokio::time::timeout(Duration::from_secs(15), async {
            while charged(&fixture.core.accounts) != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            settled.is_ok(),
            "{} still charged once the core answered",
            charged(&fixture.core.accounts)
        );
    }

    // ── Round-5 follow-ups ───────────────────────────────────────────────

    #[tokio::test]
    async fn a_close_the_writer_refused_locally_is_tried_again() {
        let fixture = Fixture::new().await;
        let (_, session) = fixture.sign_in("alice").await;
        let mut chat = core_caller_for(&fixture, &session)
            .open_chat("support", None)
            .await
            .expect("opens");
        let closing = chat.closing.take().expect("an open chat can be closed");
        let core_id = chat.core_id.clone();

        // The chat's connection loses its writer between the close's state
        // check and its send: the outbound lane has closed, and the client
        // reports the refused send as an RPC error, not as a lost connection.
        let client = chat.bound();
        client.shutdown();
        tokio::time::timeout(Duration::from_secs(5), client.closed())
            .await
            .expect("the outbound lane closes");
        let refused = client
            .call::<Value>(Method::SessionClose, json!({ "session_id": core_id }))
            .await;
        assert!(
            matches!(&refused, Err(ClientError::Rpc(_))),
            "the premise: a locally refused send reads as an RPC error: {refused:?}"
        );

        // That is not the core refusing the close: it is tried again.
        assert_eq!(
            closing.redial.close_on(&core_id, &client).await,
            CloseAttempt::Retry,
            "a send the writer refused locally never reached the core"
        );
        assert!(
            closing.redial.close(&core_id, Some(client)).await,
            "the next attempt, on a new connection, closes it"
        );
        assert!(!fixture.store.list_ids().await.contains(&core_id));
        drop(chat);
    }
}
