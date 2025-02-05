use anyhow::Context;
use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Host,
};
use std::sync::mpsc::sync_channel;
use tracing::{debug, error, info, level_filters::LevelFilter};
use tracing_subscriber::EnvFilter;

const INITIAL_LATENCY: std::time::Duration = std::time::Duration::from_millis(1000);
const BUFFER_SIZE: usize = 550;

#[derive(Parser, Debug)]
#[command(version, about = "sidetone", long_about = None)]
struct Cli {
    /// The input audio device to use
    #[arg(short, long, value_name = "IN", default_value_t = String::from("default"))]
    input_device: String,

    /// The output audio device to use
    #[arg(short, long, value_name = "OUT", default_value_t = String::from("default"))]
    output_device: String,
}

fn init_logging() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env()?,
        )
        .init();
    Ok(())
}

fn serve() -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    ctrlc::set_handler(move || {
        if tx.send(()).is_err() {
            error!("could not send ctrl-c signal on channel")
        }
    })
    .context("could not set ctrl-c handler")?;
    rx.recv()?;
    Ok(())
}

fn find_input_device(device_name: &str, host: &Host) -> Option<Device> {
    if device_name == "default" {
        host.default_input_device()
    } else {
        host.input_devices()
            .ok()?
            .find(|x| x.name().map(|y| y == device_name).unwrap_or(false))
    }
}

fn find_output_device(device_name: &str, host: &Host) -> Option<Device> {
    if device_name == "default" {
        host.default_output_device()
    } else {
        host.output_devices()
            .ok()?
            .find(|x| x.name().map(|y| y == device_name).unwrap_or(false))
    }
}

fn main() -> anyhow::Result<()> {
    init_logging()?;
    let args = Cli::parse();
    let host = cpal::default_host();
    let input_device =
        find_input_device(&args.input_device, &host).context("failed to find input device")?;
    let output_device =
        find_output_device(&args.output_device, &host).context("failed to find output device")?;
    let input_config: cpal::StreamConfig = input_device.default_input_config()?.into();
    debug!("input device config {:#?}", &input_config);
    let output_config: cpal::StreamConfig = output_device.default_output_config()?.into();
    debug!("output device config {:#?}", &output_config);
    if input_config.sample_rate.0 != output_config.sample_rate.0 {
        anyhow::bail!("The sampling frequency of the input device must be the same as the sampling frequency of the output device");
    }
    let latency_frames = (INITIAL_LATENCY.as_secs() as f32) * output_config.sample_rate.0 as f32;
    let latency_samples = latency_frames as usize * output_config.channels as usize;
    let (sender, receiver) = sync_channel(BUFFER_SIZE);

    for _ in 0..latency_samples {
        sender.try_send(0.0).ok();
    }

    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        let mut output_fell_behind = false;
        for &sample in data {
            if sender.try_send(sample).is_err() {
                output_fell_behind = true;
            }
        }
        if output_fell_behind {
            debug!("output stream fell behind: try increasing latency");
        }
    };

    let output_data_fn = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let mut input_fell_behind = false;
        for frame in data.chunks_mut(output_config.channels as usize) {
            if let Ok(_sample) = receiver.try_recv() {
                for sample in frame.iter_mut() {
                    *sample = _sample;
                }
            } else {
                input_fell_behind = true;
            }
        }
        if input_fell_behind {
            debug!("input stream fell behind: try increasing latency");
        }
    };

    let err_fn = |err: cpal::StreamError| {
        error!("an error occurred on stream: {}", err);
    };

    info!(
        "Redirecting audio stream from device '{}' to '{}'",
        input_device.name()?,
        output_device.name()?
    );
    let input_stream =
        input_device.build_input_stream(&input_config, input_data_fn, err_fn, None)?;
    std::thread::sleep(INITIAL_LATENCY);
    let output_stream =
        output_device.build_output_stream(&output_config, output_data_fn, err_fn, None)?;
    input_stream.play()?;
    output_stream.play()?;

    serve()?;
    Ok(())
}
