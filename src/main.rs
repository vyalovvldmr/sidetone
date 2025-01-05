use anyhow::Context;
use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Host,
};
use ringbuf::{
    traits::{Consumer, Producer, Split},
    HeapRb,
};
use tracing::{debug, error, info, level_filters::LevelFilter};
use tracing_subscriber::EnvFilter;

const LATENCY: std::time::Duration = std::time::Duration::from_millis(11);

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
    let config: cpal::StreamConfig = input_device.default_input_config()?.into();
    debug!("input device config {:#?}", &config);
    let latency_frames = (LATENCY.as_millis() as f32 / 1_000.0) * config.sample_rate.0 as f32;
    let latency_samples = latency_frames as usize * config.channels as usize;
    let ring = HeapRb::<f32>::new(latency_samples);
    let (mut producer, mut consumer) = ring.split();

    for _ in 0..latency_samples {
        producer.try_push(0.0).ok();
    }

    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        let mut output_fell_behind = false;
        for &sample in data {
            if producer.try_push(sample).is_err() {
                output_fell_behind = true;
            }
        }
        if output_fell_behind {
            debug!("output stream fell behind: try increasing latency");
        }
    };

    let output_data_fn = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let mut input_fell_behind = false;
        for sample in data {
            *sample = match consumer.try_pop() {
                Some(s) => s,
                None => {
                    input_fell_behind = true;
                    0.0
                }
            };
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
    let input_stream = input_device.build_input_stream(&config, input_data_fn, err_fn, None)?;
    let output_stream = output_device.build_output_stream(&config, output_data_fn, err_fn, None)?;
    input_stream.play()?;
    output_stream.play()?;

    serve()?;
    Ok(())
}
