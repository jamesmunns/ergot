//! Module to manage the data stream from ergot (currently just simulated data) and provide the
//! TiltDataManager that holds data and prepares them for plotting from the UI.

use std::{
    pin::pin,
    sync::mpsc,
    time::{Duration, Instant},
};

use eframe::egui;

use shared_icd::tilt::{Data, DataTopic};

/// Holds all the data vectors ready for plotting.
#[derive(Default)]
pub struct DataToPlot {
    pub gyro_p: Vec<[f64; 2]>,
    pub gyro_r: Vec<[f64; 2]>,
    pub gyro_y: Vec<[f64; 2]>,
    pub accl_x: Vec<[f64; 2]>,
    pub accl_y: Vec<[f64; 2]>,
    pub accl_z: Vec<[f64; 2]>,
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
    pub fn add_datapoint(&mut self, data: Data) {
        let ts = data.imu_timestamp as f64; // FIXME: convert to seconds
        self.plot_data.gyro_p.push([ts, data.gyro_p as f64]);
        self.plot_data.gyro_r.push([ts, data.gyro_r as f64]);
        self.plot_data.gyro_y.push([ts, data.gyro_y as f64]);
        self.plot_data.accl_x.push([ts, data.accl_x as f64]);
        self.plot_data.accl_y.push([ts, data.accl_y as f64]);
        self.plot_data.accl_z.push([ts, data.accl_z as f64]);
        self.num_datapoints += 1;
    }

    /// Get the data to plot, only the last `points_to_plot` points.
    pub fn get_plot_data(&self) -> DataToPlot {
        let start = if self.num_datapoints > self.points_to_plot {
            (self.num_datapoints - self.points_to_plot) as usize
        } else {
            0
        };
        DataToPlot {
            gyro_p: self.plot_data.gyro_p[start..].to_vec(),
            gyro_r: self.plot_data.gyro_r[start..].to_vec(),
            gyro_y: self.plot_data.gyro_y[start..].to_vec(),
            accl_x: self.plot_data.accl_x[start..].to_vec(),
            accl_y: self.plot_data.accl_y[start..].to_vec(),
            accl_z: self.plot_data.accl_z[start..].to_vec(),
        }
    }
}

/// Spawns a tokio task that simulates fetching data from an external source.
pub fn run_stream(ctx: egui::Context, tx: mpsc::Sender<Data>, stack: Option<crate::RouterStack>) {
    match stack {
        Some(stack) => {
            tokio::spawn(async move {
                fetch_data_ergot(ctx, tx, stack).await;
            });
        }
        None => {
            tokio::spawn(async move {
                fetch_data_simulated(ctx, tx).await;
            });
        }
    };
}

/// Fetching the data from ergot.
async fn fetch_data_ergot(ctx: egui::Context, tx: mpsc::Sender<Data>, stack: crate::RouterStack) {
    let subber = stack.topics().heap_bounded_receiver::<DataTopic>(64, None);
    let subber = pin!(subber);
    let mut hdl = subber.subscribe();
    let mut last_update = Instant::now();

    loop {
        let msg = hdl.recv().await;
        if last_update.elapsed() < Duration::from_millis(5) {
            continue;
        }
        if tx.send(msg.t.inner[3].clone()).is_err() {
            break;
        }
        ctx.request_repaint(); // tell egui to repaint the UI (and get the data form the channel)
        last_update = Instant::now();
    }
}

/// Fetching simulated data.
///
/// Data points at 20 Hz.
async fn fetch_data_simulated(ctx: egui::Context, tx: mpsc::Sender<Data>) {
    let mut it = 0;
    loop {
        it += 1;
        let ts = it as f64 * 0.01;

        let gyro_p = (ts.sin() * 1000.) as i16;
        let gyro_r = (ts.cos() * 1000.) as i16;
        let gyro_y = (ts.sin().powf(2.) * 300. + 500.) as i16;
        let accl_x = (ts.cos().abs() * 800.) as i16;
        let accl_y = if (it / 100) % 2 == 0 { 250 } else { 0 };
        let accl_z = ((it % 100) * 10) as i16;

        let data_to_send = Data {
            gyro_p,
            gyro_r,
            gyro_y,
            accl_x,
            accl_y,
            accl_z,
            imu_timestamp: it,
        };
        if tx.send(data_to_send).is_err() {
            break;
        };
        ctx.request_repaint(); // tell egui to repaint the UI (and get the data form the channel)
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
