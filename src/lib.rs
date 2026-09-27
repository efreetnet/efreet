//! Watch the UTxOs of a Cardano address by plugging into a local node over the
//! node-to-client (N2C) mini-protocols.
//!
//! On connect, efreet snapshots the address's UTxOs at the node's tip with a
//! local-state query, then follows the chain with chain-sync: every new block
//! is scanned for outputs paying to the address and for inputs spending its
//! UTxOs. On a rollback, the set is re-queried at the point the chain rolled
//! back to.
//!
//! ```no_run
//! # async fn run() -> Result<(), efreet::Error> {
//! let mut watcher = efreet::Watcher::connect(
//!     "/opt/cardano/ipc/node.socket", // or `\\.\pipe\cardano-node` on Windows
//!     efreet::PREPROD_MAGIC,
//!     "addr_test1vz...",
//! )
//! .await?;
//!
//! println!("{} lovelace", watcher.lovelace());
//!
//! loop {
//!     let update = watcher.next().await?;
//!     for utxo in &update.added {
//!         println!("+ {} {} lovelace", utxo.output_ref, utxo.lovelace);
//!     }
//!     for utxo in &update.removed {
//!         println!("- {} {} lovelace", utxo.output_ref, utxo.lovelace);
//!     }
//! }
//! # }
//! ```

use std::path::Path;

use pallas_addresses::Address;
use pallas_codec::minicbor;
use pallas_codec::utils::AnyCbor;
use pallas_network::facades::NodeClient;
use pallas_network::miniprotocols::chainsync::{self, NextResponse};
use pallas_network::miniprotocols::localstate::{
    self,
    queries_v16::{self, BlockQuery, LedgerQuery, Request},
};
use pallas_traverse::MultiEraBlock;

mod state;
mod utxo;

use state::State;

pub use pallas_crypto::hash::Hash;
pub use pallas_network::miniprotocols::{MAINNET_MAGIC, PREPROD_MAGIC, PREVIEW_MAGIC, Point};
pub use utxo::{Asset, Datum, OutputRef, Update, Utxo};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid address: {0}")]
    Address(#[from] pallas_addresses::Error),

    #[error("stake addresses hold no UTxOs")]
    StakeAddress,

    #[error("node connection failed: {0}")]
    Connect(#[from] pallas_network::facades::Error),

    #[error("chain-sync failed: {0}")]
    ChainSync(#[from] chainsync::ClientError),

    #[error("local state query failed: {0}")]
    StateQuery(#[from] localstate::ClientError),

    #[error("invalid block: {0}")]
    Block(#[from] pallas_traverse::Error),

    #[error("invalid query result: {0}")]
    Cbor(#[from] minicbor::decode::Error),
}

/// Follows a node's chain and tracks the UTxOs held by one address.
pub struct Watcher {
    client: NodeClient,
    state: State,
}

impl Watcher {
    /// Connects to the node's local socket (a named pipe on Windows), and
    /// snapshots `address`'s UTxOs at the current tip.
    ///
    /// `address` may be bech32 (Shelley) or base58 (Byron).
    pub async fn connect(
        socket: impl AsRef<Path>,
        magic: u64,
        address: &str,
    ) -> Result<Self, Error> {
        let address: Address = address.parse()?;
        if matches!(address, Address::Stake(_)) {
            return Err(Error::StakeAddress);
        }
        let address = address.to_vec();

        #[cfg(unix)]
        let mut client = NodeClient::connect(socket.as_ref(), magic).await?;
        #[cfg(windows)]
        let mut client = NodeClient::connect(socket.as_ref().as_os_str(), magic).await?;

        let point = client.chainsync().intersect_tip().await?;
        let utxos = query_utxos(&mut client, &address, point.clone()).await?;

        Ok(Self {
            client,
            state: State {
                address,
                point,
                utxos,
            },
        })
    }

    /// The chain point the current UTxO set reflects.
    pub fn point(&self) -> &Point {
        &self.state.point
    }

    /// The address's current UTxOs, ordered by output reference.
    pub fn utxos(&self) -> impl Iterator<Item = &Utxo> {
        self.state.utxos.values()
    }

    /// The total lovelace held by the address.
    pub fn lovelace(&self) -> u64 {
        self.utxos().map(|utxo| utxo.lovelace).sum()
    }

    /// Waits for the next change to the address's UTxOs. Blocks that don't
    /// touch the address are consumed silently.
    ///
    /// Not cancel-safe: if this future is dropped before it completes, the
    /// connection is left mid-protocol and the watcher must be discarded.
    pub async fn next(&mut self) -> Result<Update, Error> {
        loop {
            let update = match self.client.chainsync().request_or_await_next().await? {
                NextResponse::RollForward(block, _) => {
                    self.state.roll_forward(&MultiEraBlock::decode(&block.0)?)
                }
                // Chain-sync always opens with a rollback to the intersection.
                NextResponse::RollBackward(point, _) if point == self.state.point => continue,
                NextResponse::RollBackward(point, _) => {
                    let utxos =
                        query_utxos(&mut self.client, &self.state.address, point.clone()).await?;
                    self.state.roll_back(point, utxos)
                }
                NextResponse::Await => continue,
            };

            if !update.is_empty() {
                return Ok(update);
            }
        }
    }

    /// Closes the connection to the node.
    pub async fn close(self) {
        self.client.abort().await;
    }
}

/// Queries the UTxOs held by `address` in the ledger state at `point`.
async fn query_utxos(
    client: &mut NodeClient,
    address: &[u8],
    point: Point,
) -> Result<utxo::Utxos, Error> {
    let client = client.statequery();
    client.acquire(Some(point)).await?;

    let era = queries_v16::get_current_era(client).await?;
    let query = BlockQuery::GetUTxOByAddress(vec![address.to_vec().into()]);
    let query = Request::LedgerQuery(LedgerQuery::BlockQuery(era, query));
    let result = client.query_any(AnyCbor::from_encode(query)).await?;

    client.send_release().await?;

    Ok(state::decode_utxos(&result)?)
}
