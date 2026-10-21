//! TCP, Wi-Fi management, and response encoding never execute on control.
use super::{Response, Transport};
use alloc::boxed::Box;
use core::{cell::RefCell, ffi::c_void};
use esp_hal::time::Duration;
use motor_control::api::Action;
use smoltcp::wire::Ipv4Address;

struct Data {
    action: Option<(usize, Action)>,
    response: Option<(usize, Response)>,
    allow_reconnect: bool,
    running: bool,
    address: Option<Ipv4Address>,
    connected: bool,
    revision: u32,
}
type Shared = critical_section::Mutex<RefCell<Data>>;

pub struct Server {
    shared: &'static Shared,
    pub address: Option<Ipv4Address>,
    pub changed: bool,
    revision: u32,
}
struct Worker {
    transport: Transport<'static>,
    shared: &'static Shared,
}

impl Server {
    pub fn new(
        radio: &'static esp_radio::Controller<'static>,
        wifi: esp_hal::peripherals::WIFI<'static>,
        now: u64,
    ) -> Self {
        let shared = Box::leak(Box::new(Shared::new(RefCell::new(Data {
            action: None,
            response: None,
            allow_reconnect: true,
            running: false,
            address: None,
            connected: false,
            revision: 0,
        }))));
        let worker = Box::new(Worker {
            transport: Transport::new(radio, wifi, now),
            shared,
        });
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&*worker);
        // SAFETY: transfer the unique, Send allocation into a never-returning
        // worker. Control retains only the static synchronized mailboxes.
        let handle = unsafe {
            esp_radio_rtos_driver::task_create(
                "motor-http",
                entry,
                Box::into_raw(worker).cast(),
                // Below radio/control workers, above idle application work.
                // Network code must not busy-wait above the SDK TX worker.
                2,
                Some(0),
                12288,
            )
        };
        assert!(!handle.is_null());
        Self {
            shared,
            address: None,
            changed: false,
            revision: 0,
        }
    }

    pub fn set_motor_running(&mut self, running: bool) {
        critical_section::with(|cs| self.shared.borrow(cs).borrow_mut().running = running);
    }
    pub fn link_connected(&self) -> bool {
        critical_section::with(|cs| self.shared.borrow(cs).borrow().connected)
    }
    pub fn poll(&mut self, _now: u64, allow_reconnect: bool) -> Option<(usize, Action)> {
        critical_section::with(|cs| {
            let mut data = self.shared.borrow(cs).borrow_mut();
            data.allow_reconnect = allow_reconnect;
            self.address = data.address;
            self.changed = self.revision != data.revision;
            self.revision = data.revision;
            data.action.take()
        })
    }
    pub fn respond(&mut self, index: usize, response: Response) {
        critical_section::with(|cs| {
            let mut data = self.shared.borrow(cs).borrow_mut();
            assert!(data.response.is_none());
            data.response = Some((index, response));
        });
    }
}

extern "C" fn entry(param: *mut c_void) {
    // SAFETY: new() transfers the one Send allocation exactly once.
    let Worker {
        mut transport,
        shared,
    } = *unsafe { Box::from_raw(param.cast::<Worker>()) };
    let thread = esp_rtos::CurrentThreadHandle::get();
    let mut waiting = false;
    loop {
        let (running, reconnect, response) = critical_section::with(|cs| {
            let mut data = shared.borrow(cs).borrow_mut();
            (data.running, data.allow_reconnect, data.response.take())
        });
        // All potentially slow work is outside the mailbox lock.
        transport.set_motor_running(running);
        if let Some((index, response)) = response {
            transport.respond(index, response);
            waiting = false;
        }
        if !waiting {
            let started = crate::now_us();
            let action = transport.poll(started, reconnect);
            crate::record_max(
                &crate::NETWORK_POLL_MAX_US,
                crate::now_us().saturating_sub(started),
            );
            critical_section::with(|cs| {
                let mut data = shared.borrow(cs).borrow_mut();
                data.address = transport.address;
                data.connected = transport.link_connected();
                if transport.changed {
                    data.revision = data.revision.wrapping_add(1);
                }
                if let Some(action) = action {
                    assert!(data.action.is_none());
                    data.action = Some(action);
                    waiting = true;
                }
            });
        }
        thread.delay(Duration::from_micros(if running { 500 } else { 5000 }));
    }
}
