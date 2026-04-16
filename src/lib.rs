use std::{
    net::UdpSocket,
    sync::{Arc, RwLock, mpsc},
    thread,
    time::Duration,
    vec::Vec,
};

use log::{debug, error, info, trace, warn};
#[cfg(unix)]
use midir::os::unix::{VirtualInput, VirtualOutput};
use midir::{MidiInput, MidiOutput};

use rosc::encoder;
use rosc::{OscMessage, OscPacket};

use rusb::{Context, Device, DeviceDescriptor, DeviceHandle, Direction, TransferType, UsbContext};

mod autocrap;
pub use autocrap::{config, interpreter};

pub mod error;
pub use error::Error;

use crate::config::{Config, Interface, MidiInterface, MidiPort, OscInterface};
use crate::interpreter::{CtrlResponse, Interpreter, MidiResponse, OscResponse};

type Result<T> = std::result::Result<T, Error>;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug)]
#[allow(unused)]
struct Endpoint {
    config: u8,
    iface: u8,
    setting: u8,
    address: u8,
    transfer_type: TransferType,
    direction: Direction,
}

pub fn run(config: Config) -> Result<()> {
    let mut context = Context::new()?;

    match open_device(&mut context, config.vendor_id, config.product_id) {
        Some((mut device, device_desc, mut handle)) => {
            handle.reset()?;

            let languages = handle.read_languages(DEFAULT_TIMEOUT)?;

            info!("active configuration: {}", handle.active_configuration()?);
            info!("languages: {:?}", languages);

            if !languages.is_empty() {
                let language = languages[0];

                info!(
                    "manufacturer: {:?}",
                    handle
                        .read_manufacturer_string(language, &device_desc, DEFAULT_TIMEOUT)
                        .ok()
                );
                info!(
                    "product: {:?}",
                    handle
                        .read_product_string(language, &device_desc, DEFAULT_TIMEOUT)
                        .ok()
                );
                info!(
                    "serial number: {:?}",
                    handle
                        .read_serial_number_string(language, &device_desc, DEFAULT_TIMEOUT)
                        .ok()
                );
            }

            let ctrl_in_endpoint = find_endpoint(&mut device, &device_desc, |e| {
                e.config == config.in_endpoint
                    && e.transfer_type == TransferType::Interrupt
                    && e.direction == Direction::In
            })
            .ok_or(Error::EndpointNotFound("control in"))?;

            let ctrl_out_endpoint = find_endpoint(&mut device, &device_desc, |e| {
                e.config == config.out_endpoint
                    && e.transfer_type == TransferType::Interrupt
                    && e.direction == Direction::Out
            })
            .ok_or(Error::EndpointNotFound("control out"))?;

            info!("control in endpoint: {:?}", ctrl_in_endpoint);
            info!("control out endpoint: {:?}", ctrl_out_endpoint);

            match handle.set_auto_detach_kernel_driver(true) {
                Ok(()) => Ok(()),
                Err(rusb::Error::NotSupported) => Ok(()),
                err => err,
            }?;

            configure_endpoint(&mut handle, &ctrl_in_endpoint)?;
            configure_endpoint(&mut handle, &ctrl_out_endpoint)?;

            let interpreter = Arc::new(RwLock::new(Interpreter::new(&config)));
            let (receiver_ctrl_tx, ctrl_rx) = mpsc::channel();
            let reader_ctrl_tx = receiver_ctrl_tx.clone();

            write_init(&mut handle, ctrl_out_endpoint.address)?;

            thread::scope(|s| {
                let writer_thread = s.spawn(|| {
                    run_writer(&handle, &ctrl_out_endpoint, ctrl_rx).unwrap();
                });

                let receiver_thread = s.spawn(|| match config.interface {
                    Interface::Midi(_) => {
                        run_midi_receiver(&config, &interpreter, receiver_ctrl_tx).unwrap()
                    }
                    Interface::Osc(_) => {
                        run_osc_receiver(&config, &interpreter, receiver_ctrl_tx).unwrap()
                    }
                });

                if let Err(e) = run_reader(
                    &config,
                    &interpreter,
                    &handle,
                    &ctrl_in_endpoint,
                    reader_ctrl_tx,
                ) {
                    info!("device disconnected ({}), shutting down", e);
                    // The receiver thread blocks on MIDI/OSC input indefinitely and
                    // has no shutdown signal, so we exit the process directly and
                    // let the OS clean up the remaining threads.
                    std::process::exit(0);
                }

                receiver_thread.join().expect("receiver thread panicked");
                writer_thread.join().expect("writer thread panicked");
            });
        }
        None => error!(
            "could not find device {:04x}:{:04x}",
            config.vendor_id, config.product_id
        ),
    }

    Ok(())
}

fn write_init<T: UsbContext>(handle: &mut DeviceHandle<T>, address: u8) -> Result<()> {
    let write = |bytes| handle.write_interrupt(address, bytes, DEFAULT_TIMEOUT);

    // b0 looks to be a "start" byte, 00 00 is reset (all leds off)
    write(&[0xb0, 0x00, 0x00])?;

    Ok(())
}

fn open_device<T: UsbContext>(
    context: &mut T,
    vid: u16,
    pid: u16,
) -> Option<(Device<T>, DeviceDescriptor, DeviceHandle<T>)> {
    let devices = match context.devices() {
        Ok(d) => d,
        Err(_) => return None,
    };

    for device in devices.iter() {
        let device_desc = match device.device_descriptor() {
            Ok(d) => d,
            Err(_) => continue,
        };

        if device_desc.vendor_id() == vid && device_desc.product_id() == pid {
            match device.open() {
                Ok(handle) => return Some((device, device_desc, handle)),
                Err(e) => panic!("Device found but failed to open: {}", e),
            }
        }
    }

    None
}

fn find_endpoint<T: UsbContext>(
    device: &mut Device<T>,
    device_desc: &DeviceDescriptor,
    predicate: impl Fn(Endpoint) -> bool,
) -> Option<Endpoint> {
    for n in 0..device_desc.num_configurations() {
        let config_desc = match device.config_descriptor(n) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for interface in config_desc.interfaces() {
            for interface_desc in interface.descriptors() {
                for endpoint_desc in interface_desc.endpoint_descriptors() {
                    let endpoint = Endpoint {
                        config: config_desc.number(),
                        iface: interface_desc.interface_number(),
                        setting: interface_desc.setting_number(),
                        address: endpoint_desc.address(),
                        transfer_type: endpoint_desc.transfer_type(),
                        direction: endpoint_desc.direction(),
                    };

                    if predicate(endpoint) {
                        return Some(endpoint);
                    }
                }
            }
        }
    }

    None
}

fn configure_endpoint<T: UsbContext>(
    handle: &mut DeviceHandle<T>,
    endpoint: &Endpoint,
) -> Result<()> {
    // handle.set_active_configuration(endpoint.config)?;
    info!("configure_endpoint {:?}", endpoint);
    handle.claim_interface(endpoint.iface)?;
    // handle.set_alternate_setting(endpoint.iface, endpoint.setting)?;
    Ok(())
}

fn run_reader<T: UsbContext>(
    config: &Config,
    interpreter: &Arc<RwLock<Interpreter>>,
    handle: &DeviceHandle<T>,
    endpoint: &Endpoint,
    ctrl_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    let osc = if let Interface::Osc(OscInterface {
        host_addr,
        out_addr,
        ..
    }) = config.interface
    {
        let sock = UdpSocket::bind(host_addr)?;
        Some((sock, out_addr))
    } else {
        None
    };

    let mut midi = if let Interface::Midi(ref interface) = config.interface {
        let client_name = &interface.client_name;
        let midi_out = MidiOutput::new(client_name)?;
        match interface.out_port {
            MidiPort::Index(index) => {
                let p = midi_out.ports().remove(index);
                let name = midi_out.port_name(&p)?;
                let conn = midi_out
                    .connect(&p, client_name)
                    .map_err(|e| Error::MidiConnect(e.kind()))?;
                Some((name, conn))
            }
            MidiPort::Name(ref name) => midi_out
                .ports()
                .into_iter()
                .find(|p| midi_out.port_name(p).as_deref() == Ok(name.as_str()))
                .map(|p| -> Result<_> {
                    let port_name = midi_out.port_name(&p)?;
                    let conn = midi_out
                        .connect(&p, client_name)
                        .map_err(|e| Error::MidiConnect(e.kind()))?;
                    Ok((port_name, conn))
                })
                .transpose()?,
            #[cfg(unix)]
            MidiPort::Virtual(ref _name) => {
                let conn = midi_out
                    .create_virtual(client_name)
                    .map_err(|e| Error::MidiConnect(e.kind()))?;
                Some((client_name.to_string(), conn))
            }
            #[cfg(not(unix))]
            MidiPort::Virtual(ref _name) => {
                unimplemented!("virtual midi ports are currently unsupported on non-unix systems")
            }
        }
    } else {
        None
    };

    let mut all_bytes = [0u8; 8];

    loop {
        let num_bytes =
            match handle.read_interrupt(endpoint.address, &mut all_bytes, DEFAULT_TIMEOUT) {
                Ok(n) => n,
                Err(rusb::Error::Timeout) => continue,
                Err(e) => {
                    info!("device disconnected ({}), exiting", e);
                    return Err(e.into());
                }
            };

        trace!("read({:?}): {:02x?}", num_bytes, &all_bytes[..num_bytes]);
        for (num, val) in parse_ctrl_bytes(&all_bytes[..num_bytes]) {
            trace!("bytes: {:02x?}", [num, val]);

            let Some(response) = interpreter
                .write()
                .expect("interpreter lock poisoned")
                .handle_ctrl(num, val)
            else {
                warn!("unhandled data: {:02x?}", [num, val]);
                continue;
            };

            if let Some((sock, out_addr)) = osc.as_ref()
                && let Some(OscResponse { addr, args }) = response.osc
            {
                let msg = OscPacket::Message(OscMessage { addr, args });
                debug!("send osc: {:?}", msg);
                let msg_buf = encoder::encode(&msg)?;

                sock.send_to(&msg_buf, out_addr)?;
            }

            if let Some((_, out_conn)) = midi.as_mut()
                && let Some(MidiResponse { data }) = response.midi
            {
                debug!("send midi: {:02x?}", data);
                out_conn.send(&data)?;
            }

            if let Some(CtrlResponse { data }) = response.ctrl {
                ctrl_tx.send(data).map_err(|_| Error::ChannelSend)?;
            }
        }
    }
}

fn parse_ctrl_bytes(buf: &[u8]) -> Vec<(u8, u8)> {
    let mut pairs = Vec::new();
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == 0xb0 {
            i += 1;
            continue;
        }
        pairs.push((buf[i], buf[i + 1]));
        i += 2;
    }
    pairs
}

fn run_writer<T: UsbContext>(
    handle: &DeviceHandle<T>,
    endpoint: &Endpoint,
    ctrl_rx: mpsc::Receiver<Vec<u8>>,
) -> Result<()> {
    loop {
        let data = ctrl_rx.recv()?;
        debug!("send ctrl: {:02x?}", data);
        handle.write_interrupt(endpoint.address, &data, DEFAULT_TIMEOUT)?;
    }
}

fn run_osc_receiver(
    config: &Config,
    interpreter: &Arc<RwLock<Interpreter>>,
    ctrl_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    let Interface::Osc(OscInterface { in_addr, .. }) = config.interface else {
        return Ok(());
    };

    let sock = UdpSocket::bind(in_addr)?;
    info!("listening to {}", in_addr);

    let mut buf = [0u8; rosc::decoder::MTU];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((size, addr)) => {
                let (_, packet) = rosc::decoder::decode_udp(&buf[..size])?;
                match packet {
                    OscPacket::Message(msg) => {
                        debug!("recv osc: {} {:?}", msg.addr, msg.args);
                        let Some(response) = interpreter
                            .write()
                            .expect("interpreter lock poisoned")
                            .handle_osc(&msg)
                        else {
                            warn!(
                                "unhandled osc message: with size {} from {}: {} {:?}",
                                size, addr, msg.addr, msg.args
                            );
                            continue;
                        };

                        trace!("osc in response: {:?}", response);

                        let Some(CtrlResponse { data }) = response.ctrl else {
                            continue;
                        };

                        ctrl_tx.send(data).map_err(|_| Error::ChannelSend)?;
                    }
                    OscPacket::Bundle(bundle) => {
                        debug!("recv osc bundle: {:?}", bundle);
                        warn!("unhandled osc bundle: {:?}", bundle);
                    }
                }
            }
            Err(e) => {
                error!("error receiving from socket: {}", e);
                break;
            }
        }
    }

    Ok(())
}

fn run_midi_receiver(
    config: &Config,
    interpreter: &Arc<RwLock<Interpreter>>,
    ctrl_tx: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    let Interface::Midi(MidiInterface {
        ref client_name,
        ref in_port,
        ..
    }) = config.interface
    else {
        return Ok(());
    };

    let (tx, rx) = mpsc::channel();
    let midi_in = MidiInput::new(client_name)?;
    let midi = match in_port {
        MidiPort::Index(index) => {
            let p = midi_in.ports().remove(*index);
            let name = midi_in.port_name(&p)?;
            let conn = midi_in
                .connect(
                    &p,
                    client_name,
                    move |_time, msg, tx| {
                        tx.send(msg.to_vec()).expect("MIDI rx channel closed");
                    },
                    tx,
                )
                .map_err(|e| Error::MidiConnect(e.kind()))?;
            Some((name, conn))
        }
        MidiPort::Name(name) => midi_in
            .ports()
            .into_iter()
            .find(|p| midi_in.port_name(p).as_deref() == Ok(name.as_str()))
            .map(|p| -> Result<_> {
                let port_name = midi_in.port_name(&p)?;
                let conn = midi_in
                    .connect(
                        &p,
                        client_name,
                        move |_time, msg, tx| {
                            tx.send(msg.to_vec()).expect("MIDI rx channel closed");
                        },
                        tx,
                    )
                    .map_err(|e| Error::MidiConnect(e.kind()))?;
                Ok((port_name, conn))
            })
            .transpose()?,
        #[cfg(unix)]
        MidiPort::Virtual(_name) => {
            let conn = midi_in
                .create_virtual(
                    client_name,
                    move |_time, msg, tx| {
                        tx.send(msg.to_vec()).expect("MIDI rx channel closed");
                    },
                    tx,
                )
                .map_err(|e| Error::MidiConnect(e.kind()))?;
            Some((client_name.to_string(), conn))
        }
        #[cfg(not(unix))]
        MidiPort::Virtual(ref _name) => {
            unimplemented!("virtual midi ports are currently unsupported on non-unix systems")
        }
    };

    if midi.is_none() {
        warn!("no midi in port???");
    }

    loop {
        let msg = rx.recv()?;
        let Some(response) = interpreter
            .write()
            .expect("interpreter lock poisoned")
            .handle_midi(&msg)
        else {
            warn!("unhandled midi message: {:02x?}", msg);
            continue;
        };

        let Some(CtrlResponse { data }) = response.ctrl else {
            continue;
        };

        ctrl_tx.send(data).map_err(|_| Error::ChannelSend)?;
    }
}

#[cfg(test)]
mod tests {
    use super::parse_ctrl_bytes;

    #[test]
    fn parse_ctrl_empty() {
        assert_eq!(parse_ctrl_bytes(&[]), vec![]);
    }

    #[test]
    fn parse_ctrl_simple_pair() {
        assert_eq!(parse_ctrl_bytes(&[0x10, 0x7f]), vec![(0x10, 0x7f)]);
    }

    #[test]
    fn parse_ctrl_multiple_pairs() {
        assert_eq!(
            parse_ctrl_bytes(&[0x10, 0x7f, 0x20, 0x00]),
            vec![(0x10, 0x7f), (0x20, 0x00)]
        );
    }

    #[test]
    fn parse_ctrl_skips_leading_marker_byte() {
        assert_eq!(parse_ctrl_bytes(&[0xb0, 0x10, 0x7f]), vec![(0x10, 0x7f)]);
    }

    #[test]
    fn parse_ctrl_marker_between_pairs() {
        assert_eq!(
            parse_ctrl_bytes(&[0x10, 0x7f, 0xb0, 0x20, 0x00]),
            vec![(0x10, 0x7f), (0x20, 0x00)]
        );
    }

    #[test]
    fn parse_ctrl_trailing_single_byte_ignored() {
        // odd byte at end can't form a pair — silently dropped
        assert_eq!(parse_ctrl_bytes(&[0x10, 0x7f, 0x20]), vec![(0x10, 0x7f)]);
    }

    #[test]
    fn parse_ctrl_marker_then_single_byte_ignored() {
        // marker advances i by 1, leaving only 1 byte — not enough for a pair
        assert_eq!(parse_ctrl_bytes(&[0xb0, 0x10]), vec![]);
    }
}
