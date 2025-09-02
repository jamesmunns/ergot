//! Module to manage the data stream from ergot (simulated or real) and provide the
//! TiltDataManager that holds data and prepares them for plotting from the UI.

use std::{
    pin::pin,
    sync::mpsc,
    time::{Duration, Instant},
};

use egui_plot::PlotPoint;
use shared_icd::tilt::{Data, DataTopic};
use tokio::time::interval;

use crate::DataTimed;

const GYRO_SCALER: f64 = i16::MAX as f64 / 245.0; // +/-245 dps range, 16-bit resolution
const ACCEL_SCALER: f64 = i16::MAX as f64 / 2.0; // +/-2g range, 16-bit resolution
const TIME_SCALER: f64 = 1_000_000.; // 1MHz

/// Holds all the data vectors ready for plotting.
#[derive(Default)]
pub struct DataToPlot {
    pub gyro_p: Vec<PlotPoint>,
    pub gyro_r: Vec<PlotPoint>,
    pub gyro_y: Vec<PlotPoint>,
    pub accl_x: Vec<PlotPoint>,
    pub accl_y: Vec<PlotPoint>,
    pub accl_z: Vec<PlotPoint>,
}

/// Holds slices of the data for plotting to avoid unnecessary copies.
pub struct DataSlices<'a> {
    pub gyro_p: &'a [PlotPoint],
    pub gyro_r: &'a [PlotPoint],
    pub gyro_y: &'a [PlotPoint],
    pub accl_x: &'a [PlotPoint],
    pub accl_y: &'a [PlotPoint],
    pub accl_z: &'a [PlotPoint],
}

/// Manages datapoints that are added and prepares them for plotting.
pub struct TiltDataManager {
    plot_data: DataToPlot,
    pub points_to_plot: u64,
    num_datapoints: u64,
}

impl TiltDataManager {
    /// Create a new TiltDataMangager, setting points to plot to 10_000.
    pub fn new() -> Self {
        Self {
            plot_data: DataToPlot::default(),
            points_to_plot: 10_000,
            num_datapoints: 0,
        }
    }

    /// Add a new data point to the manager.
    pub fn add_datapoint(&mut self, data: Data, mcu_time: u64) {
        let ts = mcu_time as f64 / TIME_SCALER;
        self.plot_data.gyro_p.push(PlotPoint {
            x: ts,
            y: data.gyro_p as f64 / GYRO_SCALER,
        });
        self.plot_data.gyro_r.push(PlotPoint {
            x: ts,
            y: data.gyro_r as f64 / GYRO_SCALER,
        });
        self.plot_data.gyro_y.push(PlotPoint {
            x: ts,
            y: data.gyro_y as f64 / GYRO_SCALER,
        });
        self.plot_data.accl_x.push(PlotPoint {
            x: ts,
            y: data.accl_x as f64 / ACCEL_SCALER,
        });
        self.plot_data.accl_y.push(PlotPoint {
            x: ts,
            y: data.accl_y as f64 / ACCEL_SCALER,
        });
        self.plot_data.accl_z.push(PlotPoint {
            x: ts,
            y: data.accl_z as f64 / ACCEL_SCALER,
        });
        self.num_datapoints += 1;
    }

    /// Get the data to plot, only the last `points_to_plot` points.
    pub fn get_plot_data(&self) -> DataSlices<'_> {
        let start = if self.num_datapoints > self.points_to_plot {
            (self.num_datapoints - self.points_to_plot) as usize
        } else {
            0
        };

        DataSlices {
            gyro_p: &self.plot_data.gyro_p[start..],
            gyro_r: &self.plot_data.gyro_r[start..],
            gyro_y: &self.plot_data.gyro_y[start..],
            accl_x: &self.plot_data.accl_x[start..],
            accl_y: &self.plot_data.accl_y[start..],
            accl_z: &self.plot_data.accl_z[start..],
        }
    }
}

/// Spawns a tokio task that simulates fetching data from an external source.
pub fn run_stream(tx: mpsc::Sender<DataTimed>, stack: Option<crate::RouterStack>) {
    match stack {
        Some(stack) => {
            tokio::spawn(async move {
                fetch_data_ergot(tx, stack).await;
            });
        }
        None => {
            tokio::spawn(async move {
                fetch_data_simulated(tx).await;
            });
        }
    };
}

/// Fetching the data from ergot.
async fn fetch_data_ergot(tx: mpsc::Sender<DataTimed>, stack: crate::RouterStack) {
    let subber = stack.topics().heap_bounded_receiver::<DataTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();

    loop {
        let msg = hdl.recv().await;
        // Each update contains four samples, use the most recent
        if tx.send(DataTimed { data: msg.t.inner[3].clone(), time: msg.t.mcu_timestamp }).is_err() {
            break;
        }
    }
}

/// Fetching simulated data.
async fn fetch_data_simulated(tx: mpsc::Sender<DataTimed>) {
    let mut it = 0;
    let start = Instant::now();

    // The real data is sampled at 6.66kHz, sent in batches of four.
    let ival = (1.0f64 / 6664.0) * 4.0;
    let mut ticker = interval(Duration::from_secs_f64(ival));

    loop {
        ticker.tick().await;
        it += 1;
        let ts = it as f64 * 0.01;

        let gyro_p = (ts.sin() * 1000.) as i16;
        let gyro_r = (ts.cos() * 1000.) as i16;
        let gyro_y = (ts.sin().powf(2.) * 300. + 500.) as i16;
        let accl_x = (16384. * (it as f64 % 10. / 100. + 1.)) as i16;
        let accl_y = if (it / 100) % 2 == 0 { 12000 } else { 0 };
        let accl_z = ((it % 100) * 75) as i16;

        let data_to_send = DataTimed {
            data: Data {
                gyro_p,
                gyro_r,
                gyro_y,
                accl_x,
                accl_y,
                accl_z,
                imu_timestamp: it,
            },
            // Don't run the simulation for more than u64::MAX microseconds (584542 years)
            time: start.elapsed().as_micros() as u64,
        };
        if tx.send(data_to_send).is_err() {
            break;
        };
    }
}
