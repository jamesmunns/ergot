#![no_std]
use ergot::{endpoint, topic};
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

pub mod tilt {

    use super::*;

    endpoint!(PwmSetEndpoint, f32, u64, "pwm/set");
    topic!(DataTopic, Datas, "tilt/data");

    #[derive(Serialize, Deserialize, Schema, Default, Clone, Debug)]
    pub struct Datas {
        pub mcu_timestamp: u64,
        pub inner: [Data; 4],
    }

    #[derive(Serialize, Deserialize, Schema, Default, Clone, Debug)]
    pub struct Data {
        pub gyro_p: i16,
        pub gyro_r: i16,
        pub gyro_y: i16,
        pub accl_x: i16,
        pub accl_y: i16,
        pub accl_z: i16,
        pub imu_timestamp: u32,
    }
}
