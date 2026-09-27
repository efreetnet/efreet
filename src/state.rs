use pallas_codec::minicbor::{self, Decoder};
use pallas_codec::utils::AnyCbor;
use pallas_crypto::hash::Hash;
use pallas_network::miniprotocols::Point;
use pallas_traverse::{Era, MultiEraBlock, MultiEraOutput};

use crate::utxo::{OutputRef, Update, Utxo, Utxos};

/// The watched address's UTxO set as of `point`, kept separate from the
/// network plumbing so it can be driven by plain blocks and query results.
pub(crate) struct State {
    pub address: Vec<u8>,
    pub point: Point,
    pub utxos: Utxos,
}

impl State {
    /// Applies a block that extends the chain at `self.point`.
    pub fn roll_forward(&mut self, block: &MultiEraBlock) -> Update {
        let mut added = Utxos::new();
        let mut removed = Vec::new();

        for tx in block.txs() {
            // `consumes`/`produces` account for phase-2 failures, where only
            // collateral is spent and only the collateral return is created.
            for input in tx.consumes() {
                let output_ref = OutputRef::new(*input.hash(), input.index());
                if let Some(utxo) = self.utxos.remove(&output_ref) {
                    // Created and spent within this block: net zero.
                    if added.remove(&output_ref).is_none() {
                        removed.push(utxo);
                    }
                }
            }

            let tx_hash = tx.hash();
            for (index, output) in tx.produces() {
                if output.address().is_ok_and(|a| a.to_vec() == self.address) {
                    let output_ref = OutputRef::new(tx_hash, index as u64);
                    let utxo = Utxo::new(output_ref, &output, output.encode());
                    self.utxos.insert(output_ref, utxo.clone());
                    added.insert(output_ref, utxo);
                }
            }
        }

        self.point = Point::Specific(block.slot(), block.hash().to_vec());

        Update {
            point: self.point.clone(),
            rollback: false,
            added: added.into_values().collect(),
            removed,
        }
    }

    /// Replaces the UTxO set with `utxos`, as queried at `point` after the
    /// chain rolled back to it.
    pub fn roll_back(&mut self, point: Point, utxos: Utxos) -> Update {
        let added = utxos
            .values()
            .filter(|utxo| !self.utxos.contains_key(&utxo.output_ref))
            .cloned()
            .collect();

        let previous = std::mem::replace(&mut self.utxos, utxos);
        let removed = previous
            .into_values()
            .filter(|utxo| !self.utxos.contains_key(&utxo.output_ref))
            .collect();

        self.point = point;

        Update {
            point: self.point.clone(),
            rollback: true,
            added,
            removed,
        }
    }
}

/// Decodes a `GetUTxOByAddress` result: `[{ [tx_hash, index] => output }]`.
pub(crate) fn decode_utxos(cbor: &[u8]) -> Result<Utxos, minicbor::decode::Error> {
    let mut d = Decoder::new(cbor);
    d.array()?;

    let mut utxos = Utxos::new();

    for entry in d.map_iter::<(Hash<32>, u64), AnyCbor>()? {
        let ((tx_hash, index), output) = entry?;
        let output_ref = OutputRef::new(tx_hash, index);

        // Conway's output decoder accepts every post-Byron output shape.
        let cbor = output.unwrap();
        let output = MultiEraOutput::decode(Era::Conway, &cbor)?;
        utxos.insert(output_ref, Utxo::new(output_ref, &output, cbor.clone()));
    }

    Ok(utxos)
}

#[cfg(test)]
mod tests {
    use pallas_addresses::Address;
    use pallas_codec::minicbor::{Encoder, data::IanaTag};

    use super::*;
    use crate::utxo::{Asset, Datum};

    const ADDRESS: &str = "addr_test1vpfwv0ezc5g8a4mkku8hhy3y3vp92t7s3ul8g778g5yegsgalc6gc";

    fn address() -> Vec<u8> {
        Address::from_bech32(ADDRESS).unwrap().to_vec()
    }

    fn hash(s: &str) -> Hash<32> {
        s.parse().unwrap()
    }

    fn utxo(tx_hash: Hash<32>, index: u64, lovelace: u64) -> Utxo {
        Utxo {
            output_ref: OutputRef::new(tx_hash, index),
            lovelace,
            assets: vec![],
            datum: None,
            cbor: vec![],
        }
    }

    fn state(address: Vec<u8>, utxos: impl IntoIterator<Item = Utxo>) -> State {
        State {
            address,
            point: Point::Origin,
            utxos: utxos.into_iter().map(|u| (u.output_ref, u)).collect(),
        }
    }

    // A Conway testnet block at slot 22075282 with a single tx spending af09..#0 and paying
    // 5220878836 lovelace plus one native asset to ADDRESS.
    fn with_block<T>(f: impl FnOnce(&MultiEraBlock) -> T) -> T {
        let cbor = hex::decode(include_str!("../test_data/conway1.block").trim()).unwrap();
        f(&MultiEraBlock::decode(&cbor).unwrap())
    }

    #[test]
    fn roll_forward_tracks_spent_and_created_outputs() {
        let spent = hash("af09d312a642fecb47da719156517bec678469c15789bcf002ce2ef563edf542");
        let mut state = state(address(), [utxo(spent, 0, 42)]);

        let update = with_block(|block| state.roll_forward(block));

        assert!(!update.rollback);
        assert_eq!(update.removed, vec![utxo(spent, 0, 42)]);
        assert_eq!(update.added.len(), 1);

        let created = &update.added[0];
        let tx = hash("ed8431dbe32cff36814ee838a7a002152d43a7465faaf05529907717c793527a");
        assert_eq!(created.output_ref, OutputRef::new(tx, 0));
        assert_eq!(created.lovelace, 5220878836);
        assert_eq!(created.assets.len(), 1);
        assert_eq!(created.datum, None);

        assert_eq!(
            state.utxos.keys().collect::<Vec<_>>(),
            vec![&created.output_ref]
        );
        assert_eq!(
            update.point,
            Point::Specific(
                22075282,
                hash("9b51ccd4f161c08382a445684ff3eb788923608acbea283081fa5ccf663fef8d").to_vec()
            )
        );
        assert_eq!(state.point, update.point);
    }

    #[test]
    fn roll_forward_ignores_other_addresses() {
        let mut state = state(vec![0x60; 29], []);

        let update = with_block(|block| state.roll_forward(block));

        assert!(update.is_empty());
        assert!(state.utxos.is_empty());
        assert_eq!(state.point, update.point);
    }

    #[test]
    fn roll_back_diffs_against_requeried_set() {
        let (a, b, c) = (Hash::new([1; 32]), Hash::new([2; 32]), Hash::new([3; 32]));
        let mut state = state(address(), [utxo(a, 0, 1), utxo(b, 0, 2)]);

        let requeried = [utxo(b, 0, 2), utxo(c, 1, 3)];
        let point = Point::Specific(7, vec![7; 32]);
        let update = state.roll_back(
            point.clone(),
            requeried
                .iter()
                .map(|u| (u.output_ref, u.clone()))
                .collect(),
        );

        assert!(update.rollback);
        assert_eq!(update.point, point);
        assert_eq!(update.added, vec![utxo(c, 1, 3)]);
        assert_eq!(update.removed, vec![utxo(a, 0, 1)]);
        assert_eq!(state.utxos.values().cloned().collect::<Vec<_>>(), requeried);
        assert_eq!(state.point, point);
    }

    #[test]
    fn decodes_utxo_by_address_result() {
        let address = address();
        let policy = [9; 28];
        let (a, b) = (Hash::new([1; 32]), Hash::new([2; 32]));

        let mut e = Encoder::new(Vec::new());
        e.array(1).unwrap().map(2).unwrap();
        // Legacy (array) output: [address, coin]
        e.array(2)
            .unwrap()
            .bytes(a.as_ref())
            .unwrap()
            .u64(0)
            .unwrap();
        e.array(2)
            .unwrap()
            .bytes(&address)
            .unwrap()
            .u64(1_000_000)
            .unwrap();
        // Post-Alonzo (map) output with a native asset and an inline datum.
        e.array(2)
            .unwrap()
            .bytes(b.as_ref())
            .unwrap()
            .u64(3)
            .unwrap();
        e.map(3).unwrap();
        e.u8(0).unwrap().bytes(&address).unwrap();
        e.u8(1).unwrap().array(2).unwrap().u64(2_000_000).unwrap();
        e.map(1).unwrap().bytes(&policy).unwrap();
        e.map(1).unwrap().bytes(b"efreet").unwrap().u64(5).unwrap();
        e.u8(2).unwrap().array(2).unwrap().u8(1).unwrap();
        e.tag(IanaTag::Cbor)
            .unwrap()
            .bytes(&[0xd8, 0x79, 0x80])
            .unwrap();

        let utxos = decode_utxos(&e.into_writer()).unwrap();
        let utxos: Vec<_> = utxos.into_values().collect();

        assert_eq!(utxos.len(), 2);

        assert_eq!(utxos[0].output_ref, OutputRef::new(a, 0));
        assert_eq!(utxos[0].lovelace, 1_000_000);
        assert!(utxos[0].assets.is_empty());
        assert_eq!(utxos[0].datum, None);

        assert_eq!(utxos[1].output_ref, OutputRef::new(b, 3));
        assert_eq!(utxos[1].lovelace, 2_000_000);
        assert_eq!(
            utxos[1].assets,
            vec![Asset {
                policy: Hash::new(policy),
                name: b"efreet".to_vec(),
                quantity: 5,
            }]
        );
        assert_eq!(utxos[1].datum, Some(Datum::Inline(vec![0xd8, 0x79, 0x80])));
    }
}
