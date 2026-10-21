use alloc::{vec, vec::Vec};
use core::fmt::Write;
use esp_radio::wifi::{ClientConfig, ModeConfig, PowerSaveMode, WifiController, WifiDevice};
use motor_control::api::{self, Action};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    socket::{dhcpv4, tcp},
    time::Instant,
    wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Address},
};
mod status;
mod worker;
pub use status::{KnobStatus, Status};
pub use worker::Server;

include!(concat!(env!("OUT_DIR"), "/wifi_config.rs"));
const PAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/motor_panel.html"));

pub struct Response {
    header: heapless::String<256>,
    body: Body,
    sent: usize,
}

// Keep heartbeat responses in fixed storage rather than allocate per request.
#[allow(clippy::large_enum_variant)]
enum Body {
    Static(&'static [u8]),
    Json(heapless::String<1024>),
    Status(Status),
    Owned(Vec<u8>),
}

impl Response {
    pub fn status(status: Status) -> Self {
        Self {
            header: heapless::String::new(),
            body: Body::Status(status),
            sent: 0,
        }
    }

    fn prepare(self) -> Self {
        if let Body::Status(status) = &self.body {
            Self::json(200, status.json())
        } else {
            self
        }
    }
    pub fn json(status: u16, body: heapless::String<1024>) -> Self {
        Self::new(status, "application/json", Body::Json(body))
    }

    pub fn error(status: u16, message: &str) -> Self {
        let mut body = heapless::String::new();
        let _ = write!(body, "{{\"error\":\"{}\"}}", message);
        Self::json(status, body)
    }

    fn new(status: u16, content_type: &str, body: Body) -> Self {
        let len = match &body {
            Body::Static(bytes) => bytes.len(),
            Body::Json(s) => s.len(),
            Body::Owned(bytes) => bytes.len(),
            Body::Status(_) => unreachable!("deferred status must be prepared by network worker"),
        };
        let mut header = heapless::String::new();
        let _ = write!(
            header,
            "HTTP/1.1 {} Result\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n",
            status, content_type, len
        );
        Self {
            header,
            body,
            sent: 0,
        }
    }

    fn body(&self) -> &[u8] {
        match &self.body {
            Body::Static(bytes) => bytes,
            Body::Json(s) => s.as_bytes(),
            Body::Owned(bytes) => bytes,
            Body::Status(_) => unreachable!("deferred status must be prepared by network worker"),
        }
    }
}

struct Connection {
    handle: SocketHandle,
    request: heapless::Vec<u8, 1536>,
    response: Option<Response>,
    touched: u64,
}

struct Transport<'a> {
    controller: WifiController<'a>,
    device: WifiDevice<'a>,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    connections: Vec<Connection>,
    pub address: Option<Ipv4Address>,
    pub changed: bool,
    next_check: u64,
    connected: bool,
    motor_running: bool,
}

impl<'a> Transport<'a> {
    pub fn new(
        radio: &'a esp_radio::Controller<'a>,
        wifi: esp_hal::peripherals::WIFI<'a>,
        now: u64,
    ) -> Self {
        let (mut controller, interfaces) =
            esp_radio::wifi::new(radio, wifi, Default::default()).unwrap();
        controller
            .set_config(&ModeConfig::Client(
                ClientConfig::default()
                    .with_ssid(WIFI_SSID.into())
                    .with_password(WIFI_PASSWORD.into()),
            ))
            .unwrap();
        controller.start().unwrap();
        controller.set_power_saving(PowerSaveMode::Minimum).unwrap();
        let _ = controller.connect();
        let mut device = interfaces.sta;
        let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(
            device.mac_address(),
        )));
        config.random_seed = now ^ 0xa4cb_8f1e_54e0;
        let iface = Interface::new(config, &mut device, Instant::from_micros(now as i64));
        let mut sockets = SocketSet::new(Vec::new());
        let dhcp = sockets.add(dhcpv4::Socket::new());
        let mut connections = Vec::with_capacity(6);
        for _ in 0..6 {
            let mut socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 2048]),
                tcp::SocketBuffer::new(vec![0; 2048]),
            );
            socket.set_timeout(Some(smoltcp::time::Duration::from_secs(3)));
            socket.set_nagle_enabled(false);
            socket.set_ack_delay(Some(smoltcp::time::Duration::from_millis(10)));
            connections.push(Connection {
                handle: sockets.add(socket),
                request: heapless::Vec::new(),
                response: None,
                touched: now,
            });
        }
        Self {
            controller,
            device,
            iface,
            sockets,
            dhcp,
            connections,
            address: None,
            changed: false,
            next_check: now + 1_000_000,
            connected: false,
            motor_running: false,
        }
    }

    /// Keep the modem asleep between DTIM beacons while idle, but avoid adding
    /// modem-sleep latency to administration while the motor runs.
    pub fn set_motor_running(&mut self, running: bool) {
        if self.motor_running == running {
            return;
        }
        self.controller
            .set_power_saving(if running {
                PowerSaveMode::None
            } else {
                PowerSaveMode::Minimum
            })
            .unwrap();
        self.motor_running = running;
    }

    /// At most one ingress packet per call; never wait for a client or radio.
    pub fn link_connected(&self) -> bool {
        self.connected
    }

    /// At most one ingress packet per call; never wait for a client or radio.
    pub fn poll(&mut self, now: u64, allow_reconnect: bool) -> Option<(usize, Action)> {
        self.changed = false;
        if now >= self.next_check {
            self.next_check = now + 1_000_000;
            let connected = self.controller.is_connected().unwrap_or(false);
            if self.connected && !connected {
                self.address = None;
                self.changed = true;
                self.iface.update_ip_addrs(|a| a.clear());
                self.iface.routes_mut().remove_default_ipv4_route();
                self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).reset();
                for connection in &mut self.connections {
                    self.sockets
                        .get_mut::<tcp::Socket>(connection.handle)
                        .abort();
                }
            }
            self.connected = connected;
            if !connected && allow_reconnect {
                let _ = self.controller.connect();
            }
        }
        let instant = Instant::from_micros(now as i64);
        self.iface
            .poll_ingress_single(instant, &mut self.device, &mut self.sockets);
        self.iface
            .poll_egress(instant, &mut self.device, &mut self.sockets);
        if let Some(event) = self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            match event {
                dhcpv4::Event::Configured(config) => {
                    let address = config.address.address();
                    if self.address != Some(address) {
                        self.changed = true;
                    }
                    self.address = Some(address);
                    self.iface.update_ip_addrs(|a| {
                        a.clear();
                        a.push(IpCidr::Ipv4(config.address)).unwrap();
                    });
                    if let Some(router) = config.router {
                        let _ = self.iface.routes_mut().add_default_ipv4_route(router);
                    }
                }
                dhcpv4::Event::Deconfigured => {
                    self.changed = self.address.take().is_some();
                    self.iface.update_ip_addrs(|a| a.clear());
                    self.iface.routes_mut().remove_default_ipv4_route();
                }
            }
        }
        for (index, connection) in self.connections.iter_mut().enumerate() {
            let socket = self.sockets.get_mut::<tcp::Socket>(connection.handle);
            if !socket.is_open() {
                connection.request.clear();
                connection.response = None;
                connection.touched = now;
                let _ = socket.listen(80);
            }
            // Listening time is not client inactivity: a SYN arriving after
            // a long idle period must get a fresh timeout window.
            if !socket.is_active() {
                connection.touched = now;
            }
            if now.saturating_sub(connection.touched) > 2_000_000 && socket.is_active() {
                socket.abort();
                continue;
            }
            if let Some(response) = &mut connection.response {
                if socket.can_send() {
                    let header_len = response.header.len();
                    let data = if response.sent < header_len {
                        &response.header.as_bytes()[response.sent..]
                    } else {
                        &response.body()[response.sent - header_len..]
                    };
                    let count = socket.send_slice(data).unwrap_or(0);
                    response.sent += count;
                    if count > 0 {
                        connection.touched = now;
                    }
                    if response.sent == header_len + response.body().len() {
                        socket.close();
                    }
                }
                continue;
            }
            if socket.can_recv() {
                let mut bytes = [0; 256];
                let count = socket.recv_slice(&mut bytes).unwrap_or(0);
                connection.touched = now;
                if connection
                    .request
                    .extend_from_slice(&bytes[..count])
                    .is_err()
                {
                    connection.response = Some(Response::error(413, "request too large"));
                    continue;
                }
                match api::parse(&connection.request) {
                    Ok(Some(Action::Page)) => {
                        connection.response =
                            Some(Response::new(200, "text/html", Body::Static(PAGE)))
                    }
                    Ok(Some(Action::HapticTrace)) => {
                        connection.response = Some(match crate::motor_trace::snapshot() {
                            Some(bytes) => {
                                Response::new(200, "application/octet-stream", Body::Owned(bytes))
                            }
                            None => Response::error(503, "trace memory unavailable"),
                        });
                    }
                    Ok(Some(action)) => return Some((index, action)),
                    Ok(None) if !socket.may_recv() => socket.abort(),
                    Ok(None) => {}
                    Err(error) => {
                        connection.response = Some(Response::error(
                            error.status(),
                            "invalid request or parameters",
                        ))
                    }
                }
            }
        }
        None
    }

    pub fn respond(&mut self, index: usize, response: Response) {
        self.connections[index].response = Some(response.prepare());
    }
}
