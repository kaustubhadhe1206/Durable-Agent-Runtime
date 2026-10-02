//! Token-based cost calculation. Prices live in `pricing.toml`, never in code.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

// [Phase 3]
/// The bundled `pricing.toml`, compiled into the binary.
const DEFAULT_PRICING_TOML: &str = include_str!("../pricing.toml");

// [Phase 3]
#[derive(Debug, thiserror::Error)]
pub enum PricingError {
    #[error("could not read pricing file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid pricing file: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("no price listed for model {0:?} in pricing.toml")]
    UnknownModel(String),
}

// [Phase 3]
/// Price of one model, in USD per 1 million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct ModelPrice {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

// [Phase 3]
/// A table of model prices loaded from TOML.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pricing {
    models: HashMap<String, ModelPrice>,
}

// [Phase 3]
impl Pricing {
    // [Phase 3]
    /// The prices bundled with this crate (`crates/durable-llm/pricing.toml`).
    pub fn bundled() -> Result<Self, PricingError> {
        Self::from_toml_str(DEFAULT_PRICING_TOML)
    }

    // [Phase 3]
    /// Loads prices from a TOML file on disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, PricingError> {
        Self::from_toml_str(&std::fs::read_to_string(path)?)
    }

    // [Phase 3]
    pub fn from_toml_str(s: &str) -> Result<Self, PricingError> {
        Ok(toml::from_str(s)?)
    }

    // [Phase 3]
    pub fn price(&self, model: &str) -> Result<ModelPrice, PricingError> {
        self.models
            .get(model)
            .copied()
            .ok_or_else(|| PricingError::UnknownModel(model.to_string()))
    }

    // [Phase 3]
    /// Cost in USD of a call to `model` that used the given token counts.
    pub fn cost_usd(
        &self,
        model: &str,
        tokens_in: u64,
        tokens_out: u64,
    ) -> Result<f64, PricingError> {
        let p = self.price(model)?;
        // u64 -> f64 is exact for any realistic token count (below 2^53).
        Ok(
            (tokens_in as f64 * p.input_per_million + tokens_out as f64 * p.output_per_million)
                / 1_000_000.0,
        )
    }
}

// [Phase 3]
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [models.cheap]
        input_per_million = 1.0
        output_per_million = 2.0
    "#;

    #[test]
    fn cost_is_computed_from_token_counts() {
        let p = Pricing::from_toml_str(SAMPLE).unwrap();
        // 1M in at $1 + 500k out at $2 = $2
        let cost = p.cost_usd("cheap", 1_000_000, 500_000).unwrap();
        assert!((cost - 2.0).abs() < 1e-12, "got {cost}");
        assert_eq!(p.cost_usd("cheap", 0, 0).unwrap(), 0.0);
    }

    #[test]
    fn unknown_model_is_an_error() {
        let p = Pricing::from_toml_str(SAMPLE).unwrap();
        assert!(matches!(
            p.cost_usd("nope", 1, 1),
            Err(PricingError::UnknownModel(m)) if m == "nope"
        ));
    }

    #[test]
    fn invalid_toml_is_an_error() {
        assert!(matches!(
            Pricing::from_toml_str("models = 3"),
            Err(PricingError::Parse(_))
        ));
    }

    #[test]
    fn bundled_table_covers_default_model_and_dated_names() {
        let p = Pricing::bundled().unwrap();
        for model in [
            crate::mistral::DEFAULT_MODEL,
            "ministral-8b-2512",
            "ministral-3b-latest",
            "ministral-14b-latest",
        ] {
            assert!(p.price(model).is_ok(), "missing price for {model}");
        }
        assert_eq!(
            p.price("ministral-8b-latest").unwrap(),
            p.price("ministral-8b-2512").unwrap()
        );
    }
}
