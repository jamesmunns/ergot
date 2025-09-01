use std::{sync::mpsc, time::Instant};

use eframe::egui;
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::datastream::{TiltDataManager, run_stream};
use shared_icd::tilt::Data;

#[derive(Clone, Copy, PartialEq)]
enum StreamMode {
    Simulated,
    Ergot,
}

pub struct StreamPlottingApp {
    data: TiltDataManager,
    rx: mpsc::Receiver<Data>,
    stack: crate::RouterStack,
    stream_mode: StreamMode,
    frame_time: Instant,
    frame_count: u16,
    dpts_sum: u64,
    avg_data_time: Instant,
    avg_data_rate: f64,
}

impl StreamPlottingApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, stack: crate::RouterStack) -> Self {
        let stream_mode = StreamMode::Ergot;
        let (tx, rx) = mpsc::channel();
        let tx = tx.clone();
        let mut data = TiltDataManager::new();
        data.points_to_plot = 600;
        run_stream(tx, Some(stack.clone()));
        Self {
            data,
            rx,
            stack,
            stream_mode,
            frame_time: Instant::now(),
            frame_count: 0,
            dpts_sum: 0,
            avg_data_time: Instant::now(),
            avg_data_rate: 0.0,
        }
    }

    fn create_data(&mut self) {
        let (tx, rx) = mpsc::channel();
        let tx = tx.clone();
        let mut data = TiltDataManager::new();
        data.points_to_plot = 2_000;

        match self.stream_mode {
            StreamMode::Simulated => {
                run_stream(tx, None);
            }
            StreamMode::Ergot => {
                run_stream(tx, Some(self.stack.clone()));
            }
        }

        self.rx = rx;
        self.data = data;
    }
}

impl eframe::App for StreamPlottingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            while let Ok(dt) = self.rx.try_recv() {
                self.data.add_datapoint(dt);
                self.dpts_sum += 1;
            }
            ui.heading("Gyro data");

            let data_to_plot = self.data.get_plot_data();
            let gyro_p = Line::new("gyro_p", PlotPoints::from(data_to_plot.gyro_p));
            let gyro_l = Line::new("gyro_r", PlotPoints::from(data_to_plot.gyro_r));
            let gyro_y = Line::new("gyro_y", PlotPoints::from(data_to_plot.gyro_y));
            let accl_x = Line::new("accl_x", PlotPoints::from(data_to_plot.accl_x));
            let accl_y = Line::new("accl_y", PlotPoints::from(data_to_plot.accl_y));
            let accl_z = Line::new("accl_z", PlotPoints::from(data_to_plot.accl_z));

            let plt_height = ui.available_height() / 2.2;

            let link_group_id = ui.id().with("linked_demo");

            let xlbl = match self.stream_mode {
                StreamMode::Ergot => "Time (s)",
                StreamMode::Simulated => "Time (arbitrary)",
            };

            Plot::new("gyro_plot")
                .view_aspect(2.0)
                .legend(Legend::default())
                .x_axis_label(xlbl)
                .y_axis_label("Angular rate (dps)")
                .link_axis(link_group_id, true)
                .link_cursor(link_group_id, true)
                .height(plt_height)
                .width(ui.available_width())
                .show(ui, |plot_ui| {
                    plot_ui.line(gyro_p);
                    plot_ui.line(gyro_l);
                    plot_ui.line(gyro_y);
                });

            ui.heading("Accelerometer data");

            Plot::new("accl_plot")
                .view_aspect(2.0)
                .legend(Legend::default())
                .x_axis_label(xlbl)
                .y_axis_label("Acceleration (g)")
                .link_axis(link_group_id, true)
                .link_cursor(link_group_id, true)
                .height(plt_height)
                .width(ui.available_width())
                .show(ui, |plot_ui| {
                    plot_ui.line(accl_x);
                    plot_ui.line(accl_y);
                    plot_ui.line(accl_z);
                });

            // Controls for the plots
            ui.horizontal_centered(|ui| {
                ui.add(
                    egui::Slider::new(&mut self.data.points_to_plot, 10..=10_000)
                        .text("Points to plot (10 to 10000)"),
                );

                ui.add_space(30.);

                ui.label("Data source:");
                if ui
                    .add(egui::RadioButton::new(
                        self.stream_mode == StreamMode::Ergot,
                        "Ergot",
                    ))
                    .clicked()
                {
                    self.stream_mode = StreamMode::Ergot;
                    self.create_data();
                }
                if ui
                    .add(egui::RadioButton::new(
                        self.stream_mode == StreamMode::Simulated,
                        "Simulated",
                    ))
                    .clicked()
                {
                    self.stream_mode = StreamMode::Simulated;
                    self.create_data();
                }

                ui.add_space(30.);
                let now = Instant::now();
                let elapsed = (now - self.frame_time).as_secs_f64();
                self.frame_count += 1;
                if self.frame_count >= 60 {
                    self.avg_data_rate =
                        (self.dpts_sum as f64) / (now - self.avg_data_time).as_secs_f64();
                    self.frame_count = 0;
                    self.avg_data_time = now;
                    self.dpts_sum = 0;
                }

                ui.label(format!(
                    "{:.0} fps, Data rate (60 frame avg.): {:.0} Hz",
                    1. / elapsed,
                    self.avg_data_rate
                ));
                self.frame_time = now;
            });
        });

        ctx.request_repaint();
    }
}
