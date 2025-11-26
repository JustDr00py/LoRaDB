use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Pre-decoded payload from network server (ChirpStack/TTN)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedPayload {
    /// Arbitrary JSON object from decoder
    pub object: Value,
}

impl DecodedPayload {
    pub fn from_json(value: Value) -> Self {
        Self { object: value }
    }

    /// Get a field by path (e.g., "temperature" or "sensor.temp")
    pub fn get_field(&self, path: &str) -> Option<&Value> {
        let mut current = &self.object;
        for segment in path.split('.') {
            current = current.get(segment)?;
        }
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_decoded_payload_get_field() {
        let payload = DecodedPayload::from_json(json!({
            "temperature": 22.5,
            "sensor": {
                "temp": 22.5,
                "humidity": 60.0
            }
        }));

        assert_eq!(payload.get_field("temperature"), Some(&json!(22.5)));
        assert_eq!(payload.get_field("sensor.temp"), Some(&json!(22.5)));
        assert_eq!(payload.get_field("sensor.humidity"), Some(&json!(60.0)));
        assert_eq!(payload.get_field("nonexistent"), None);
    }
}
