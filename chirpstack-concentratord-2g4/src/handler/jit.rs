use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libconcentratord::jitqueue::TxPacket;
use libconcentratord::signals::Signal;
use libconcentratord::{jitqueue, stats};
use libloragw_2g4::hal;

use super::super::wrapper;

pub fn jit_loop(
    queue: Arc<Mutex<jitqueue::Queue<wrapper::TxPacket>>>,
    stop_receive: Receiver<Signal>,
) {
    debug!("Start JIT queue loop");

    loop {
        // Instead of a 10ms sleep, we receive from the stop channel with a
        // timeout of 10ms.
        if let Ok(v) = stop_receive.recv_timeout(Duration::from_millis(10)) {
            debug!("Received stop signal, signal: {}", v);
            break;
        }

        let tx_packet = match get_tx_packet(&queue) {
            Some(v) => v,
            None => continue,
        };

        let downlink_id = tx_packet.get_id();
        let tx_packet = tx_packet.tx_packet();

        match hal::send(&tx_packet) {
            Ok(_) => {
                info!("Scheduled packet for TX, downlink_id: {}, count_us: {}, freq: {}, bw: {}, mod: {:?}, dr: {:?}",
                    downlink_id,
                    tx_packet.count_us,
                    tx_packet.freq_hz,
                    tx_packet.bandwidth,
                    hal::Modulation::LoRa,
                    tx_packet.datarate
                    );

                if let Ok(tx_info) = wrapper::downlink_to_tx_info_proto(&tx_packet) {
                    stats::inc_tx_counts(&tx_info);
                }
            }
            Err(err) => {
                error!("Schedule packet for tx error, error: {}", err);
            }
        }
    }

    debug!("JIT loop ended");
}

fn get_tx_packet(
    queue: &Arc<Mutex<jitqueue::Queue<wrapper::TxPacket>>>,
) -> Option<wrapper::TxPacket> {
    let mut queue = queue.lock().unwrap();
    let concentrator_count = hal::get_instcnt().expect("get concentrator count error");
    queue.pop(concentrator_count)
}
