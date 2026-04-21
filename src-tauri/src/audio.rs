use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct ActiveRecording {
    stream: cpal::Stream,
    samples: Arc<Mutex<Vec<i16>>>,
    sample_rate: u32,
    channels: u16,
    amplitude: Arc<AtomicU32>,
}

impl ActiveRecording {
    /// Latest peak sample level from the last input callback, in the range
    /// `[0.0, 1.0]`. Used by the UI to animate the speaking indicator.
    pub fn amplitude_handle(&self) -> Arc<AtomicU32> {
        self.amplitude.clone()
    }
}

pub struct AudioCapture {
    pub wav_path: PathBuf,
    pub duration_ms: u64,
}

/// Convert an `AtomicU32` amplitude slot back to a `0.0..=1.0` float.
pub fn read_amplitude(slot: &AtomicU32) -> f32 {
    f32::from_bits(slot.load(Ordering::Relaxed)).clamp(0.0, 1.0)
}

fn store_amplitude(slot: &AtomicU32, value: f32) {
    slot.store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

pub fn start_recording() -> Result<ActiveRecording, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "Microphone access required. Open System Settings > Privacy & Security > Microphone and enable FlowingThoughts.".to_string())?;
    let config = device
        .default_input_config()
        .map_err(|e| format!("Failed to read default input config: {e}"))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
    let amplitude = Arc::new(AtomicU32::new(0));
    let err_fn = |err| eprintln!("Audio input stream error: {err}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::I16 => build_i16_stream(
            &device,
            &config.clone().into(),
            samples.clone(),
            amplitude.clone(),
            err_fn,
        )?,
        cpal::SampleFormat::U16 => build_u16_stream(
            &device,
            &config.clone().into(),
            samples.clone(),
            amplitude.clone(),
            err_fn,
        )?,
        cpal::SampleFormat::F32 => build_f32_stream(
            &device,
            &config.clone().into(),
            samples.clone(),
            amplitude.clone(),
            err_fn,
        )?,
        other => {
            return Err(format!("Unsupported sample format: {other:?}"));
        }
    };

    stream
        .play()
        .map_err(|e| format!("Failed to start recording stream: {e}"))?;

    Ok(ActiveRecording {
        stream,
        samples,
        sample_rate,
        channels,
        amplitude,
    })
}

pub fn stop_and_finalize(
    recording: ActiveRecording,
    session_id: u64,
) -> Result<AudioCapture, String> {
    recording
        .stream
        .pause()
        .map_err(|e| format!("Failed to pause recording stream: {e}"))?;

    let samples = {
        let guard = recording
            .samples
            .lock()
            .map_err(|_| "Audio sample buffer lock poisoned".to_string())?;
        guard.clone()
    };

    if samples.is_empty() {
        return Err("No audio was captured".to_string());
    }

    let output_dir = std::env::temp_dir().join("flowing-thoughts");
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create temp audio directory: {e}"))?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("System time error: {e}"))?
        .as_millis();
    let wav_path = output_dir.join(format!("session-{session_id}-{timestamp}.wav"));

    let spec = hound::WavSpec {
        channels: recording.channels,
        sample_rate: recording.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec)
        .map_err(|e| format!("Failed to create wav writer: {e}"))?;

    for sample in samples {
        writer
            .write_sample(sample)
            .map_err(|e| format!("Failed to write wav sample: {e}"))?;
    }

    writer
        .finalize()
        .map_err(|e| format!("Failed to finalize wav file: {e}"))?;

    let frames = recording
        .samples
        .lock()
        .map_err(|_| "Audio sample buffer lock poisoned".to_string())?
        .len() as u64
        / u64::from(recording.channels.max(1));
    let duration_ms = (frames * 1000) / u64::from(recording.sample_rate.max(1));

    Ok(AudioCapture {
        wav_path,
        duration_ms,
    })
}

fn build_i16_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Mutex<Vec<i16>>>,
    amplitude: Arc<AtomicU32>,
    err_fn: fn(cpal::StreamError),
) -> Result<cpal::Stream, String> {
    device
        .build_input_stream(
            config,
            move |data: &[i16], _| {
                if let Ok(mut buf) = samples.lock() {
                    buf.extend_from_slice(data);
                }
                let mut peak = 0.0f32;
                for &s in data {
                    let v = (s as f32 / f32::from(i16::MAX)).abs();
                    if v > peak {
                        peak = v;
                    }
                }
                store_amplitude(&amplitude, peak);
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("Failed to build i16 input stream: {e}"))
}

fn build_u16_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Mutex<Vec<i16>>>,
    amplitude: Arc<AtomicU32>,
    err_fn: fn(cpal::StreamError),
) -> Result<cpal::Stream, String> {
    device
        .build_input_stream(
            config,
            move |data: &[u16], _| {
                if let Ok(mut buf) = samples.lock() {
                    buf.extend(data.iter().map(|&s| (i32::from(s) - 32768) as i16));
                }
                let mut peak = 0.0f32;
                for &s in data {
                    let centered = i32::from(s) - 32768;
                    let v = (centered as f32 / 32768.0).abs();
                    if v > peak {
                        peak = v;
                    }
                }
                store_amplitude(&amplitude, peak);
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("Failed to build u16 input stream: {e}"))
}

fn build_f32_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Mutex<Vec<i16>>>,
    amplitude: Arc<AtomicU32>,
    err_fn: fn(cpal::StreamError),
) -> Result<cpal::Stream, String> {
    device
        .build_input_stream(
            config,
            move |data: &[f32], _| {
                if let Ok(mut buf) = samples.lock() {
                    buf.extend(data.iter().map(|&s| {
                        let clamped = s.clamp(-1.0, 1.0);
                        (clamped * f32::from(i16::MAX)) as i16
                    }));
                }
                let mut peak = 0.0f32;
                for &s in data {
                    let v = s.abs();
                    if v > peak {
                        peak = v;
                    }
                }
                store_amplitude(&amplitude, peak);
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("Failed to build f32 input stream: {e}"))
}
