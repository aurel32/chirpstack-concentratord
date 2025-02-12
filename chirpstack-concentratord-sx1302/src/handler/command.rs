use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use libconcentratord::signals::Signal;
use libconcentratord::{commands, jitqueue, stats};
use libloragw_sx1302::hal;
use prost::Message;

use super::super::config::vendor;
use super::super::wrapper;

pub fn handle_loop(
    vendor_config: &vendor::Configuration,
    gateway_id: &[u8],
    antenna_gain: i8,
    queue: Arc<Mutex<jitqueue::Queue<wrapper::TxPacket>>>,
    rep_sock: zmq::Socket,
    stop_receive: Receiver<Signal>,
    stop_send: Sender<Signal>,
) {
    debug!("Starting command handler loop");

    // A timeout is used so that we can consume from the stop signal.
    let reader = commands::Reader::new(&rep_sock, Duration::from_millis(100));

    for cmd in reader {
        if let Ok(v) = stop_receive.recv_timeout(Duration::from_millis(0)) {
            debug!("Received stop signal, signal: {}", v);
            break;
        }

        let resp = match cmd {
            commands::Command::Timeout => {
                continue;
            }
            commands::Command::Downlink(pl) => {
                match handle_downlink(vendor_config, gateway_id, &queue, &pl, antenna_gain) {
                    Ok(v) => v,
                    Err(_) => Vec::new(),
                }
            }
            commands::Command::GatewayID => gateway_id.to_vec(),
            commands::Command::Configuration(pl) => {
                match handle_configuration(stop_send.clone(), pl) {
                    Ok(v) => v,
                    Err(_) => Vec::new(),
                }
            }
            commands::Command::Error(err) => {
                error!("Read command error, error: {}", err);
                Vec::new()
            }
            commands::Command::Unknown(command, _) => {
                warn!("Unknown command received, command: {}", command);
                Vec::new()
            }
        };

        rep_sock.send(resp, 0).unwrap();
    }

    debug!("Command loop ended");
}

fn handle_downlink(
    vendor_config: &vendor::Configuration,
    gateway_id: &[u8],
    queue: &Arc<Mutex<jitqueue::Queue<wrapper::TxPacket>>>,
    pl: &chirpstack_api::gw::DownlinkFrame,
    antenna_gain: i8,
) -> Result<Vec<u8>> {
    stats::inc_tx_packets_received();

    let mut tx_ack = chirpstack_api::gw::DownlinkTxAck {
        gateway_id: hex::encode(gateway_id),
        downlink_id: pl.downlink_id,
        items: vec![Default::default(); pl.items.len()],
        ..Default::default()
    };
    let mut stats_tx_status = chirpstack_api::gw::TxAckStatus::Ignored;

    for (i, item) in pl.items.iter().enumerate() {
        // convert protobuf to hal struct
        let mut tx_packet = match wrapper::downlink_from_proto(item) {
            Ok(v) => v,
            Err(err) => {
                error!(
                    "Convert downlink protobuf to HAL struct error, downlink_id: {}, error: {}",
                    pl.downlink_id, err,
                );
                return Err(err);
            }
        };
        tx_packet.rf_power -= antenna_gain;

        // validate frequency range
        match vendor_config.radio_config.get(tx_packet.rf_chain as usize) {
            Some(v) => {
                if tx_packet.freq_hz < v.tx_freq_min || tx_packet.freq_hz > v.tx_freq_max {
                    error!("Frequency is not within min/max gateway frequency, downlink_id: {}, min_freq: {}, max_freq: {}", pl.downlink_id, v.tx_freq_min, v.tx_freq_max);
                    tx_ack.items[i].set_status(chirpstack_api::gw::TxAckStatus::TxFreq);

                    // try next
                    continue;
                }
            }
            None => {
                tx_ack.items[i].set_status(chirpstack_api::gw::TxAckStatus::TxFreq);

                // try next
                continue;
            }
        };

        // try enqueue
        match queue.lock().unwrap().enqueue(
            hal::get_instcnt().expect("get concentrator count error"),
            wrapper::TxPacket::new(pl.downlink_id, tx_packet),
        ) {
            Ok(_) => {
                tx_ack.items[i].set_status(chirpstack_api::gw::TxAckStatus::Ok);
                stats_tx_status = chirpstack_api::gw::TxAckStatus::Ok;

                // break out of loop
                break;
            }
            Err(status) => {
                tx_ack.items[i].set_status(status);
                stats_tx_status = status;
            }
        };
    }

    stats::inc_tx_status_count(stats_tx_status);

    Ok(tx_ack.encode_to_vec())
}

fn handle_configuration(
    stop_send: Sender<Signal>,
    pl: chirpstack_api::gw::GatewayConfiguration,
) -> Result<Vec<u8>> {
    stop_send.send(Signal::Configuration(pl)).unwrap();
    Ok(Vec::new())
}
