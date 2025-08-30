use stream_plotting::StreamPlottingApp;

#[tokio::main]
async fn main() {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Ergot Plotting Demo",
        native_options,
        Box::new(|cc| Ok(Box::new(StreamPlottingApp::new(cc)))),
    )
    .unwrap();
}
