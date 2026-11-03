//! Watch-together rooms over end-to-end encrypted peer-to-peer connections.
//!
//! There is no password. Each reel has a private key that never leaves its machine, and a
//! public reel ID. A host invites a specific reel ID, and the connection handshake (QUIC with
//! TLS 1.3) makes the guest prove it holds that ID's private key. A copied, leaked or guessed
//! invite is therefore useless to anyone else. Each invite works once, expires, and the host
//! still approves the guest after comparing safety words derived from the session keys, which
//! catches a reel ID or invite swapped in transit.
//!
//! Peers are not trusted: every message is size-limited, strictly parsed and validated, and
//! can only name content. It can never carry a URL, path or command, so a host cannot make a
//! guest open something of the host's choosing.
use crate::config::config_dir;
use anyhow::{Context, Result, anyhow, bail};
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Semaphore, mpsc, oneshot};

const ALPN: &[u8] = b"reel/room/1";
const PROTOCOL_VERSION: u32 = 1;
const ID_PREFIX: &str = "reel-id-";
const INVITE_PREFIX: &str = "reel-invite-";
/// Separates invite signatures from anything else this key might ever sign.
const INVITE_DOMAIN: &[u8] = b"reel-invite-v1";
const SAFETY_LABEL: &[u8] = b"reel-room-safety-words-v1";
const INVITE_LIFETIME_SECONDS: u64 = 15 * 60;
const MAX_FRAME_BYTES: u32 = 16 * 1024;
const MAX_GUESTS: usize = 8;
const MAX_TITLE_CHARS: usize = 200;
const MAX_HANDSHAKES_AT_ONCE: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_CONTROLS_PER_SECOND: u32 = 10;
const OUTGOING_QUEUE: usize = 32;

// --- Identity ---

fn identity_path() -> std::path::PathBuf {
    config_dir().join("identity.key")
}

/// This reel's private key, created on first use.
pub fn load_identity() -> Result<SecretKey> {
    load_identity_from(&identity_path())
}

fn load_identity_from(path: &Path) -> Result<SecretKey> {
    if let Ok(bytes) = std::fs::read(path) {
        let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| anyhow!("{} is damaged", path.display()))?;
        return Ok(SecretKey::from_bytes(&bytes));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let secret = SecretKey::generate();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, &secret.to_bytes())?;
    Ok(secret)
}

pub fn format_id(id: &EndpointId) -> String {
    format!("{ID_PREFIX}{id}")
}

pub fn parse_id(text: &str) -> Result<EndpointId> {
    let hex = text.trim().strip_prefix(ID_PREFIX).context("a reel ID starts with reel-id-")?;
    hex.parse().map_err(|_| anyhow!("that reel ID is not valid"))
}

/// A short form for lists; never used to decide who is allowed in.
pub fn short_id(id: &EndpointId) -> String {
    id.to_string().chars().take(8).collect()
}

// --- Invites ---

const fn invite_body_len() -> usize {
    1 + 32 + 32 + 16 + 8
}

#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    pub host: EndpointId,
    pub guest: EndpointId,
    pub room: [u8; 16],
    pub expires: u64,
}

impl Invite {
    fn body(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(invite_body_len());
        body.push(1);
        body.extend_from_slice(self.host.as_bytes());
        body.extend_from_slice(self.guest.as_bytes());
        body.extend_from_slice(&self.room);
        body.extend_from_slice(&self.expires.to_be_bytes());
        body
    }

    fn signed_bytes(body: &[u8]) -> Vec<u8> {
        [INVITE_DOMAIN, body].concat()
    }

    fn encode(&self, host_key: &SecretKey) -> String {
        let body = self.body();
        let signature = host_key.sign(&Self::signed_bytes(&body));
        let mut code = String::from(INVITE_PREFIX);
        for byte in body.iter().chain(signature.to_bytes().iter()) {
            code.push_str(&format!("{byte:02x}"));
        }
        code
    }

    /// Checks the signature, that this invite names `me`, and that it has not expired.
    pub fn open(code: &str, me: &EndpointId, now: u64) -> Result<Self> {
        let hex = code.trim().strip_prefix(INVITE_PREFIX).context("an invite starts with reel-invite-")?;
        let bytes = decode_hex(hex).context("that invite is damaged")?;
        if bytes.len() != invite_body_len() + Signature::LENGTH {
            bail!("that invite is damaged");
        }
        let (body, signature) = bytes.split_at(invite_body_len());
        if body[0] != 1 {
            bail!("that invite is from a newer version of reel");
        }
        let key = |range: std::ops::Range<usize>| -> Result<EndpointId> {
            let raw: [u8; 32] = body[range].try_into()?;
            EndpointId::from_bytes(&raw).map_err(|_| anyhow!("that invite is damaged"))
        };
        let host = key(1..33)?;
        let guest = key(33..65)?;
        let signature: [u8; Signature::LENGTH] = signature.try_into()?;
        host.verify(&Self::signed_bytes(body), &Signature::from_bytes(&signature)).map_err(|_| anyhow!("that invite is forged or damaged"))?;
        if guest != *me {
            bail!("that invite was made for a different reel ID");
        }
        let expires = u64::from_be_bytes(body[81..89].try_into()?);
        if now > expires {
            bail!("that invite has expired; ask the host for a new one");
        }
        Ok(Self { host, guest, room: body[65..81].try_into()?, expires })
    }
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// --- Safety words ---

const WORDS: [&str; 256] = [
    "acorn", "actor", "adobe", "agent", "aisle", "alarm", "album", "alley", "amber", "angle", "ankle", "apple", "apron", "arrow", "aspen", "atlas", "attic", "award", "bacon", "badge", "bagel", "baker", "bamboo", "banjo", "barn", "basil", "beach", "beard", "beast", "bench", "berry", "bison", "blade", "blaze", "bloom", "board", "boat", "bonus", "boots", "brain", "brass", "bread", "brick", "bridge", "brook", "brush", "bugle", "cabin", "cable", "camel", "candle", "canoe", "canyon", "cargo", "carrot", "castle", "cedar", "chair", "chalk", "cheese", "cherry", "chess", "chief", "cider", "cliff", "clock", "cloud", "clover", "coast", "cobra", "comet", "coral", "cotton", "couch", "crane", "crate", "creek", "crown", "daisy", "dance", "delta", "denim", "desert", "diary", "dingo", "dolphin", "donut", "dragon", "drum", "eagle", "earth", "easel", "ember", "engine", "fable", "falcon", "feast", "fence", "fern", "ferry", "fiddle", "field", "flame", "flute", "forest", "fossil", "fox", "frost", "fudge", "galaxy", "garden", "garlic", "gecko", "giant", "ginger", "glass", "globe", "glove", "goose", "grape", "gravel", "guitar", "harbor", "hazel", "heron", "hiker", "hippo", "honey", "horse", "hotel", "igloo", "island", "ivory", "jacket", "jaguar", "jelly", "jewel", "juice", "kayak", "kettle", "kiwi", "koala", "ladder", "lagoon", "lake", "lantern", "lemon", "lily", "lion", "llama", "lobster", "lotus", "lunar", "magnet", "mango", "maple", "marble", "meadow", "melon", "mirror", "mitten", "monkey", "moose", "mosaic", "mountain", "muffin", "mural", "napkin", "nectar", "needle", "noodle", "oasis", "ocean", "olive", "onion", "orange", "orchid", "otter", "owl", "paddle", "panda", "paper", "parrot", "pasta", "peach", "pearl", "pebble", "pencil", "pepper", "piano", "pickle", "pillow", "pine", "pirate", "planet", "plum", "pocket", "poppy", "pretzel", "prism", "pumpkin", "puzzle", "quail", "quartz", "quilt", "rabbit", "radar", "raven", "reef", "ribbon", "river", "robin", "rocket", "rose", "ruby", "saddle", "salmon", "sand", "sapphire", "scarf", "shell", "silver", "skate", "sloth", "snail", "spoon", "squid", "stone", "storm", "sugar", "summit", "swan", "table", "tiger", "tomato", "topaz", "torch", "tulip", "turtle", "umbrella", "valley", "velvet", "violin", "walnut", "walrus", "wheat", "willow", "window", "yogurt", "zebra", "zinnia", "nutmeg", "oyster", "pilot", "sparrow", "tunnel",
];

/// Four words derived from this connection's session keys. Both ends only get the same words
/// when they share one session, with nobody in between.
fn safety_words(connection: &Connection) -> Result<String> {
    let mut bytes = [0u8; 4];
    connection.export_keying_material(&mut bytes, SAFETY_LABEL, b"").map_err(|_| anyhow!("could not derive safety words"))?;
    Ok(bytes.iter().map(|b| WORDS[*b as usize]).collect::<Vec<_>>().join(" "))
}

// --- Messages ---

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadOffer {
    pub meta_id: String,
    pub video_id: String,
    pub kind: String,
    pub title: String,
    /// Only a hint: a guest uses it only if its own addons also list this torrent.
    pub hash: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncState {
    pub position: f64,
    pub paused: bool,
    pub speed: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Control {
    Pause,
    Resume,
    Seek { position: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// Externally tagged: serde does not enforce deny_unknown_fields on internally tagged enums.
#[serde(deny_unknown_fields)]
enum Wire {
    Hello { version: u32, room: String },
    Waiting,
    Welcome,
    Load { offer: LoadOffer },
    State { state: SyncState },
    Control { control: Control },
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// Only id shapes that name content in a public catalog. Local files, URLs and anything else
/// a guest could be tricked into opening do not pass.
fn valid_content_id(id: &str) -> bool {
    if id.len() > 64 {
        return false;
    }
    if let Some(video) = id.strip_prefix("yt:") {
        return video.len() == 11 && video.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    }
    let rest = id.strip_prefix("tt").or_else(|| id.strip_prefix("kitsu:"));
    rest.is_some_and(|rest| {
        let mut parts = rest.split(':');
        parts.next().is_some_and(is_digits) && parts.clone().count() <= 2 && parts.all(is_digits)
    })
}

fn valid_position(value: f64) -> bool {
    value.is_finite() && (0.0..=864_000.0).contains(&value)
}

impl LoadOffer {
    fn is_valid(&self) -> bool {
        valid_content_id(&self.meta_id)
            && valid_content_id(&self.video_id)
            && matches!(self.kind.as_str(), "movie" | "series" | "anime" | "youtube")
            && self.title.chars().count() <= MAX_TITLE_CHARS
            && !self.title.chars().any(char::is_control)
            && self.hash.as_ref().is_none_or(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    }
}

/// Makes a title safe to send: removes terminal control characters and limits its length.
pub fn clean_title(title: &str) -> String {
    title.chars().filter(|c| !c.is_control()).take(MAX_TITLE_CHARS).collect()
}

/// Whether a host may offer this content at all. Local files stay private.
pub fn can_share(meta_id: &str, video_id: &str) -> bool {
    valid_content_id(meta_id) && valid_content_id(video_id)
}

impl Wire {
    fn is_valid(&self) -> bool {
        match self {
            Wire::Hello { room, .. } => room.len() == 32 && room.bytes().all(|b| b.is_ascii_hexdigit()),
            Wire::Waiting | Wire::Welcome => true,
            Wire::Load { offer } => offer.is_valid(),
            Wire::State { state } => valid_position(state.position) && state.speed.is_finite() && (0.25..=4.0).contains(&state.speed),
            Wire::Control { control } => match control {
                Control::Seek { position } => valid_position(*position),
                Control::Pause | Control::Resume => true,
            },
        }
    }
}

async fn write_frame(send: &mut SendStream, message: &Wire) -> Result<()> {
    let bytes = serde_json::to_vec(message)?;
    if bytes.len() as u64 > MAX_FRAME_BYTES as u64 {
        bail!("message too large");
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await.map_err(|e| anyhow!("{e}"))?;
    send.write_all(&bytes).await.map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// Reads one message. Oversized, malformed or invalid messages are an error, which ends the
/// connection: a well-behaved reel never sends one.
async fn read_frame(recv: &mut RecvStream) -> Result<Wire> {
    let mut length = [0u8; 4];
    recv.read_exact(&mut length).await.map_err(|e| anyhow!("{e}"))?;
    let length = u32::from_be_bytes(length);
    if length > MAX_FRAME_BYTES {
        bail!("peer sent an oversized message");
    }
    let mut bytes = vec![0u8; length as usize];
    recv.read_exact(&mut bytes).await.map_err(|e| anyhow!("{e}"))?;
    let message: Wire = serde_json::from_slice(&bytes).context("peer sent a malformed message")?;
    if !message.is_valid() {
        bail!("peer sent an invalid message");
    }
    Ok(message)
}

// --- Events ---

#[derive(Clone, Debug, PartialEq)]
pub enum RoomEvent {
    /// A correctly invited guest has connected and needs approval.
    GuestWaiting { id: EndpointId, words: String },
    GuestJoined { id: EndpointId },
    GuestLeft { id: EndpointId },
    /// Someone without a valid invite tried to connect and was turned away.
    Refused { id: EndpointId },
    GuestControl { id: EndpointId, control: Control },
    /// A guest connected to the host and is waiting for approval.
    Connected { words: String },
    Admitted,
    Load(LoadOffer),
    State(SyncState),
    Closed(String),
}

type Events = mpsc::UnboundedSender<RoomEvent>;

async fn bind(secret: SecretKey) -> Result<Endpoint> {
    Endpoint::builder(presets::N0).secret_key(secret).alpns(vec![ALPN.to_vec()]).bind().await.map_err(|e| anyhow!("could not start networking: {e}"))
}

// --- Host ---

#[derive(Default)]
struct HostState {
    /// Who may connect and until when. Entries are removed on first use.
    invited: HashMap<EndpointId, u64>,
    pending: HashMap<EndpointId, oneshot::Sender<bool>>,
    members: HashMap<EndpointId, mpsc::Sender<Wire>>,
    last_load: Option<LoadOffer>,
    last_state: Option<SyncState>,
}

pub struct Host {
    endpoint: Endpoint,
    secret: SecretKey,
    room: [u8; 16],
    state: Arc<Mutex<HostState>>,
}

impl Host {
    pub async fn start(secret: SecretKey, events: Events) -> Result<Self> {
        let endpoint = bind(secret.clone()).await?;
        Ok(Self::on_endpoint(endpoint, secret, events))
    }

    fn on_endpoint(endpoint: Endpoint, secret: SecretKey, events: Events) -> Self {
        // Key generation is the CSPRNG at hand; these bytes only need to be unguessable.
        let room: [u8; 16] = SecretKey::generate().to_bytes()[..16].try_into().unwrap_or([0; 16]);
        let state: Arc<Mutex<HostState>> = Arc::default();
        let (loop_endpoint, loop_state) = (endpoint.clone(), state.clone());
        tokio::spawn(accept_loop(loop_endpoint, loop_state, encode_hex(&room), events));
        Self { endpoint, secret, room, state }
    }

    /// An invite only this reel ID can use.
    pub fn invite(&self, guest: EndpointId) -> Result<String> {
        if guest == self.endpoint.id() {
            bail!("that is your own reel ID");
        }
        let expires = now_seconds() + INVITE_LIFETIME_SECONDS;
        self.state.lock().unwrap().invited.insert(guest, expires);
        Ok(Invite { host: self.endpoint.id(), guest, room: self.room, expires }.encode(&self.secret))
    }

    pub fn decide(&self, guest: &EndpointId, approve: bool) {
        if let Some(answer) = self.state.lock().unwrap().pending.remove(guest) {
            let _ = answer.send(approve);
        }
    }

    /// Removes a guest: dropping its queue ends its connection task.
    pub fn remove(&self, guest: &EndpointId) {
        let mut state = self.state.lock().unwrap();
        state.members.remove(guest);
        state.invited.remove(guest);
    }

    pub fn load(&self, offer: LoadOffer) {
        let mut state = self.state.lock().unwrap();
        state.last_load = Some(offer.clone());
        broadcast(&state, &Wire::Load { offer });
    }

    pub fn sync(&self, sync: SyncState) {
        let mut state = self.state.lock().unwrap();
        state.last_state = Some(sync);
        broadcast(&state, &Wire::State { state: sync });
    }

    pub async fn close(self) {
        self.endpoint.close().await;
    }
}

/// A slow guest loses messages instead of growing a queue; the next state corrects it.
fn broadcast(state: &HostState, message: &Wire) {
    for member in state.members.values() {
        let _ = member.try_send(message.clone());
    }
}

async fn accept_loop(endpoint: Endpoint, state: Arc<Mutex<HostState>>, room: String, events: Events) {
    let handshakes = Arc::new(Semaphore::new(MAX_HANDSHAKES_AT_ONCE));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(permit) = handshakes.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let (state, room, events) = (state.clone(), room.clone(), events.clone());
        tokio::spawn(async move {
            let connecting = async {
                let accepting = incoming.accept().map_err(|e| anyhow!("{e}"))?;
                accepting.await.map_err(|e| anyhow!("{e}"))
            };
            let connection = tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting).await;
            drop(permit);
            if let Ok(Ok(connection)) = connection {
                let _ = serve_guest(connection, state, room, events).await;
            }
        });
    }
}

/// The handshake has proved this peer holds `remote_id`'s private key. It gets nothing at all
/// unless that exact id has an unused, unexpired invite.
async fn serve_guest(connection: Connection, state: Arc<Mutex<HostState>>, room: String, events: Events) -> Result<()> {
    let id = connection.remote_id();
    let allowed = {
        let mut state = state.lock().unwrap();
        let invited = state.invited.remove(&id).is_some_and(|expires| now_seconds() <= expires);
        invited && state.members.len() + state.pending.len() < MAX_GUESTS
    };
    if !allowed {
        connection.close(1u32.into(), b"");
        let _ = events.send(RoomEvent::Refused { id });
        return Ok(());
    }
    let result = run_guest(&connection, id, state.clone(), room, &events).await;
    {
        let mut state = state.lock().unwrap();
        state.pending.remove(&id);
        state.members.remove(&id);
    }
    connection.close(0u32.into(), b"");
    let _ = events.send(RoomEvent::GuestLeft { id });
    result
}

async fn run_guest(connection: &Connection, id: EndpointId, state: Arc<Mutex<HostState>>, room: String, events: &Events) -> Result<()> {
    let opening = async {
        let (send, mut recv) = connection.accept_bi().await.map_err(|e| anyhow!("{e}"))?;
        let hello = read_frame(&mut recv).await?;
        anyhow::Ok((send, recv, hello))
    };
    let (mut send, mut recv, hello) = tokio::time::timeout(HANDSHAKE_TIMEOUT, opening).await.context("guest said nothing")??;
    match hello {
        Wire::Hello { version, room: claimed } if version == PROTOCOL_VERSION && claimed == room => {}
        _ => bail!("guest did not present this room"),
    }
    write_frame(&mut send, &Wire::Waiting).await?;

    let (answer, approval) = oneshot::channel();
    state.lock().unwrap().pending.insert(id, answer);
    let _ = events.send(RoomEvent::GuestWaiting { id, words: safety_words(connection)? });
    let approved = tokio::time::timeout(APPROVAL_TIMEOUT, approval).await.is_ok_and(|answer| answer.unwrap_or(false));
    if !approved {
        bail!("not approved");
    }

    let (queue, mut outgoing) = mpsc::channel(OUTGOING_QUEUE);
    let catch_up = {
        let mut state = state.lock().unwrap();
        state.members.insert(id, queue);
        (state.last_load.clone(), state.last_state)
    };
    write_frame(&mut send, &Wire::Welcome).await?;
    if let Some(offer) = catch_up.0 {
        write_frame(&mut send, &Wire::Load { offer }).await?;
    }
    if let Some(sync) = catch_up.1 {
        write_frame(&mut send, &Wire::State { state: sync }).await?;
    }
    let _ = events.send(RoomEvent::GuestJoined { id });

    let mut window = Instant::now();
    let mut in_window = 0;
    loop {
        tokio::select! {
            message = outgoing.recv() => match message {
                Some(message) => write_frame(&mut send, &message).await?,
                None => return Ok(()),
            },
            frame = read_frame(&mut recv) => {
                if window.elapsed() >= Duration::from_secs(1) {
                    window = Instant::now();
                    in_window = 0;
                }
                in_window += 1;
                if in_window > MAX_CONTROLS_PER_SECOND {
                    bail!("guest sent too many messages");
                }
                match frame? {
                    Wire::Control { control } => {
                        let _ = events.send(RoomEvent::GuestControl { id, control });
                    }
                    _ => bail!("guest sent a message only hosts may send"),
                }
            }
        }
    }
}

// --- Guest ---

pub struct Guest {
    outgoing: mpsc::Sender<Control>,
    endpoint: Endpoint,
}

impl Guest {
    pub async fn join(secret: SecretKey, code: &str, events: Events) -> Result<Self> {
        let invite = Invite::open(code, &secret.public(), now_seconds())?;
        let endpoint = bind(secret).await?;
        Ok(Self::on_endpoint(endpoint, invite.host.into(), invite, events))
    }

    fn on_endpoint(endpoint: Endpoint, host: EndpointAddr, invite: Invite, events: Events) -> Self {
        let (outgoing, controls) = mpsc::channel(OUTGOING_QUEUE);
        let task_endpoint = endpoint.clone();
        tokio::spawn(async move {
            let result = run_as_guest(&task_endpoint, host, invite, controls, &events).await;
            let reason = match result {
                Ok(()) => "the room closed".to_string(),
                Err(e) => format!("{e:#}"),
            };
            let _ = events.send(RoomEvent::Closed(reason));
        });
        Self { outgoing, endpoint }
    }

    pub fn control(&self, control: Control) {
        let _ = self.outgoing.try_send(control);
    }

    pub async fn close(self) {
        self.endpoint.close().await;
    }
}

async fn run_as_guest(endpoint: &Endpoint, host: EndpointAddr, invite: Invite, mut controls: mpsc::Receiver<Control>, events: &Events) -> Result<()> {
    // Connecting to the host's id authenticates the host: nobody else can complete this.
    let connection = endpoint.connect(host, ALPN).await.map_err(|e| anyhow!("could not reach the host: {e}"))?;
    let (mut send, mut recv) = connection.open_bi().await.map_err(|e| anyhow!("{e}"))?;
    write_frame(&mut send, &Wire::Hello { version: PROTOCOL_VERSION, room: encode_hex(&invite.room) }).await?;
    match read_frame(&mut recv).await.context("the host turned this connection away")? {
        Wire::Waiting => {}
        _ => bail!("the host did not follow the protocol"),
    }
    let _ = events.send(RoomEvent::Connected { words: safety_words(&connection)? });
    match read_frame(&mut recv).await.context("the host did not let you in")? {
        Wire::Welcome => {}
        _ => bail!("the host did not follow the protocol"),
    }
    let _ = events.send(RoomEvent::Admitted);
    loop {
        tokio::select! {
            control = controls.recv() => match control {
                Some(control) => write_frame(&mut send, &Wire::Control { control }).await?,
                None => return Ok(()),
            },
            frame = read_frame(&mut recv) => match frame {
                Ok(Wire::Load { offer }) => { let _ = events.send(RoomEvent::Load(offer)); }
                Ok(Wire::State { state }) => { let _ = events.send(RoomEvent::State(state)); }
                Ok(_) => bail!("the host did not follow the protocol"),
                // The host leaving ends the stream, which is a normal way for a room to end.
                Err(_) => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::RelayMode;

    async fn local_endpoint(secret: SecretKey) -> Endpoint {
        Endpoint::builder(presets::Minimal).secret_key(secret).alpns(vec![ALPN.to_vec()]).relay_mode(RelayMode::Disabled).bind().await.unwrap()
    }

    async fn next(events: &mut mpsc::UnboundedReceiver<RoomEvent>) -> RoomEvent {
        tokio::time::timeout(Duration::from_secs(15), events.recv()).await.expect("timed out waiting for an event").expect("channel closed")
    }

    struct Setup {
        host: Host,
        host_events: mpsc::UnboundedReceiver<RoomEvent>,
        host_addr: EndpointAddr,
    }

    async fn host() -> Setup {
        let secret = SecretKey::generate();
        let endpoint = local_endpoint(secret.clone()).await;
        let host_addr = endpoint.addr();
        let (tx, host_events) = mpsc::unbounded_channel();
        Setup { host: Host::on_endpoint(endpoint, secret, tx), host_events, host_addr }
    }

    async fn guest(secret: &SecretKey, addr: &EndpointAddr, invite: Invite) -> (Guest, mpsc::UnboundedReceiver<RoomEvent>) {
        let (tx, events) = mpsc::unbounded_channel();
        let endpoint = local_endpoint(secret.clone()).await;
        (Guest::on_endpoint(endpoint, addr.clone(), invite, tx), events)
    }

    fn offer() -> LoadOffer {
        LoadOffer { meta_id: "tt0063350".into(), video_id: "tt0063350".into(), kind: "movie".into(), title: "Night".into(), hash: None }
    }

    #[tokio::test]
    async fn invited_guest_joins_after_approval_and_receives_sync() {
        let mut s = host().await;
        let guest_key = SecretKey::generate();
        let code = s.host.invite(guest_key.public()).unwrap();
        let invite = Invite::open(&code, &guest_key.public(), now_seconds()).unwrap();
        let (joined, mut guest_events) = guest(&guest_key, &s.host_addr, invite).await;

        let RoomEvent::GuestWaiting { id, words: host_words } = next(&mut s.host_events).await else { panic!("expected a waiting guest") };
        assert_eq!(id, guest_key.public());
        let RoomEvent::Connected { words: guest_words } = next(&mut guest_events).await else { panic!("expected connected") };
        assert_eq!(host_words, guest_words, "both ends must derive the same safety words");
        assert_eq!(host_words.split(' ').count(), 4);

        s.host.decide(&id, true);
        assert_eq!(next(&mut guest_events).await, RoomEvent::Admitted);
        assert_eq!(next(&mut s.host_events).await, RoomEvent::GuestJoined { id });

        s.host.load(offer());
        assert_eq!(next(&mut guest_events).await, RoomEvent::Load(offer()));
        let sync = SyncState { position: 12.5, paused: true, speed: 1.0 };
        s.host.sync(sync);
        assert_eq!(next(&mut guest_events).await, RoomEvent::State(sync));

        joined.control(Control::Seek { position: 90.0 });
        assert_eq!(next(&mut s.host_events).await, RoomEvent::GuestControl { id, control: Control::Seek { position: 90.0 } });
    }

    /// The core promise: someone holding the real invite still cannot join.
    #[tokio::test]
    async fn stolen_invite_is_useless_to_anyone_else() {
        let mut s = host().await;
        let victim = SecretKey::generate();
        let code = s.host.invite(victim.public()).unwrap();
        let invite = Invite::open(&code, &victim.public(), now_seconds()).unwrap();

        // The client refuses a code made for someone else.
        let thief = SecretKey::generate();
        assert!(Invite::open(&code, &thief.public(), now_seconds()).is_err());

        // A modified client skips that check and presents the stolen invite anyway.
        let (_stolen, mut thief_events) = guest(&thief, &s.host_addr, invite.clone()).await;
        assert_eq!(next(&mut s.host_events).await, RoomEvent::Refused { id: thief.public() });
        let RoomEvent::Closed(_) = next(&mut thief_events).await else { panic!("the thief must get nothing but a closed connection") };

        // The thief's attempt did not use up the real guest's invite.
        let (_real, _events) = guest(&victim, &s.host_addr, invite).await;
        let RoomEvent::GuestWaiting { id, .. } = next(&mut s.host_events).await else { panic!("the real guest should still get in") };
        assert_eq!(id, victim.public());
    }

    #[tokio::test]
    async fn an_invite_works_only_once() {
        let mut s = host().await;
        let guest_key = SecretKey::generate();
        let code = s.host.invite(guest_key.public()).unwrap();
        let invite = Invite::open(&code, &guest_key.public(), now_seconds()).unwrap();
        let (first, _first_events) = guest(&guest_key, &s.host_addr, invite.clone()).await;
        let RoomEvent::GuestWaiting { .. } = next(&mut s.host_events).await else { panic!("expected waiting") };
        let (_second, _second_events) = guest(&guest_key, &s.host_addr, invite).await;
        assert_eq!(next(&mut s.host_events).await, RoomEvent::Refused { id: guest_key.public() });
        drop(first);
    }

    #[tokio::test]
    async fn denied_guest_learns_nothing_about_the_room() {
        let mut s = host().await;
        s.host.load(offer());
        let guest_key = SecretKey::generate();
        let code = s.host.invite(guest_key.public()).unwrap();
        let invite = Invite::open(&code, &guest_key.public(), now_seconds()).unwrap();
        let (_joined, mut guest_events) = guest(&guest_key, &s.host_addr, invite).await;
        let RoomEvent::GuestWaiting { id, .. } = next(&mut s.host_events).await else { panic!("expected waiting") };
        let RoomEvent::Connected { .. } = next(&mut guest_events).await else { panic!("expected connected") };
        s.host.decide(&id, false);
        let RoomEvent::Closed(_) = next(&mut guest_events).await else { panic!("a denied guest must not receive room content") };
    }

    #[tokio::test]
    async fn waiting_guest_receives_nothing_before_approval() {
        let mut s = host().await;
        let guest_key = SecretKey::generate();
        let code = s.host.invite(guest_key.public()).unwrap();
        let invite = Invite::open(&code, &guest_key.public(), now_seconds()).unwrap();
        let (_joined, mut guest_events) = guest(&guest_key, &s.host_addr, invite).await;
        let RoomEvent::GuestWaiting { .. } = next(&mut s.host_events).await else { panic!("expected waiting") };
        let RoomEvent::Connected { .. } = next(&mut guest_events).await else { panic!("expected connected") };
        s.host.load(offer());
        s.host.sync(SyncState { position: 1.0, paused: false, speed: 1.0 });
        assert!(tokio::time::timeout(Duration::from_millis(500), guest_events.recv()).await.is_err(), "nothing may arrive before approval");
    }

    #[test]
    fn forged_and_altered_invites_are_rejected() {
        let host_key = SecretKey::generate();
        let attacker = SecretKey::generate();
        let guest = SecretKey::generate();
        let invite = Invite { host: host_key.public(), guest: guest.public(), room: [7; 16], expires: 2000 };
        let good = invite.encode(&host_key);
        assert_eq!(Invite::open(&good, &guest.public(), 1000).unwrap(), invite);

        // Signed by someone other than the host it names.
        assert!(Invite::open(&invite.encode(&attacker), &guest.public(), 1000).is_err());

        // Every single changed character breaks it, including redirecting it to the attacker.
        let hex_start = INVITE_PREFIX.len();
        for at in (hex_start..good.len()).step_by(7) {
            let mut bad = good.clone().into_bytes();
            bad[at] = if bad[at] == b'0' { b'1' } else { b'0' };
            assert!(Invite::open(std::str::from_utf8(&bad).unwrap(), &guest.public(), 1000).is_err(), "changed byte {at} was accepted");
        }
        let redirected = Invite { guest: attacker.public(), ..invite.clone() };
        let spliced = format!("{}{}", &redirected.encode(&host_key)[..INVITE_PREFIX.len() + invite_body_len() * 2], &good[INVITE_PREFIX.len() + invite_body_len() * 2..]);
        assert!(Invite::open(&spliced, &attacker.public(), 1000).is_err());

        for junk in ["", "reel-invite-", "reel-invite-zz", "reel-invite-00", "password123", "reel-invite-é"] {
            assert!(Invite::open(junk, &guest.public(), 1000).is_err(), "{junk:?} was accepted");
        }
    }

    #[test]
    fn expired_invites_are_rejected() {
        let host_key = SecretKey::generate();
        let guest = SecretKey::generate();
        let code = Invite { host: host_key.public(), guest: guest.public(), room: [1; 16], expires: 1000 }.encode(&host_key);
        assert!(Invite::open(&code, &guest.public(), 1000).is_ok());
        assert!(Invite::open(&code, &guest.public(), 1001).is_err());
    }

    #[test]
    fn messages_cannot_smuggle_urls_paths_or_escape_codes() {
        for id in ["tt0063350", "tt0944947:1:2", "kitsu:123:4", "yt:aqz-KE-bpKQ"] {
            assert!(valid_content_id(id), "{id} should be allowed");
        }
        for id in [
            "", "http://evil.test/x", "file:///c:/secret", "local:C:\\Users\\me\\film.mkv", "tt", "tt1;calc", "tt1:2:3:4", "tt1 --script=evil.lua", "edl://x", "av://lavfi:x", "yt:short", "yt:../../../etc", "--vo=x", "tt0063350\n",
        ] {
            assert!(!valid_content_id(id), "{id:?} must be rejected");
        }
        let ok = offer();
        assert!(ok.is_valid());
        assert!(!LoadOffer { title: "x\u{1b}]52;c;Y2FsYw==\u{7}".into(), ..ok.clone() }.is_valid(), "terminal escape in title");
        assert!(!LoadOffer { title: "x".repeat(MAX_TITLE_CHARS + 1), ..ok.clone() }.is_valid());
        assert!(!LoadOffer { kind: "local".into(), ..ok.clone() }.is_valid());
        assert!(!LoadOffer { hash: Some("http://evil".into()), ..ok.clone() }.is_valid());
        assert!(LoadOffer { hash: Some("a".repeat(40)), ..ok }.is_valid());
        assert_eq!(clean_title("a\u{1b}[2Jb\nc"), "a[2Jbc");

        // Fields the protocol does not define are rejected outright.
        let wire = |json: &str| serde_json::from_str::<Wire>(json);
        assert_eq!(wire(r#""Waiting""#).unwrap(), Wire::Waiting);
        assert!(wire(r#"{"Waiting":{"url":"http://evil"}}"#).is_err());
        assert!(wire(r#"{"Hello":{"version":1,"room":"00","url":"http://evil"}}"#).is_err());
        assert!(wire(r#"{"Control":{"control":{"Seek":{"position":1.0,"url":"http://evil"}}}}"#).is_err());
        assert!(wire(r#"{"Load":{"offer":{"meta_id":"tt1","video_id":"tt1","kind":"movie","title":"x","hash":null,"url":"http://evil"}}}"#).is_err());
        assert!(wire(r#"{"Exec":{"command":"calc"}}"#).is_err());
        let seek = |position| Wire::Control { control: Control::Seek { position } }.is_valid();
        assert!(seek(10.0));
        assert!(!seek(-1.0));
        assert!(!seek(f64::INFINITY));
        assert!(!seek(f64::NAN));
    }

    #[test]
    fn reel_ids_round_trip_and_reject_junk() {
        let key = SecretKey::generate().public();
        assert_eq!(parse_id(&format!("  {}\n", format_id(&key))).unwrap(), key);
        for bad in ["", "reel-id-", "reel-id-xyz", "abc", &key.to_string()] {
            assert!(parse_id(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn identity_is_kept_between_runs() {
        let dir = std::env::temp_dir().join(format!("reel-identity-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("identity.key");
        let first = load_identity_from(&path).unwrap();
        assert_eq!(load_identity_from(&path).unwrap().public(), first.public());
        std::fs::write(&path, b"short").unwrap();
        assert!(load_identity_from(&path).is_err(), "a damaged key must not be silently replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Uses public relays and discovery, so it runs only on request:
    /// cargo test internet_join -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn internet_join_finds_the_host_from_the_invite_alone() {
        let (host_tx, mut host_events) = mpsc::unbounded_channel();
        let started = Instant::now();
        let host = Host::start(SecretKey::generate(), host_tx).await.unwrap();
        host.endpoint.online().await;
        println!("host online in {} ms", started.elapsed().as_millis());
        let guest_key = SecretKey::generate();
        let code = host.invite(guest_key.public()).unwrap();
        println!("invite is {} characters", code.len());

        let started = Instant::now();
        let (guest_tx, mut guest_events) = mpsc::unbounded_channel();
        let joined = Guest::join(guest_key.clone(), &code, guest_tx).await.unwrap();
        let RoomEvent::GuestWaiting { id, words } = next(&mut host_events).await else { panic!("expected a waiting guest") };
        println!("guest reached host in {} ms, safety words: {words}", started.elapsed().as_millis());
        let RoomEvent::Connected { words: guest_words } = next(&mut guest_events).await else { panic!("expected connected") };
        assert_eq!(words, guest_words);
        host.decide(&id, true);
        assert_eq!(next(&mut guest_events).await, RoomEvent::Admitted);
        host.load(offer());
        assert_eq!(next(&mut guest_events).await, RoomEvent::Load(offer()));
        joined.close().await;
        host.close().await;
    }

    #[test]
    fn safety_word_list_has_no_duplicates() {
        let mut words = WORDS.to_vec();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), 256);
    }
}
