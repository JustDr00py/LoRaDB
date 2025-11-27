use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Pre-decoded payload from network server (ChirpStack/TTN)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedPayload {
    /// Arbitrary JSON object from decoder
    /// Uses custom serialization for bincode compatibility
    #[serde(serialize_with = "serialize_value", deserialize_with = "deserialize_value")]
    pub object: Value,
}

/// Custom serializer for serde_json::Value to make it bincode-compatible
/// Serializes the Value as a JSON string
fn serialize_value<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let json_string = serde_json::to_string(value).map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&json_string)
}

/// Custom deserializer for serde_json::Value to make it bincode-compatible
/// Deserializes from a JSON string back to Value
fn deserialize_value<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    let json_string = String::deserialize(deserializer)?;
    serde_json::from_str(&json_string).map_err(serde::de::Error::custom)
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
