#![no_std]
#![no_main]

use core::{cell::{Cell, RefCell}, mem};

use embassy_executor::Spawner;
use embassy_nrf::{config::Config, pac::usbd::vals::Io};
use embassy_time::{Timer, Duration};
use embassy_nrf::{gpio::{Level, Output, OutputDrive}};
use embassy_nrf::config::{HfclkSource, LfclkSource};
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};
use defmt::*;

use nrf_softdevice::{Softdevice, ble::gatt_server::characteristic, raw};
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload, ServiceList, ServiceUuid16,
    AdvertisementDataType
};
use nrf_softdevice::ble::gatt_server::builder::ServiceBuilder;
use nrf_softdevice::ble::gatt_server::characteristic::{Attribute, Metadata, Properties};
use nrf_softdevice::ble::gatt_server::{CharacteristicHandles, RegisterError, WriteOp, set_sys_attrs};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection, Uuid, SecurityMode, EncryptionInfo, IdentityKey, MasterId,};
use nrf_softdevice::ble::security::{SecurityHandler, IoCapabilities,};

const PERIPHERAL_REQUESTS_SECURITY: bool = false;

#[embassy_executor::task]
async fn softdevice_task(sd: &'static Softdevice) -> ! {
    sd.run().await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = Config::default();
    config.gpiote_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    config.time_interrupt_priority = embassy_nrf::interrupt::Priority::P2;
    config.hfclk_source = HfclkSource::ExternalXtal;
    config.lfclk_source = LfclkSource::ExternalXtal;

    let p = embassy_nrf::init(config);

    Timer::after(Duration::from_millis(100)).await;

    let config = nrf_softdevice::Config {
        clock: Some(raw::nrf_clock_lf_cfg_t {
            source: raw::NRF_CLOCK_LF_SRC_RC as u8,
            rc_ctiv: 16,
            rc_temp_ctiv: 2,
            accuracy: raw::NRF_CLOCK_LF_ACCURACY_500_PPM as u8,
        }),
        conn_gap: Some(raw::ble_gap_conn_cfg_t {
            conn_count: 1,
            event_length: 24,
        }),
        conn_gatt: Some(raw::ble_gatt_conn_cfg_t { att_mtu: 256 }),
        gatts_attr_tab_size: Some(raw::ble_gatts_cfg_attr_tab_size_t {
            attr_tab_size: raw::BLE_GATTS_ATTR_TAB_SIZE_DEFAULT,
        }),
        gap_role_count: Some(raw::ble_gap_cfg_role_count_t {
            adv_set_count: 1,
            periph_role_count: 3,
            central_role_count: 3,
            central_sec_count: 0,
            _bitfield_1: raw::ble_gap_cfg_role_count_t::new_bitfield_1(0),
        }),
        gap_device_name: Some(raw::ble_gap_cfg_device_name_t {
            p_value: b"HelloRust" as *const u8 as _,
            current_len: 9,
            max_len: 9,
            write_perm: unsafe { mem::zeroed() },
            _bitfield_1: raw::ble_gap_cfg_device_name_t::new_bitfield_1(raw::BLE_GATTS_VLOC_STACK as u8),
        }),
        ..Default::default()
    };

    let sd= Softdevice::enable(&config);
    
    let server = Server::new(sd).unwrap();
    unwrap!(spawner.spawn(softdevice_task(sd)));

    let mut led = Output::new(p.P0_15, Level::Low, OutputDrive::Standard);

    // static uuid128: [u8; 16]= [0x81, 0x1b, 0x86, 0x4d, 0x71, 0x8c, 0xb0, 0x35,
    //         0x80, 0x4d, 0x92, 0xc4, 0x76, 0x12, 0x43, 0xc0];
    static ADV_DATA: LegacyAdvertisementPayload = LegacyAdvertisementBuilder::new()
        .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
        .full_name("HelloRust")
        .build();

    static SCAN_DATA:[u8; 0]= [];

    static BONDER: StaticCell<Bounder> = StaticCell::new();
    let bonder = BONDER.init(Bounder::default());

    loop {
        let config = peripheral::Config::default();
        let adv= peripheral::ConnectableAdvertisement::ScannableUndirected {
            adv_data: &ADV_DATA,
            scan_data: &SCAN_DATA,
        };

        info!("Starting advertising...");

        let conn = unwrap!(peripheral::advertise_pairable(sd, adv, &config, bonder)
            .await);
        
        if PERIPHERAL_REQUESTS_SECURITY {
            if let Err(err) = conn.request_security() {
                error!("Security request failed: {:?}", err);
                continue;
            }
        }

        let e = gatt_server::run(&conn, &server, |_| {}).await;

    }
}

#[derive(Debug, Clone, Copy)]
struct Peer {
    master_id: MasterId,
    key: EncryptionInfo,
    peer_id: IdentityKey,
}

pub struct Bounder {
    peer: Cell<Option<Peer>>,
    sys_attrs: RefCell<heapless::Vec<u8, 62>>,
}

impl Default for Bounder {
    fn default() -> Self {
        Self {
            peer: Cell::new(None),
            sys_attrs: Default::default(),
        }
    }
}

impl SecurityHandler for Bounder {
    fn io_capabilities(&self) -> IoCapabilities {
        IoCapabilities::DisplayOnly
    }

    fn can_bond(&self, _conn: &Connection) -> bool {
        true
    }

    fn on_bonded(&self, _conn: &Connection, master_id: MasterId, key: EncryptionInfo, peer_id: IdentityKey) {
        
        self.sys_attrs.borrow_mut().clear();
        self.peer.set(Some(Peer {
            master_id,
            key,
            peer_id,
        }));
    }

    fn get_key(&self, _conn: &Connection, master_id: MasterId) -> Option<EncryptionInfo> {
        self.peer.get().and_then(|peer| { 
            (peer.master_id == master_id).then_some(peer.key) 
        })
    }

    fn save_sys_attrs(&self, conn: &Connection) {

        debug!("saving system attributes for: {}", conn.peer_address());

        if let Some(peer) = self.peer.get() {
            if peer.peer_id.is_match(conn.peer_address()) {
                let mut sys_attrs = self.sys_attrs.borrow_mut();
                let capacity = sys_attrs.capacity();
                unwrap!(sys_attrs.resize(capacity, 0));
                let len = unwrap!(gatt_server::get_sys_attrs(conn, &mut sys_attrs)) as u16;
                sys_attrs.truncate(usize::from(len));
                // In a real application you would want to signal another task to permanently store sys_attrs for this connection's peer
            }
        }
    }

    fn load_sys_attrs(&self, conn: &Connection) {
        let addr = conn.peer_address();
        debug!("loading system attributes for: {}", addr);

        let attrs = self.sys_attrs.borrow();
        // In a real application you would search all stored peers to find a match
        let attrs = if self.peer.get().map(|peer| peer.peer_id.is_match(addr)).unwrap_or(false) {
            (!attrs.is_empty()).then_some(attrs.as_slice())
        } else {
            None
        };

        unwrap!(set_sys_attrs(conn, attrs));
    }
}

pub struct TempService{
    value_handle: u16,
    cccd_handle: u16,
}

impl TempService {
    pub fn new(sd: &mut Softdevice) -> Result<Self, RegisterError> {
        let uuid: [u8; 16]= [0x81, 0x1b, 0x86, 0x4d, 0x71, 0x8c, 0xb0, 0x35,
            0x80, 0x4d, 0x92, 0xc4, 0x76, 0x12, 0x43, 0xc0];
        let mut service_builder = ServiceBuilder::new(sd, Uuid::new_128(&uuid))?;

        let attr = Attribute::new(&[0u8; 2])
                .security(SecurityMode::Open)
                .deferred_read()
                .read_security(SecurityMode::Open); // セキュリティモードを設定?
        let metadata = Metadata::new(Properties::new().read().notify()); // 読み取りと通知を許可
        let characteristic_builder = service_builder.add_characteristic(
            Uuid::new_128(&[
                0x06, 0x01, 0xa0, 0x01, 0x0e, 0xca, 0xb5, 0xab,
                0xed, 0xc2, 0x88, 0x7f, 0xd5, 0xf3, 0x2b, 0x84,
            ]),
            attr,
            metadata,
        )?;
        let characteristic_handles = characteristic_builder.build();

        let _service_handles = service_builder.build();
        Ok(TempService {
            value_handle: characteristic_handles.value_handle,
            cccd_handle: characteristic_handles.cccd_handle,
        })
    }

    pub fn temp_get(&self, sd: &Softdevice) -> Result<u8, gatt_server::GetValueError> {
        let buf = &mut [0u8];
        gatt_server::get_value(sd, self.value_handle, buf)?;
        Ok(buf[0])
    }

    pub fn temp_set(&self, sd: &Softdevice, data: u8) -> Result<(), gatt_server::SetValueError> {
        gatt_server::set_value(sd, self.value_handle, &[data])
    }

    pub fn temp_notify(&self, conn: &Connection, data: u8) -> Result<(), gatt_server::NotifyValueError> {
        gatt_server::notify_value(conn, self.value_handle, &[data])
    }

    pub fn on_write(&self, handle: u16, data: &[u8]) {
        if handle == self.cccd_handle && !data.is_empty() {
            // 書き込み処理
            info!("Temperature characteristic written: {:?}", data);
        }
    }
}
struct Server {
    temp: TempService
}

impl Server {
    pub fn new(sd: &mut Softdevice) -> Result<Self, RegisterError> {
        Ok(Server {
            temp: TempService::new(sd)?
        })
    }
}

impl gatt_server::Server for Server {
    type Event = ();

    fn on_write(
        &self,
        _conn: &Connection,
        handle: u16,
        _op: WriteOp,
        _offset: usize,
        data: &[u8],
    ) -> Option<Self::Event> {
        info!("on_write called with handle");
        self.temp.on_write(handle, data);
        None
    }

    fn on_deferred_read(&self, handle: u16, offset: usize, reply: nrf_softdevice::ble::DeferredReadReply) -> Option<Self::Event> {
        // 1. 送信したいデータ (2バイト) を定義
        const DUMMY_VALUE: u16 = 0x1234;
        let dummy_bytes: [u8; 2] = DUMMY_VALUE.to_le_bytes();
        let value_option: Option<&[u8]> = Some(dummy_bytes.as_slice());
        
        reply.reply(Ok(value_option));
            
        
        None
    }

}