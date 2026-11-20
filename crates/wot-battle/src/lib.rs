//! Pure-Rust World of Tanks 0.6.5 battle simulation and retail asset loaders.

pub mod assets;
pub mod balance;
pub mod lobby;
pub mod sim;
pub mod world;

pub use assets::AssetError;
pub use balance::{BalanceConfig, MatchmakerBalanceConfig, TerrainResistanceDefaults};
