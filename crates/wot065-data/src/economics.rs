//! Economics configuration and shop/pricing models for WoT 0.6.5.
//!
//! Mirrors `.scratch/research/decompiled/common/Economics.py` and
//! `.scratch/research/decompiled/client/account_helpers/Shop.py`.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EconomicsError {
    InvalidFreeXpConversion { discrecity: i32, cost: i32 },
    InvalidTankmanCost(String),
    InvalidSlotsPrices(String),
    InvalidBerthsPrices(String),
    InvalidExchangeRate(i32),
    InvalidSellPriceModif(String),
    Io(String),
    Json(String),
}

impl fmt::Display for EconomicsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFreeXpConversion { discrecity, cost } => {
                write!(f, "invalid free XP conversion ({discrecity}, {cost}): discrecity must be > 0 and cost >= 0")
            }
            Self::InvalidTankmanCost(msg) => write!(f, "invalid tankman cost: {msg}"),
            Self::InvalidSlotsPrices(msg) => write!(f, "invalid slots prices: {msg}"),
            Self::InvalidBerthsPrices(msg) => write!(f, "invalid berths prices: {msg}"),
            Self::InvalidExchangeRate(rate) => write!(f, "invalid exchange rate {rate}: must be > 0"),
            Self::InvalidSellPriceModif(msg) => write!(f, "invalid sell price modif: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON error: {err}"),
        }
    }
}

impl std::error::Error for EconomicsError {}

/// Tankman training cost tier matching `common/Economics.py:125-143`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TankmanCostTier {
    pub role_level: u8,
    pub credits: i32,
    pub gold: i32,
    pub base_role_loss: f64,
    pub class_change_role_loss: f64,
    pub is_premium: bool,
}

/// Trade fees matching `common/Economics.py:52-53`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeFees {
    pub default: (f64, f64),
    #[serde(rename = "sameClan")]
    pub same_clan: (f64, f64),
}

impl Default for TradeFees {
    fn default() -> Self {
        Self {
            default: (0.05, 0.05),
            same_clan: (0.0, 0.0),
        }
    }
}

/// Starting currencies and slot/berth allowances for a profile preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetStarting {
    pub credits: i64,
    pub gold: i64,
    pub free_xp: i64,
    pub slots: i32,
    pub berths: i32,
    #[serde(default)]
    pub starter_veh_xp: i32,
    #[serde(default)]
    pub starter_tman_free_xp: i32,
}

/// Master economics config with vanilla WoT 0.6.5 defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EconomicsConfig {
    pub sell_price_modif: f64,
    pub exchange_rate: i32,
    pub free_xp_conversion: (i32, i32),
    pub slots_prices: (i32, Vec<i32>),
    pub berths_prices: (i32, i32, Vec<i32>),
    pub premium_cost: BTreeMap<i32, i32>,
    pub tankman_cost: Vec<TankmanCostTier>,
    pub passport_change_cost: i32,
    pub paid_removal_cost: i32,
    pub trade_fees: TradeFees,
    pub retail_starting: PresetStarting,
    pub sandbox_starting: PresetStarting,
}

impl Default for EconomicsConfig {
    fn default() -> Self {
        Self {
            sell_price_modif: 0.5,
            exchange_rate: 400,
            free_xp_conversion: (25, 1),
            slots_prices: (6, vec![300]),
            berths_prices: (16, 16, vec![300]),
            premium_cost: BTreeMap::from([
                (1, 250),
                (3, 650),
                (7, 1250),
                (30, 2500),
                (180, 13500),
                (360, 24000),
            ]),
            tankman_cost: vec![
                TankmanCostTier {
                    role_level: 50,
                    credits: 0,
                    gold: 0,
                    base_role_loss: 0.2,
                    class_change_role_loss: 0.4,
                    is_premium: false,
                },
                TankmanCostTier {
                    role_level: 75,
                    credits: 20_000,
                    gold: 0,
                    base_role_loss: 0.1,
                    class_change_role_loss: 0.2,
                    is_premium: false,
                },
                TankmanCostTier {
                    role_level: 100,
                    credits: 0,
                    gold: 200,
                    base_role_loss: 0.0,
                    class_change_role_loss: 0.0,
                    is_premium: true,
                },
            ],
            passport_change_cost: 50,
            paid_removal_cost: 10,
            trade_fees: TradeFees::default(),
            retail_starting: PresetStarting {
                credits: 10_000,
                gold: 0,
                free_xp: 0,
                slots: 6,
                berths: 16,
                starter_veh_xp: 0,
                starter_tman_free_xp: 0,
            },
            sandbox_starting: PresetStarting {
                credits: 50_000_000,
                gold: 1_000_000,
                free_xp: 5_000_000,
                slots: 6,
                berths: 16,
                starter_veh_xp: 500_000,
                starter_tman_free_xp: 1_000_000,
            },
        }
    }
}

impl EconomicsConfig {
    pub fn validate(&self) -> Result<(), EconomicsError> {
        if self.free_xp_conversion.0 <= 0 || self.free_xp_conversion.1 < 0 {
            return Err(EconomicsError::InvalidFreeXpConversion {
                discrecity: self.free_xp_conversion.0,
                cost: self.free_xp_conversion.1,
            });
        }
        if self.exchange_rate <= 0 {
            return Err(EconomicsError::InvalidExchangeRate(self.exchange_rate));
        }
        if self.sell_price_modif < 0.0 || self.sell_price_modif.is_nan() {
            return Err(EconomicsError::InvalidSellPriceModif(format!(
                "{}",
                self.sell_price_modif
            )));
        }
        if self.slots_prices.0 < 0 || self.slots_prices.1.is_empty() {
            return Err(EconomicsError::InvalidSlotsPrices(
                "slots_prices initial slots must be >= 0 and prices list non-empty".into(),
            ));
        }
        if self.berths_prices.0 < 0 || self.berths_prices.1 <= 0 || self.berths_prices.2.is_empty() {
            return Err(EconomicsError::InvalidBerthsPrices(
                "berths_prices pack size must be > 0 and prices list non-empty".into(),
            ));
        }
        if self.tankman_cost.len() != 3 {
            return Err(EconomicsError::InvalidTankmanCost(format!(
                "expected exactly 3 tankman cost tiers, got {}",
                self.tankman_cost.len()
            )));
        }
        if !self
            .tankman_cost
            .windows(2)
            .all(|w| w[0].role_level < w[1].role_level)
        {
            return Err(EconomicsError::InvalidTankmanCost(
                "tankman cost tiers must be strictly ascending by roleLevel".into(),
            ));
        }
        if self.tankman_cost[0].credits != 0 || self.tankman_cost[0].gold != 0 {
            return Err(EconomicsError::InvalidTankmanCost(
                "first tankman tier must be free (0 credits, 0 gold)".into(),
            ));
        }
        Ok(())
    }

    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self, EconomicsError> {
        let text = fs::read_to_string(path.as_ref())
            .map_err(|e| EconomicsError::Io(format!("{}: {e}", path.as_ref().display())))?;
        let config: Self = serde_json::from_str(&text)
            .map_err(|e| EconomicsError::Json(format!("{}: {e}", path.as_ref().display())))?;
        config.validate()?;
        Ok(config)
    }

    pub fn next_slot_price(&self, current_slots: i32) -> i32 {
        if current_slots < self.slots_prices.0 {
            return 0;
        }
        let add = current_slots - self.slots_prices.0;
        if (add as usize) < self.slots_prices.1.len() {
            self.slots_prices.1[add as usize]
        } else {
            *self.slots_prices.1.last().unwrap_or(&0)
        }
    }

    pub fn next_berth_pack_price(&self, current_berths: i32) -> i32 {
        if self.berths_prices.1 <= 0 || current_berths < self.berths_prices.0 {
            return 0;
        }
        let add = (current_berths - self.berths_prices.0) / self.berths_prices.1;
        if (add as usize) < self.berths_prices.2.len() {
            self.berths_prices.2[add as usize]
        } else {
            *self.berths_prices.2.last().unwrap_or(&0)
        }
    }

    pub fn sell_price_credits(&self, buy_credits: i64, buy_gold: i64) -> i64 {
        let gross = buy_credits as f64 + (buy_gold * self.exchange_rate as i64) as f64;
        (self.sell_price_modif * gross).ceil() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_economics_default_and_validation() {
        let config = EconomicsConfig::default();
        assert!(config.validate().is_ok());

        assert_eq!(config.free_xp_conversion, (25, 1));
        assert_eq!(config.berths_prices, (16, 16, vec![300]));
        assert_eq!(config.slots_prices, (6, vec![300]));
        assert_eq!(config.tankman_cost.len(), 3);

        let t0 = &config.tankman_cost[0];
        assert_eq!(t0.role_level, 50);
        assert_eq!(t0.credits, 0);
        assert_eq!(t0.gold, 0);
        assert_eq!(t0.base_role_loss, 0.2);
        assert_eq!(t0.class_change_role_loss, 0.4);
        assert!(!t0.is_premium);

        let t1 = &config.tankman_cost[1];
        assert_eq!(t1.role_level, 75);
        assert_eq!(t1.credits, 20_000);
        assert_eq!(t1.gold, 0);
        assert_eq!(t1.base_role_loss, 0.1);
        assert_eq!(t1.class_change_role_loss, 0.2);
        assert!(!t1.is_premium);

        let t2 = &config.tankman_cost[2];
        assert_eq!(t2.role_level, 100);
        assert_eq!(t2.credits, 0);
        assert_eq!(t2.gold, 200);
        assert_eq!(t2.base_role_loss, 0.0);
        assert_eq!(t2.class_change_role_loss, 0.0);
        assert!(t2.is_premium);
    }

    #[test]
    fn test_economics_invalid_overrides() {
        let mut cfg = EconomicsConfig::default();
        cfg.free_xp_conversion = (0, 1);
        assert!(matches!(
            cfg.validate(),
            Err(EconomicsError::InvalidFreeXpConversion { .. })
        ));

        let mut cfg = EconomicsConfig::default();
        cfg.tankman_cost.swap(0, 1);
        assert!(matches!(
            cfg.validate(),
            Err(EconomicsError::InvalidTankmanCost(_))
        ));

        let mut cfg = EconomicsConfig::default();
        cfg.tankman_cost.pop();
        assert!(matches!(
            cfg.validate(),
            Err(EconomicsError::InvalidTankmanCost(_))
        ));

        let mut cfg = EconomicsConfig::default();
        cfg.slots_prices.1.clear();
        assert!(matches!(
            cfg.validate(),
            Err(EconomicsError::InvalidSlotsPrices(_))
        ));

        let mut cfg = EconomicsConfig::default();
        cfg.berths_prices.1 = 0;
        assert!(matches!(
            cfg.validate(),
            Err(EconomicsError::InvalidBerthsPrices(_))
        ));

        let mut cfg = EconomicsConfig::default();
        cfg.exchange_rate = 0;
        assert!(matches!(
            cfg.validate(),
            Err(EconomicsError::InvalidExchangeRate(0))
        ));
    }

    #[test]
    fn test_economics_pricing_helpers() {
        let cfg = EconomicsConfig::default();

        // Slots: initial = 6, price = 300
        assert_eq!(cfg.next_slot_price(5), 0);
        assert_eq!(cfg.next_slot_price(6), 300);
        assert_eq!(cfg.next_slot_price(10), 300);

        // Berths: initial = 16, pack = 16, price = 300
        assert_eq!(cfg.next_berth_pack_price(15), 0);
        assert_eq!(cfg.next_berth_pack_price(16), 300);
        assert_eq!(cfg.next_berth_pack_price(31), 300);
        assert_eq!(cfg.next_berth_pack_price(32), 300);

        // Sell price: ceil(0.5 * (credits + gold * 400))
        assert_eq!(cfg.sell_price_credits(1000, 0), 500);
        assert_eq!(cfg.sell_price_credits(1, 0), 1);
        assert_eq!(cfg.sell_price_credits(0, 1), 200);
        assert_eq!(cfg.sell_price_credits(1000, 10), 2500);
    }
}
