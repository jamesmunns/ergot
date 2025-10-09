//! NUSB Bulk packet interface
//!
//! This interface is designed to be paired with an interface like the embassy-usb impl.
//! It also uses bulk packets to frame communication with the device. See the embassy-usb
//! impl for details on how ergot packets are framed.

use std::collections::HashSet;

use nusb::transfer::{Direction, EndpointType, Queue, RequestBuffer};

use crate::interface_manager::{
    utils::{framed_stream, std::StdQueue},
    Interface,
};
use log::{debug, info, trace, warn};

/// Interface impl using Nusb Bulk packets
pub struct NusbBulk {}

impl Interface for NusbBulk {
    type Sink = framed_stream::Sink<StdQueue>;
}

/// Information retrieved about a device from [`find_new_devices`]
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct DeviceInfo {
    pub usb_serial_number: Option<String>,
    pub usb_manufacturer: Option<String>,
    pub usb_product: Option<String>,
}

/// A new device returned by [`find_new_devices`], with a connection established
pub struct NewDevice {
    pub info: DeviceInfo,
    pub biq: Queue<RequestBuffer>,
    pub boq: Queue<Vec<u8>>,
    pub max_packet_size: Option<usize>,
}

fn device_match(d1: &nusb::DeviceInfo, d2: &nusb::DeviceInfo) -> bool {
    let bus_match = d1.bus_number() == d2.bus_number();
    let addr_match = d1.device_address() == d2.device_address();
    #[cfg(target_os = "macos")]
    let registry_match = d1.registry_entry_id() == d2.registry_entry_id();
    #[cfg(not(target_os = "macos"))]
    let registry_match = true;

    bus_match && addr_match && registry_match
}

/// A helper function for finding new devices not contained in the provided `devs` set.
///
/// This function does not add new devices to `devs`, the caller will need to do that
/// between calls to `find_new_devices`.
pub async fn find_new_devices(devs: &HashSet<DeviceInfo>) -> Vec<NewDevice> {
    trace!("Searching for new devices...");
    let mut out = vec![];
    let devices = nusb::list_devices().unwrap();
    let devices = devices.filter(coarse_device_filter).collect::<Vec<_>>();

    for device in devices {
        let dinfo = DeviceInfo {
            usb_serial_number: device.serial_number().map(String::from),
            usb_manufacturer: device.manufacturer_string().map(String::from),
            usb_product: device.product_string().map(String::from),
        };
        if devs.contains(&dinfo) {
            continue;
        };

        let mut devices = match nusb::list_devices() {
            Ok(d) => d,
            Err(e) => {
                warn!("Error listing devices: {e:?}");
                return vec![];
            }
        };
        let Some(found) = devices.find(|d| device_match(d, &device)) else {
            warn!("Failed to find matching nusb device!");
            continue;
        };

        // NOTE: We can't enumerate interfaces on Windows. For now, just use
        // a hardcoded interface of zero instead of trying to find the right one
        #[cfg(not(target_os = "windows"))]
        let Some(interface_id) = found.interfaces().position(|i| i.class() == 0xFF) else {
            warn!("Failed to find matching interface!!");
            continue;
        };

        #[cfg(target_os = "windows")]
        let interface_id = 0;

        let dev = match found.open() {
            Ok(d) => d,
            Err(e) => {
                warn!("Failed opening device: {e:?}");
                continue;
            }
        };
        let interface = match dev.claim_interface(interface_id as u8) {
            Ok(i) => i,
            Err(e) => {
                warn!("Failed claiming interface: {e:?}");
                continue;
            }
        };

        let mut mps: Option<usize> = None;
        let mut ep_in: Option<u8> = None;
        let mut ep_out: Option<u8> = None;
        for ias in interface.descriptors() {
            for ep in ias
                .endpoints()
                .filter(|e| e.transfer_type() == EndpointType::Bulk)
            {
                match ep.direction() {
                    Direction::Out => {
                        mps = Some(match mps.take() {
                            Some(old) => old.min(ep.max_packet_size()),
                            None => ep.max_packet_size(),
                        });
                        ep_out = Some(ep.address());
                    }
                    Direction::In => ep_in = Some(ep.address()),
                }
            }
        }

        if let Some(max_packet_size) = &mps {
            debug!("Detected max packet size: {max_packet_size}");
        } else {
            warn!("Unable to detect Max Packet Size!");
        };

        let Some(ep_out) = ep_out else {
            warn!("Failed to find OUT EP");
            continue;
        };
        debug!("OUT EP: {ep_out}");

        let Some(ep_in) = ep_in else {
            warn!("Failed to find IN EP");
            continue;
        };
        debug!("IN EP: {ep_in}");

        let boq = interface.bulk_out_queue(ep_out);
        let biq = interface.bulk_in_queue(ep_in);

        out.push(NewDevice {
            info: dinfo,
            biq,
            boq,
            max_packet_size: mps,
        });
    }

    if !out.is_empty() {
        info!("Found {} new devices", out.len());
    }
    out
}

fn coarse_device_filter(info: &nusb::DeviceInfo) -> bool {
    info.interfaces().any(|intfc| {
        let pre_check =
            intfc.class() == 0xFF && intfc.subclass() == 0xCA && intfc.protocol() == 0x7D;

        pre_check
            && intfc
                .interface_string()
                .map(|s| s == "ergot")
                .unwrap_or(true)
    })
}
