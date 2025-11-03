use defmt::info;
use embassy_nrf::saadc::Time;
use embassy_nrf::{Peri, rng::Rng};
use embassy_nrf::peripherals::RNG;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_time::{Timer, Duration};
use embassy_nrf::{bind_interrupts, peripherals, rng};
use rand::Rng as _;
use {defmt_rtt as _, panic_probe as _};

use nrf_softdevice::{Softdevice, random_bytes};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection};
use embassy_sync::channel::Channel;

use crate::{SHARED_NOTIF, Server};

// use embassy_sync::blocking_mutex::CriticalSectionMutex;
// bind_interrupts!(struct Irqs {
//     RNG => rng::InterruptHandler<peripherals::RNG>;
// });

pub static SHARED_COUNT: Mutex<ThreadModeRawMutex, u8> = Mutex::new(0);

pub static SHARED_TEMP: Mutex<ThreadModeRawMutex, [u8; 4]> = Mutex::new([0;4]);
pub static BLE_STATE: Mutex<ThreadModeRawMutex, BLEConnect> = Mutex::new(BLEConnect::Disconnected);

#[derive(PartialEq)]
pub enum BLEConnect {
    Connected,
    Disconnected,
}




#[embassy_executor::task]
pub async fn temp_set_task(sd: &'static Softdevice) {
  

  let mut buf =  [0u8; 4];
  loop {
    let byte = random_bytes(sd, &mut buf);
    if byte.is_ok() {

      let nn: u16 = (buf[0] as u16) << 8 | (buf[1] as u16);
      let rand_f: i32 = ((nn % 15000) as i32) - 2000;
      let rand_f_d: f32 = (rand_f as f32) / 10.0;

      info!("{}", rand_f_d);
      // let min200_1300: i16 = (nn - 200) as i16;
      let arr = rand_f_d.to_le_bytes();

      info!("0x {:?}", arr);
      unsafe  {
        SHARED_TEMP.lock_mut(|cdata|{
          *cdata = arr;
        });
        // SHARED_COUNT.lock_mut(|d|  {
        //       *d = buf[0];
        // });
      }
      // info!("{:?}", buf[0]);
    } 
    
    Timer::after(Duration::from_secs(1)).await;
  }

}


#[embassy_executor::task]
pub async fn temp_notify_task(conn: Connection, server: &'static Server) {
  let mut should_break = false;
  Timer::after(Duration::from_secs(3)).await;
  loop {
    
    {
      BLE_STATE.lock(|b| {
        if *b == BLEConnect::Disconnected {
          should_break = true;
        }
      });
    }

    if should_break {
        break;
    }

    {
      SHARED_NOTIF.lock(|cbool|{
        if *cbool {
          SHARED_TEMP.lock(|cdata|{
            server.temp.temp_notify(&conn, cdata);
          });
        }
      });
    }

    Timer::after(Duration::from_secs(1)).await;
  }
}

#[embassy_executor::task]
pub async fn gap_handler(sd: &'static mut Softdevice) {
  // let event = sd.ble_e
}