use std::sync::mpsc;

use eframe::egui;
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::datastream::{TiltDataManager, run_stream};
use shared_icd::tilt::Data;

pub struct StreamPlottingApp {
    data: TiltDataManager,
    rx: mpsc::Receiver<Data>,
}

impl StreamPlottingApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = mpsc::channel();
        let tx = tx.clone();
        let ctx = cc.egui_ctx.clone();
        let mut data = TiltDataManager::new();
        data.points_to_plot = 600;
        run_stream(ctx, tx);
        Self { data, rx }
    }
}

impl eframe::App for StreamPlottingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Ok(dt) = self.rx.try_recv() {
                self.data.add_datapoint(dt);
            };

            ui.heading("Gyro data");

            let data_to_plot = self.data.get_plot_data();
            let gyro_p = Line::new("gyro_p", PlotPoints::new(data_to_plot.gyro_p));
            let gyro_l = Line::new("gyro_r", PlotPoints::new(data_to_plot.gyro_r));
            let gyro_y = Line::new("gyro_y", PlotPoints::new(data_to_plot.gyro_y));
            let accl_x = Line::new("accl_x", PlotPoints::new(data_to_plot.accl_x));
            let accl_y = Line::new("accl_y", PlotPoints::new(data_to_plot.accl_y));
            let accl_z = Line::new("accl_z", PlotPoints::new(data_to_plot.accl_z));

            Plot::new("gyro_plot")
                .view_aspect(2.0)
                .legend(Legend::default())
                .show(ui, |plot_ui| {
                    plot_ui.line(gyro_p);
                    plot_ui.line(gyro_l);
                    plot_ui.line(gyro_y);
                });

            ui.heading("Accelerometer data");

            Plot::new("accl_plot")
                .view_aspect(2.0)
                .legend(Legend::default())
                .show(ui, |plot_ui| {
                    plot_ui.line(accl_x);
                    plot_ui.line(accl_y);
                    plot_ui.line(accl_z);
                });

            // A slider to select the datapoints to plot (10 to 10_000)
            ui.add(
                egui::Slider::new(&mut self.data.points_to_plot, 10..=1000)
                    .text("Points to plot (10 to 1000)"),
            );
        });
    }
}
