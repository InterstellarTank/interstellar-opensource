//! WoT 0.6.5 account/garage data layer.
//!
//! This crate produces the data blobs the retail 0.6.5 client's Python 2.6 `Account`
//! entity expects during login and in the hangar:
//!
//! * [`catalog`] - parses the game's own `item_defs/vehicles` tree (packed BigWorld XML,
//!   read via `wg-toolkit`'s `pxml`, with a plain-XML fallback) into the full vehicle
//!   catalog: nation, tier, price, tags, stock component ids and both compact descriptors.
//! * [`account`] - builds the CMD_SYNC_DATA full diff, `showGUI` ctx, shop cache and
//!   dossier stream payloads as Python-2 protocol-2 pickles (plus BigWorld stream
//!   descriptions).
//! * [`pickle`] - a hand-rolled Python-2 protocol-2 pickle writer (with a reader for
//!   tests); no Python is needed at runtime.
//! * [`xml`] - BigWorld packed-XML / plain-XML tree reader.
//!
//! The client-side contract that dictates the exact payload shapes is documented in
//! `docs/research/client-analysis.md` and in the README of this crate, with line
//! citations into the decompiled client sources under `.scratch/research/decompiled/`.
pub mod account;
pub mod account_state;
pub mod catalog;
pub mod descr;
pub mod dossier;
pub mod economics;
pub mod item_defs;
pub mod ops;
pub mod pickle;
pub mod store;
pub mod testutil;
pub mod xml;
