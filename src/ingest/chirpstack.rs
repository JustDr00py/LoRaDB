use super::common::{validate_payload_size, MessageParser, MAX_MQTT_PAYLOAD_SIZE};
use crate::error::LoraDbError;
use crate::model::decoded::DecodedPayload;
use crate::model::frames::{Frame, UplinkFrame};
use crate::model::gateway::{GatewayLocation, GatewayRxInfo};
use crate::model::lorawan::*;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;

pub struct ChirpStackParser;

impl ChirpStackParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChirpStackParser {
    fn default() -> Self {
        Self::new()
    }
}

/// ChirpStack v4 uplink message format
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChirpStackUplink {
    dev_eui: String,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    application_id: Option<String>,
    #[serde(default)]
    application_name: Option<String>,
    f_port: u8,
    f_cnt: u32,
    #[serde(default)]
    confirmed: bool,
    #[serde(default)]
    adr: bool,
    dr: u8,
    #[serde(default)]
    rx_info: Vec<ChirpStackRxInfo>,
    tx_info: ChirpStackTxInfo,
    #[serde(default)]
    object: Option<serde_json::Value>,
    #[serde(default)]
    data: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChirpStackRxInfo {
    gateway_id: String,
    rssi: i16,
    snr: f32,
    #[serde(default)]
    channel: u8,
    #[serde(default)]
    rf_chain: u8,
    #[serde(default)]
    location: Option<ChirpStackLocation>,
}

#[derive(Debug, Deserialize)]
struct ChirpStackLocation {
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    altitude: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ChirpStackTxInfo {
    frequency: u64,
    #[serde(default)]
    modulation: Option<String>,
}

impl MessageParser for ChirpStackParser {
    fn parse_message(&self, topic: &str, payload: &[u8]) -> Result<Option<Frame>> {
        // ChirpStack topic format: application/{app_id}/device/{dev_eui}/event/up
        if !topic.contains("/event/up") {
            return Ok(None); // Not an uplink message
        }

        validate_payload_size(payload, MAX_MQTT_PAYLOAD_SIZE)?;

        let msg: ChirpStackUplink = serde_json::from_slice(payload)
            .context("Failed to parse ChirpStack uplink JSON")?;

        // Validate and create DevEui
        let dev_eui = DevEui::new(msg.dev_eui)
            .map_err(|e| LoraDbError::MqttParseError(e.to_string()))?;

        // Determine application ID (prefer applicationId field, fallback to topic parsing)
        let application_id = msg
            .application_id
            .or(msg.application_name)
            .or_else(|| {
                // Parse from topic: application/{app_id}/device/{dev_eui}/event/up
                topic.split('/').nth(1).map(String::from)
            })
            .unwrap_or_else(|| "unknown".to_string());

        let uplink = UplinkFrame {
            dev_eui,
            application_id: ApplicationId::new(application_id),
            device_name: msg.device_name,
            received_at: Utc::now(), // ChirpStack v4 may have a time field in some versions
            f_port: msg.f_port,
            f_cnt: msg.f_cnt,
            confirmed: msg.confirmed,
            adr: msg.adr,
            dr: DataRate::new_lora(125000, msg.dr), // Default to 125kHz bandwidth
            frequency: msg.tx_info.frequency,
            rx_info: msg
                .rx_info
                .into_iter()
                .map(|rx| GatewayRxInfo {
                    gateway_id: GatewayEui::new(rx.gateway_id),
                    rssi: rx.rssi,
                    snr: rx.snr,
                    channel: rx.channel,
                    rf_chain: rx.rf_chain,
                    location: rx.location.map(|loc| GatewayLocation {
                        latitude: loc.latitude,
                        longitude: loc.longitude,
                        altitude: loc.altitude,
                    }),
                })
                .collect(),
            decoded_payload: msg.object.map(DecodedPayload::from_json),
            raw_payload: msg.data,
        };

        Ok(Some(Frame::Uplink(uplink)))
    }

    fn extract_dev_eui(&self, topic: &str) -> Option<String> {
        // application/{app_id}/device/{dev_eui}/event/up
        topic.split('/').nth(3).map(String::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chirpstack_parser() {
        let parser = ChirpStackParser;

        let payload = r#"{
            "devEui": "0123456789abcdef",
            "deviceName": "test-sensor",
            "applicationId": "test-app",
            "fPort": 1,
            "fCnt": 42,
            "confirmed": false,
            "adr": true,
            "dr": 5,
            "rxInfo": [{
                "gatewayId": "gateway-001",
                "rssi": -50,
                "snr": 10.5,
                "channel": 0,
                "rfChain": 0
            }],
            "txInfo": {
                "frequency": 868100000
            },
            "object": {
                "temperature": 22.5,
                "humidity": 60.0
            },
            "data": "AQIDBAUGBwg="
        }"#;

        let topic = "application/test-app/device/0123456789abcdef/event/up";
        let frame = parser
            .parse_message(topic, payload.as_bytes())
            .unwrap()
            .unwrap();

        match frame {
            Frame::Uplink(uplink) => {
                assert_eq!(uplink.dev_eui.as_str(), "0123456789abcdef");
                assert_eq!(uplink.f_port, 1);
                assert_eq!(uplink.f_cnt, 42);
                assert_eq!(uplink.rx_info.len(), 1);
                assert!(uplink.decoded_payload.is_some());
            }
            _ => panic!("Expected Uplink frame"),
        }
    }
}
