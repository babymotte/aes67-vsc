/*
 * This is in large parts copied with some modifications from https://crates.io/crates/statime-linux
 */

use pnet::datalink::NetworkInterface;
use rand::{rngs::StdRng, SeedableRng};
use statime::{
    config::{ClockIdentity, InstanceConfig, SdoId, TimePropertiesDS, TimeSource},
    filters::{KalmanConfiguration, KalmanFilter},
    port::{
        is_message_buffer_compatible, InBmca, Port, PortAction, PortActionIterator,
        TimestampContext, MAX_DATA_LEN,
    },
    time::Time,
    Clock, OverlayClock, PtpInstance, PtpInstanceState, SharedClock,
};
use statime_linux::{
    clock::{LinuxClock, PortTimestampToTime},
    config::{DelayType, NetworkMode, PortConfig},
    observer::ObservableInstanceState,
    socket::{
        open_ethernet_socket, open_ipv4_event_socket, open_ipv4_general_socket,
        open_ipv6_event_socket, open_ipv6_general_socket, PtpTargetAddress,
    },
    tlvforwarder::TlvForwarder,
};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::{pin, Pin},
    sync::RwLock,
    time::Duration,
};
use timestamped_socket::{
    interface::InterfaceName,
    networkaddress::{EthernetAddress, NetworkAddress},
    socket::{InterfaceTimestampMode, Open, Socket},
};
use tokio::{
    spawn,
    sync::mpsc::{self, Receiver, Sender},
    time::Sleep,
};
use worterbuch_client::{topic, Worterbuch};

use crate::utils::system_time;

pub type PtpClock = SharedClock<OverlayClock<LinuxClock>>;

pub fn current_offset(clock: &PtpClock) -> i64 {
    let ptp_time = clock.now();
    let system_time = system_time();

    let diff_s = ptp_time.secs() as i64 - system_time.tv_sec;
    let diff_ns = ptp_time.subsec_nanos() as i64 - system_time.tv_nsec;
    diff_s * Duration::from_secs(1).as_nanos() as i64 + diff_ns
}

pin_project_lite::pin_project! {
    struct Timer {
        #[pin]
        timer: Sleep,
        running: bool,
    }
}

impl Timer {
    fn new() -> Self {
        Timer {
            timer: tokio::time::sleep(std::time::Duration::from_secs(0)),
            running: false,
        }
    }

    fn reset(self: Pin<&mut Self>, duration: std::time::Duration) {
        let this = self.project();
        this.timer.reset(tokio::time::Instant::now() + duration);
        *this.running = true;
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        if *this.running {
            let result = this.timer.poll(cx);
            if result != std::task::Poll::Pending {
                *this.running = false;
            }
            result
        } else {
            std::task::Poll::Pending
        }
    }
}

pub async fn statime_linux(
    iface: NetworkInterface,
    ip: IpAddr,
    wb: Worterbuch,
    root_key: String,
) -> SharedClock<OverlayClock<LinuxClock>> {
    let mac = iface.mac.expect("we already checked this");
    let mac = [mac.0, mac.1, mac.2, mac.3, mac.4, mac.5];

    let clock_identity = ClockIdentity::from_mac_address(mac);
    let path_trace = true;
    let port_config = port_config(ip);
    let sdo_id = 0x000;

    log::info!("Clock identity: {}", hex::encode(clock_identity.0));

    let instance_config = InstanceConfig {
        clock_identity,
        priority_1: 255,
        priority_2: 255,
        domain_number: 0,
        slave_only: false,
        sdo_id: SdoId::try_from(sdo_id).expect("sdo-id should be between 0 and 4095"),
        path_trace,
    };

    let time_properties_ds =
        TimePropertiesDS::new_arbitrary_time(false, false, TimeSource::InternalOscillator);

    let system_clock = SharedClock::new(OverlayClock::new(LinuxClock::CLOCK_TAI));

    // Leak to get a static reference, the ptp instance will be around for the rest
    // of the program anyway
    let instance = Box::leak(Box::new(PtpInstance::new(
        instance_config,
        time_properties_ds,
    )));

    // The observer for the metrics exporter
    let (instance_state_sender, instance_state_receiver) = mpsc::channel(1);

    publish_stats(instance_state_receiver, wb, root_key);

    let (bmca_notify_sender, bmca_notify_receiver) = tokio::sync::watch::channel(false);

    let tlv_forwarder = TlvForwarder::new();

    let interface = port_config.interface;
    let network_mode = port_config.network_mode;
    let (port_clock, timestamping) = (system_clock.clone(), InterfaceTimestampMode::SoftwareAll);

    let rng = StdRng::from_entropy();
    let bind_phc = port_config.hardware_clock;
    let port = instance.add_port(
        port_config.into(),
        KalmanConfiguration::default(),
        port_clock.clone(),
        rng,
    );

    let (main_task_sender, port_task_receiver) = tokio::sync::mpsc::channel(1);
    let (port_task_sender, main_task_receiver) = tokio::sync::mpsc::channel(1);

    match network_mode {
        statime_linux::config::NetworkMode::Ipv4 => {
            let event_socket = open_ipv4_event_socket(interface, timestamping, bind_phc)
                .expect("Could not open event socket");
            let general_socket =
                open_ipv4_general_socket(interface).expect("Could not open general socket");

            tokio::spawn(port_task(
                port_task_receiver,
                port_task_sender,
                event_socket,
                general_socket,
                bmca_notify_receiver.clone(),
                tlv_forwarder.duplicate(),
                port_clock,
            ));
        }
        statime_linux::config::NetworkMode::Ipv6 => {
            let event_socket = open_ipv6_event_socket(interface, timestamping, bind_phc)
                .expect("Could not open event socket");
            let general_socket =
                open_ipv6_general_socket(interface).expect("Could not open general socket");

            tokio::spawn(port_task(
                port_task_receiver,
                port_task_sender,
                event_socket,
                general_socket,
                bmca_notify_receiver.clone(),
                tlv_forwarder.duplicate(),
                port_clock,
            ));
        }
        statime_linux::config::NetworkMode::Ethernet => {
            let socket = open_ethernet_socket(interface, timestamping, bind_phc)
                .expect("Could not open socket");

            tokio::spawn(ethernet_port_task(
                port_task_receiver,
                port_task_sender,
                interface
                    .get_index()
                    .expect("Unable to get network interface index") as _,
                socket,
                bmca_notify_receiver.clone(),
                tlv_forwarder.duplicate(),
                port_clock,
            ));
        }
    }

    // Drop the forwarder so we don't keep an unneeded subscriber.
    drop(tlv_forwarder);

    // All ports created, so we can start running them.
    main_task_sender
        .send(port)
        .await
        .expect("space in channel buffer");

    spawn(run(
        instance,
        bmca_notify_sender,
        instance_state_sender,
        main_task_receiver,
        main_task_sender,
    ));

    system_clock
}

async fn run(
    instance: &'static PtpInstance<KalmanFilter, RwLock<PtpInstanceState>>,
    bmca_notify_sender: tokio::sync::watch::Sender<bool>,
    instance_state_sender: mpsc::Sender<ObservableInstanceState>,
    mut main_task_receiver: Receiver<BmcaPort>,
    main_task_sender: Sender<BmcaPort>,
) -> ! {
    // run bmca over all of the ports at the same time. The ports don't perform
    // their normal actions at this time: bmca is stop-the-world!
    let mut bmca_timer = pin!(Timer::new());

    loop {
        // reset bmca timer
        bmca_timer.as_mut().reset(instance.bmca_interval());

        // wait until the next BMCA
        bmca_timer.as_mut().await;

        // notify all the ports that they need to stop what they're doing
        bmca_notify_sender
            .send(true)
            .expect("Bmca notification failed");

        let mut bmca_port = main_task_receiver.recv().await.unwrap();
        let mut mut_bmca_ports = vec![&mut bmca_port];

        // have all ports so deassert stop
        bmca_notify_sender
            .send(false)
            .expect("Bmca notification failed");

        instance.bmca(&mut mut_bmca_ports);

        // Update instance state for observability
        // We don't care if isn't anybody on the other side
        instance_state_sender
            .send(ObservableInstanceState {
                default_ds: instance.default_ds(),
                current_ds: instance.current_ds(),
                parent_ds: instance.parent_ds(),
                time_properties_ds: instance.time_properties_ds(),
                path_trace_ds: instance.path_trace_ds(),
            })
            .await
            .ok();

        drop(mut_bmca_ports);

        main_task_sender.send(bmca_port).await.unwrap();
    }
}

type BmcaPort = Port<
    'static,
    InBmca,
    Option<Vec<ClockIdentity>>,
    StdRng,
    PtpClock,
    KalmanFilter,
    RwLock<PtpInstanceState>,
>;

// the Port task
//
// This task waits for a new port (in the bmca state) to arrive on its Receiver.
// It will then move the port into the running state, and process actions. When
// the task is notified of a BMCA, it will stop running, move the port into the
// bmca state, and send it on its Sender
async fn port_task<A: NetworkAddress + PtpTargetAddress>(
    mut port_task_receiver: Receiver<BmcaPort>,
    port_task_sender: Sender<BmcaPort>,
    mut event_socket: Socket<A, Open>,
    mut general_socket: Socket<A, Open>,
    mut bmca_notify: tokio::sync::watch::Receiver<bool>,
    mut tlv_forwarder: TlvForwarder,
    clock: PtpClock,
) {
    let mut timers = Timers {
        port_sync_timer: pin!(Timer::new()),
        port_announce_timer: pin!(Timer::new()),
        port_announce_timeout_timer: pin!(Timer::new()),
        delay_request_timer: pin!(Timer::new()),
        filter_update_timer: pin!(Timer::new()),
    };

    loop {
        let port_in_bmca = port_task_receiver.recv().await.unwrap();

        // handle post-bmca actions
        let (mut port, actions) = port_in_bmca.end_bmca();

        let mut pending_timestamp = handle_actions(
            actions,
            &mut event_socket,
            &mut general_socket,
            &mut timers,
            &tlv_forwarder,
            &clock,
        )
        .await;

        while let Some((context, timestamp)) = pending_timestamp {
            pending_timestamp = handle_actions(
                port.handle_send_timestamp(context, timestamp),
                &mut event_socket,
                &mut general_socket,
                &mut timers,
                &tlv_forwarder,
                &clock,
            )
            .await;
        }

        let mut event_buffer = [0; MAX_DATA_LEN];
        let mut general_buffer = [0; 2048];

        loop {
            let mut actions = tokio::select! {
                result = event_socket.recv(&mut event_buffer) => match result {
                    Ok(packet) => {
                        if !is_message_buffer_compatible(&event_buffer[..packet.bytes_read]) {
                            // do not spam with missing timestamp error in mixed-version PTPv1+v2 networks
                            PortActionIterator::empty()
                        } else if let Some(timestamp) = packet.timestamp {
                            log::trace!("Recv timestamp: {:?}", packet.timestamp);
                            port.handle_event_receive(&event_buffer[..packet.bytes_read], clock.port_timestamp_to_time(timestamp))
                        } else {
                            log::error!("Missing recv timestamp");
                            PortActionIterator::empty()
                        }
                    }
                    Err(error) => panic!("Error receiving: {error:?}"),
                },
                result = general_socket.recv(&mut general_buffer) => match result {
                    Ok(packet) => port.handle_general_receive(&general_buffer[..packet.bytes_read]),
                    Err(error) => panic!("Error receiving: {error:?}"),
                },
                () = &mut timers.port_announce_timer => {
                    port.handle_announce_timer(&mut tlv_forwarder)
                },
                () = &mut timers.port_sync_timer => {
                    port.handle_sync_timer()
                },
                () = &mut timers.port_announce_timeout_timer => {
                    port.handle_announce_receipt_timer()
                },
                () = &mut timers.delay_request_timer => {
                    port.handle_delay_request_timer()
                },
                () = &mut timers.filter_update_timer => {
                    port.handle_filter_update_timer()
                },
                result = bmca_notify.wait_for(|v| *v) => match result {
                    Ok(_) => break,
                    Err(error) => panic!("Error on bmca notify: {error:?}"),
                }
            };

            loop {
                let pending_timestamp = handle_actions(
                    actions,
                    &mut event_socket,
                    &mut general_socket,
                    &mut timers,
                    &tlv_forwarder,
                    &clock,
                )
                .await;

                // there might be more actions to handle based on the current action
                actions = match pending_timestamp {
                    Some((context, timestamp)) => port.handle_send_timestamp(context, timestamp),
                    None => break,
                };
            }
        }

        let port_in_bmca = port.start_bmca();
        port_task_sender.send(port_in_bmca).await.unwrap();
    }
}

// the Port task for ethernet transport
//
// This task waits for a new port (in the bmca state) to arrive on its Receiver.
// It will then move the port into the running state, and process actions. When
// the task is notified of a BMCA, it will stop running, move the port into the
// bmca state, and send it on its Sender
async fn ethernet_port_task(
    mut port_task_receiver: Receiver<BmcaPort>,
    port_task_sender: Sender<BmcaPort>,
    interface: libc::c_int,
    mut socket: Socket<EthernetAddress, Open>,
    mut bmca_notify: tokio::sync::watch::Receiver<bool>,
    mut tlv_forwarder: TlvForwarder,
    clock: PtpClock,
) {
    let mut timers = Timers {
        port_sync_timer: pin!(Timer::new()),
        port_announce_timer: pin!(Timer::new()),
        port_announce_timeout_timer: pin!(Timer::new()),
        delay_request_timer: pin!(Timer::new()),
        filter_update_timer: pin!(Timer::new()),
    };

    loop {
        let port_in_bmca = port_task_receiver.recv().await.unwrap();

        // Clear out old tlvs if we are not in the master state, so we don't keep em too
        // long.
        if port_in_bmca.is_master() {
            tlv_forwarder.empty()
        }

        // handle post-bmca actions
        let (mut port, actions) = port_in_bmca.end_bmca();

        let mut pending_timestamp = handle_actions_ethernet(
            actions,
            interface,
            &mut socket,
            &mut timers,
            &tlv_forwarder,
            &clock,
        )
        .await;

        while let Some((context, timestamp)) = pending_timestamp {
            pending_timestamp = handle_actions_ethernet(
                port.handle_send_timestamp(context, timestamp),
                interface,
                &mut socket,
                &mut timers,
                &tlv_forwarder,
                &clock,
            )
            .await;
        }

        let mut event_buffer = [0; MAX_DATA_LEN];

        loop {
            let mut actions = tokio::select! {
                result = socket.recv(&mut event_buffer) => match result {
                    Ok(packet) => {
                        if let Some(timestamp) = packet.timestamp {
                            log::trace!("Recv timestamp: {:?}", packet.timestamp);
                            port.handle_event_receive(&event_buffer[..packet.bytes_read], clock.port_timestamp_to_time(timestamp))
                        } else {
                            port.handle_general_receive(&event_buffer[..packet.bytes_read])
                        }
                    }
                    Err(error) => panic!("Error receiving: {error:?}"),
                },
                () = &mut timers.port_announce_timer => {
                    port.handle_announce_timer(&mut tlv_forwarder)
                },
                () = &mut timers.port_sync_timer => {
                    port.handle_sync_timer()
                },
                () = &mut timers.port_announce_timeout_timer => {
                    port.handle_announce_receipt_timer()
                },
                () = &mut timers.delay_request_timer => {
                    port.handle_delay_request_timer()
                },
                () = &mut timers.filter_update_timer => {
                    port.handle_filter_update_timer()
                },
                result = bmca_notify.wait_for(|v| *v) => match result {
                    Ok(_) => break,
                    Err(error) => panic!("Error on bmca notify: {error:?}"),
                }
            };

            loop {
                let pending_timestamp = handle_actions_ethernet(
                    actions,
                    interface,
                    &mut socket,
                    &mut timers,
                    &tlv_forwarder,
                    &clock,
                )
                .await;

                // there might be more actions to handle based on the current action
                actions = match pending_timestamp {
                    Some((context, timestamp)) => port.handle_send_timestamp(context, timestamp),
                    None => break,
                };
            }
        }

        let port_in_bmca = port.start_bmca();
        port_task_sender.send(port_in_bmca).await.unwrap();
    }
}

struct Timers<'a> {
    port_sync_timer: Pin<&'a mut Timer>,
    port_announce_timer: Pin<&'a mut Timer>,
    port_announce_timeout_timer: Pin<&'a mut Timer>,
    delay_request_timer: Pin<&'a mut Timer>,
    filter_update_timer: Pin<&'a mut Timer>,
}

async fn handle_actions<A: NetworkAddress + PtpTargetAddress>(
    actions: PortActionIterator<'_>,
    event_socket: &mut Socket<A, Open>,
    general_socket: &mut Socket<A, Open>,
    timers: &mut Timers<'_>,
    tlv_forwarder: &TlvForwarder,
    clock: &PtpClock,
) -> Option<(TimestampContext, Time)> {
    let mut pending_timestamp = None;

    for action in actions {
        match action {
            PortAction::SendEvent {
                context,
                data,
                link_local,
            } => {
                // send timestamp of the send
                let time = event_socket
                    .send_to(
                        data,
                        if link_local {
                            A::PDELAY_EVENT
                        } else {
                            A::PRIMARY_EVENT
                        },
                    )
                    .await
                    .expect("Failed to send event message");

                // anything we send later will have a later pending (send) timestamp
                if let Some(time) = time {
                    log::trace!("Send timestamp {:?}", time);
                    pending_timestamp = Some((context, clock.port_timestamp_to_time(time)));
                } else {
                    log::error!("Missing send timestamp");
                }
            }
            PortAction::SendGeneral { data, link_local } => {
                general_socket
                    .send_to(
                        data,
                        if link_local {
                            A::PDELAY_GENERAL
                        } else {
                            A::PRIMARY_GENERAL
                        },
                    )
                    .await
                    .expect("Failed to send general message");
            }
            PortAction::ResetAnnounceTimer { duration } => {
                timers.port_announce_timer.as_mut().reset(duration);
            }
            PortAction::ResetSyncTimer { duration } => {
                timers.port_sync_timer.as_mut().reset(duration);
            }
            PortAction::ResetDelayRequestTimer { duration } => {
                timers.delay_request_timer.as_mut().reset(duration);
            }
            PortAction::ResetAnnounceReceiptTimer { duration } => {
                timers.port_announce_timeout_timer.as_mut().reset(duration);
            }
            PortAction::ResetFilterUpdateTimer { duration } => {
                timers.filter_update_timer.as_mut().reset(duration);
            }
            PortAction::ForwardTLV { tlv } => {
                tlv_forwarder.forward(tlv.into_owned());
            }
        }
    }

    pending_timestamp
}

async fn handle_actions_ethernet(
    actions: PortActionIterator<'_>,
    interface: libc::c_int,
    socket: &mut Socket<EthernetAddress, Open>,
    timers: &mut Timers<'_>,
    tlv_forwarder: &TlvForwarder,
    clock: &PtpClock,
) -> Option<(TimestampContext, Time)> {
    let mut pending_timestamp = None;

    for action in actions {
        match action {
            PortAction::SendEvent {
                context,
                data,
                link_local,
            } => {
                // send timestamp of the send
                let time = socket
                    .send_to(
                        data,
                        EthernetAddress::new(
                            if link_local {
                                EthernetAddress::PDELAY_EVENT.protocol()
                            } else {
                                EthernetAddress::PRIMARY_EVENT.protocol()
                            },
                            if link_local {
                                EthernetAddress::PDELAY_EVENT.mac()
                            } else {
                                EthernetAddress::PRIMARY_EVENT.mac()
                            },
                            interface,
                        ),
                    )
                    .await
                    .expect("Failed to send event message");

                // anything we send later will have a later pending (send) timestamp
                if let Some(time) = time {
                    log::trace!("Send timestamp {:?}", time);
                    pending_timestamp = Some((context, clock.port_timestamp_to_time(time)));
                } else {
                    log::error!("Missing send timestamp");
                }
            }
            PortAction::SendGeneral { data, link_local } => {
                socket
                    .send_to(
                        data,
                        EthernetAddress::new(
                            if link_local {
                                EthernetAddress::PDELAY_GENERAL.protocol()
                            } else {
                                EthernetAddress::PRIMARY_GENERAL.protocol()
                            },
                            if link_local {
                                EthernetAddress::PDELAY_GENERAL.mac()
                            } else {
                                EthernetAddress::PRIMARY_GENERAL.mac()
                            },
                            interface,
                        ),
                    )
                    .await
                    .expect("Failed to send general message");
            }
            PortAction::ResetAnnounceTimer { duration } => {
                timers.port_announce_timer.as_mut().reset(duration);
            }
            PortAction::ResetSyncTimer { duration } => {
                timers.port_sync_timer.as_mut().reset(duration);
            }
            PortAction::ResetDelayRequestTimer { duration } => {
                timers.delay_request_timer.as_mut().reset(duration);
            }
            PortAction::ResetAnnounceReceiptTimer { duration } => {
                timers.port_announce_timeout_timer.as_mut().reset(duration);
            }
            PortAction::ResetFilterUpdateTimer { duration } => {
                timers.filter_update_timer.as_mut().reset(duration);
            }
            PortAction::ForwardTLV { tlv } => tlv_forwarder.forward(tlv.into_owned()),
        }
    }

    pending_timestamp
}

fn port_config(ip: IpAddr) -> PortConfig {
    let interface = InterfaceName::from_socket_addr(SocketAddr::new(ip, 9091))
        .expect("no interface")
        .expect("no interface");
    let acceptable_master_list = None;
    let hardware_clock = None;
    let network_mode = NetworkMode::Ipv4;
    let announce_interval = 1;
    let sync_interval = 0;
    let announce_receipt_timeout = 3;
    let master_only = false;
    let delay_asymmetry = 0;
    let delay_mechanism = DelayType::E2E;
    let delay_interval = 1;

    PortConfig {
        interface,
        acceptable_master_list,
        hardware_clock,
        network_mode,
        announce_interval,
        sync_interval,
        announce_receipt_timeout,
        master_only,
        delay_asymmetry,
        delay_mechanism,
        delay_interval,
    }
}

fn publish_stats(
    mut instance_state_receiver: mpsc::Receiver<ObservableInstanceState>,
    wb: Worterbuch,
    root_key: String,
) {
    spawn(async move {
        while let Some(stats) = instance_state_receiver.recv().await {
            wb.publish(
                topic!(root_key, "status", "current", "offsetFromMaster"),
                &stats.current_ds.offset_from_master,
            )
            .await
            .ok();
            wb.publish(
                topic!(root_key, "status", "current", "stepsRemoved"),
                &stats.current_ds.steps_removed,
            )
            .await
            .ok();

            wb.set(
                topic!(root_key, "status", "default", "clock", "id"),
                &stats.default_ds.clock_identity,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "default", "clock", "quality"),
                &stats.default_ds.clock_quality,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "default", "domain"),
                &stats.default_ds.domain_number,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "default", "ports"),
                &stats.default_ds.number_ports,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "default", "priority1"),
                &stats.default_ds.priority_1,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "default", "priority2"),
                &stats.default_ds.priority_2,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "default", "sdoID"),
                &stats.default_ds.sdo_id,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "default", "slaveOnly"),
                &stats.default_ds.slave_only,
            )
            .await
            .ok();

            wb.set(
                topic!(
                    root_key,
                    "status",
                    "ptp",
                    "parent",
                    "grandmaster",
                    "clock",
                    "quality"
                ),
                &stats.parent_ds.grandmaster_clock_quality,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "parent", "grandmaster", "id"),
                &stats.parent_ds.grandmaster_identity,
            )
            .await
            .ok();
            wb.set(
                topic!(
                    root_key,
                    "status",
                    "ptp",
                    "parent",
                    "grandmaster",
                    "priority1"
                ),
                &stats.parent_ds.grandmaster_priority_1,
            )
            .await
            .ok();
            wb.set(
                topic!(
                    root_key,
                    "status",
                    "ptp",
                    "parent",
                    "grandmaster",
                    "priority2"
                ),
                &stats.parent_ds.grandmaster_priority_2,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "parent", "port", "id"),
                &stats.parent_ds.parent_port_identity,
            )
            .await
            .ok();

            wb.set(
                topic!(root_key, "status", "pathTrace", "enabled"),
                &stats.path_trace_ds.enable,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "pathTrace", "list"),
                &stats.path_trace_ds.list,
            )
            .await
            .ok();

            wb.publish(
                topic!(root_key, "status", "time", "properties", "utcOffset"),
                &stats.time_properties_ds.current_utc_offset,
            )
            .await
            .ok();
            wb.set(
                topic!(
                    root_key,
                    "status",
                    "ptp",
                    "time",
                    "properties",
                    "frequencyTraceable"
                ),
                &stats.time_properties_ds.frequency_traceable,
            )
            .await
            .ok();
            wb.set(
                topic!(root_key, "status", "time", "properties", "isPtp"),
                &stats.time_properties_ds.is_ptp(),
            )
            .await
            .ok();
            wb.set(
                topic!(
                    root_key,
                    "status",
                    "ptp",
                    "time",
                    "properties",
                    "leadIndicator"
                ),
                &stats.time_properties_ds.leap_indicator,
            )
            .await
            .ok();
            wb.set(
                topic!(
                    root_key,
                    "status",
                    "ptp",
                    "time",
                    "properties",
                    "timeTraceable"
                ),
                &stats.time_properties_ds.time_traceable,
            )
            .await
            .ok();
        }
    });
}
