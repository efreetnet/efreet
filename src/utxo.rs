use std::collections::BTreeMap;
use std::fmt;

use pallas_crypto::hash::Hash;
use pallas_network::miniprotocols::Point;
use pallas_primitives::conway::DatumOption;
use pallas_traverse::MultiEraOutput;

/// A reference to a transaction output: `tx_hash#index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputRef {
    pub tx_hash: Hash<32>,
    pub index: u64,
}

impl OutputRef {
    pub fn new(tx_hash: Hash<32>, index: u64) -> Self {
        Self { tx_hash, index }
    }
}

impl fmt::Display for OutputRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.tx_hash, self.index)
    }
}

/// A native asset held by an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub policy: Hash<28>,
    pub name: Vec<u8>,
    pub quantity: u64,
}

/// The datum attached to an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Datum {
    Hash(Hash<32>),
    /// An inline datum, as the raw CBOR of its `PlutusData`.
    Inline(Vec<u8>),
}

/// An unspent output sitting at the watched address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utxo {
    pub output_ref: OutputRef,
    pub lovelace: u64,
    pub assets: Vec<Asset>,
    pub datum: Option<Datum>,
    /// The output's raw CBOR, for anything not surfaced above (e.g. reference
    /// scripts). Decode it with `pallas_traverse::MultiEraOutput::decode`.
    pub cbor: Vec<u8>,
}

impl Utxo {
    pub(crate) fn new(output_ref: OutputRef, output: &MultiEraOutput, cbor: Vec<u8>) -> Self {
        let value = output.value();

        let assets = value
            .assets()
            .iter()
            .flat_map(|policy| policy.assets())
            .filter_map(|asset| {
                Some(Asset {
                    policy: *asset.policy(),
                    name: asset.name().to_vec(),
                    quantity: asset.output_coin()?,
                })
            })
            .collect();

        let datum = output.datum().map(|datum| match datum {
            DatumOption::Hash(hash) => Datum::Hash(hash),
            DatumOption::Data(data) => Datum::Inline(data.0.raw_cbor().to_vec()),
        });

        Self {
            output_ref,
            lovelace: value.coin(),
            assets,
            datum,
            cbor,
        }
    }
}

pub(crate) type Utxos = BTreeMap<OutputRef, Utxo>;

/// A change to the watched address's UTxOs.
#[derive(Debug, Clone)]
pub struct Update {
    /// The chain point the watcher's UTxO set now reflects.
    pub point: Point,
    /// Whether this change came from a chain rollback rather than a new block.
    pub rollback: bool,
    /// UTxOs that appeared at the address.
    pub added: Vec<Utxo>,
    /// UTxOs that left the address: spent, or undone by a rollback.
    pub removed: Vec<Utxo>,
}

impl Update {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}
