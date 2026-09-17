//! OWNER: WP4 (recording). Native format to 16 kHz mono, at capture time.
//!
//! A streaming resampler on `rubato` 5: interleaved f32 at any
//! `types::SourceFormat` in, mono f32 at `types::TARGET_SAMPLE_RATE` out.
//!
//! - Downmix by averaging channels, then resample (one channel of work).
//! - The resampler's startup delay is trimmed and its tail is drained on
//!   `flush`, so output sample `n` always corresponds to input time
//!   `n / 16000` since the last flush. That is what keeps a track on its
//!   timeline: the recorder can anchor the output with the input's host time.
//! - A format change mid-meeting only reconfigures: the old resampler's tail
//!   is flushed into the output first, so nothing is lost at the seam and the
//!   output length stays `input seconds x 16000` to within a frame.
//! - 16 kHz input is passed through untouched (downmixed when not mono).
//!
//! Runs on the writer thread, never on the audio callback.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Resampler, SincInterpolationParameters, WindowFunction,
};

use crate::meetings::types::{SourceFormat, TARGET_SAMPLE_RATE};

/// Input block the FFT resampler works on: 40 ms. The delay it causes (half a
/// block) is trimmed, so this only trades a little latency for filter quality.
const BLOCKS_PER_SECOND: u32 = 25;

/// The FFT resampler needs blocks of `rate / gcd(rate, 16000)` input frames:
/// 441 at worst for every standard rate. A rate that needs more than this
/// (47 999 Hz, say) would mean seconds of latency, so it goes to the
/// asynchronous sinc resampler instead.
const MAX_FFT_MIN_BLOCK: u32 = 4_096;

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// One configured rubato resampler plus the bookkeeping that makes its output
/// sample-exact: delay trimmed at the start, tail drained at the end.
struct Engine {
    resampler: Box<dyn Resampler<f32>>,
    rate: u32,
    /// Mono input that does not fill a block yet.
    pending: Vec<f32>,
    block_out: Vec<f32>,
    delay_left: usize,
    /// Input frames accepted and output frames emitted since the last flush.
    in_frames: u64,
    out_frames: u64,
}

impl Engine {
    fn new(rate: u32) -> Result<Self, String> {
        let block = (rate / BLOCKS_PER_SECOND).max(1) as usize;
        let target = TARGET_SAMPLE_RATE;
        let resampler: Box<dyn Resampler<f32>> = if rate / gcd(rate, target) <= MAX_FFT_MIN_BLOCK {
            Box::new(
                Fft::<f32>::new(rate as usize, target as usize, block, 1, FixedSync::Both)
                    .map_err(|e| format!("Failed to create the {rate} Hz resampler: {e}"))?,
            )
        } else {
            let params = SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2);
            Box::new(
                Async::<f32>::new_sinc(
                    target as f64 / rate as f64,
                    1.0,
                    &params,
                    block,
                    1,
                    FixedAsync::Input,
                )
                .map_err(|e| format!("Failed to create the {rate} Hz resampler: {e}"))?,
            )
        };
        let block_out = vec![0.0; resampler.output_frames_max()];
        let delay_left = resampler.output_delay();
        Ok(Self {
            resampler,
            rate,
            pending: Vec::new(),
            block_out,
            delay_left,
            in_frames: 0,
            out_frames: 0,
        })
    }

    /// Output frames owed for the input accepted so far.
    fn target_out_frames(&self) -> u64 {
        let rate = self.rate as u128;
        ((self.in_frames as u128 * TARGET_SAMPLE_RATE as u128 + rate / 2) / rate) as u64
    }

    fn push(&mut self, mono: &[f32], out: &mut Vec<f32>) -> Result<(), String> {
        self.pending.extend_from_slice(mono);
        self.in_frames += mono.len() as u64;
        self.process_full_blocks(out, u64::MAX)
    }

    /// Resamples every full block in `pending`, emitting at most up to
    /// `out_limit` output frames in total.
    fn process_full_blocks(&mut self, out: &mut Vec<f32>, out_limit: u64) -> Result<(), String> {
        let mut pos = 0;
        loop {
            let need = self.resampler.input_frames_next();
            if need == 0 || self.pending.len() - pos < need {
                break;
            }
            let produced = {
                let input = InterleavedSlice::new(&self.pending[pos..pos + need], 1, need)
                    .map_err(|e| format!("Resampler input: {e}"))?;
                let frames_out = self.block_out.len();
                let mut output = InterleavedSlice::new_mut(&mut self.block_out, 1, frames_out)
                    .map_err(|e| format!("Resampler output: {e}"))?;
                let (consumed, produced) = self
                    .resampler
                    .process_into_buffer(&input, &mut output, None)
                    .map_err(|e| format!("Resampling failed: {e}"))?;
                pos += consumed;
                produced
            };
            let skip = self.delay_left.min(produced);
            self.delay_left -= skip;
            let room = out_limit.saturating_sub(self.out_frames);
            let take = ((produced - skip) as u64).min(room) as usize;
            out.extend_from_slice(&self.block_out[skip..skip + take]);
            self.out_frames += take as u64;
        }
        self.pending.drain(..pos);
        Ok(())
    }

    /// Drains the tail (the input still inside the resampler) by feeding
    /// silence, stops at exactly the owed output length, and starts over.
    fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        let target = self.target_out_frames();
        let mut rounds = 0;
        while self.out_frames < target {
            // The tail is a block or two. Anything more means the resampler
            // stopped producing; never spin on the writer thread.
            rounds += 1;
            if rounds > 64 {
                return Err("Resampler produced no output while flushing".to_string());
            }
            let need = self.resampler.input_frames_next().max(1);
            let padded = self.pending.len().div_ceil(need).max(1) * need;
            self.pending.resize(padded, 0.0);
            self.process_full_blocks(out, target)?;
        }
        self.resampler.reset();
        self.pending.clear();
        self.delay_left = self.resampler.output_delay();
        self.in_frames = 0;
        self.out_frames = 0;
        Ok(())
    }
}

/// Interleaved f32 at any format in, 16 kHz mono f32 out.
pub struct StreamResampler {
    format: Option<SourceFormat>,
    /// `None` while the source already runs at 16 kHz.
    engine: Option<Engine>,
    mono: Vec<f32>,
}

impl Default for StreamResampler {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamResampler {
    pub fn new() -> Self {
        Self { format: None, engine: None, mono: Vec::new() }
    }

    /// The format of the last buffer pushed.
    pub fn format(&self) -> Option<SourceFormat> {
        self.format
    }

    /// Appends the 16 kHz mono version of `interleaved` to `out`. Output lags
    /// the input by up to a block; `flush` delivers the rest.
    ///
    /// A `format` different from the previous call's reconfigures the
    /// resampler: the old tail goes to `out` first, then the new audio.
    pub fn push(
        &mut self,
        interleaved: &[f32],
        format: SourceFormat,
        out: &mut Vec<f32>,
    ) -> Result<(), String> {
        if format.sample_rate == 0 || format.channels == 0 {
            return Err(format!(
                "Unsupported source format: {} Hz, {} channels",
                format.sample_rate, format.channels
            ));
        }
        if self.format != Some(format) {
            self.reconfigure(format, out)?;
        }
        self.downmix(interleaved, format.channels as usize);
        match &mut self.engine {
            Some(engine) => engine.push(&self.mono, out),
            None => {
                out.extend_from_slice(&self.mono);
                Ok(())
            }
        }
    }

    /// Pushes `frames` frames of silence in the current format: the recorder
    /// pads short, known losses with it. A no-op before the first `push`.
    pub fn push_silence(&mut self, frames: usize, out: &mut Vec<f32>) -> Result<(), String> {
        let Some(format) = self.format else {
            return Ok(());
        };
        let silence = vec![0.0; frames * format.channels as usize];
        self.push(&silence, format, out)
    }

    /// Ends a contiguous stretch: drains the resampler's tail into `out` and
    /// resets it. The total output since the previous flush is the input
    /// duration times 16 000, to the nearest frame.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), String> {
        match &mut self.engine {
            Some(engine) => engine.flush(out),
            None => Ok(()),
        }
    }

    fn reconfigure(&mut self, format: SourceFormat, out: &mut Vec<f32>) -> Result<(), String> {
        let same_rate = self.format.map(|f| f.sample_rate) == Some(format.sample_rate);
        if !same_rate {
            // Only a rate change needs a new resampler; a channel change is
            // absorbed by the downmix.
            self.flush(out)?;
            self.engine = if format.sample_rate == TARGET_SAMPLE_RATE {
                None
            } else {
                Some(Engine::new(format.sample_rate)?)
            };
        }
        self.format = Some(format);
        Ok(())
    }

    fn downmix(&mut self, interleaved: &[f32], channels: usize) {
        self.mono.clear();
        if channels == 1 {
            self.mono.extend_from_slice(interleaved);
        } else {
            let scale = 1.0 / channels as f32;
            self.mono.extend(
                interleaved.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() * scale),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const TONE_HZ: f64 = 440.0;

    /// A phase-continuous sine starting at `start_s`, `gains.len()` channels.
    fn sine(format_rate: u32, gains: &[f32], start_s: f64, seconds: f64) -> Vec<f32> {
        let frames = (seconds * format_rate as f64).round() as usize;
        let mut samples = Vec::with_capacity(frames * gains.len());
        for n in 0..frames {
            let t = start_s + n as f64 / format_rate as f64;
            let value = (TAU * TONE_HZ * t).sin() as f32;
            samples.extend(gains.iter().map(|gain| value * gain));
        }
        samples
    }

    /// Pushes `samples` in uneven callback-sized pieces.
    fn push_in_pieces(
        resampler: &mut StreamResampler,
        samples: &[f32],
        format: SourceFormat,
        out: &mut Vec<f32>,
    ) {
        let sizes = [512, 480, 1_024, 37];
        let mut pos = 0;
        let mut i = 0;
        while pos < samples.len() {
            let len = (sizes[i % sizes.len()] * format.channels as usize).min(samples.len() - pos);
            resampler.push(&samples[pos..pos + len], format, out).unwrap();
            pos += len;
            i += 1;
        }
    }

    fn frequency_hz(samples: &[f32]) -> f64 {
        let crossings = samples.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f64 / (samples.len() as f64 / TARGET_SAMPLE_RATE as f64)
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    /// Largest difference from the ideal 16 kHz sine of amplitude `gain`.
    fn max_error(samples: &[f32], first_frame: usize, gain: f64) -> f64 {
        samples
            .iter()
            .enumerate()
            .map(|(i, sample)| {
                let t = (first_frame + i) as f64 / TARGET_SAMPLE_RATE as f64;
                (*sample as f64 - gain * (TAU * TONE_HZ * t).sin()).abs()
            })
            .fold(0.0, f64::max)
    }

    fn assert_sine_survives(format: SourceFormat, gains: &[f32], seconds: f64) {
        let mut resampler = StreamResampler::new();
        let mut out = Vec::new();
        push_in_pieces(&mut resampler, &sine(format.sample_rate, gains, 0.0, seconds), format, &mut out);
        resampler.flush(&mut out).unwrap();

        let expected_len = (seconds * TARGET_SAMPLE_RATE as f64).round() as i64;
        assert!(
            (out.len() as i64 - expected_len).abs() <= 1,
            "{format:?}: {} frames out, expected {expected_len}",
            out.len()
        );
        let gain = gains.iter().sum::<f32>() as f64 / gains.len() as f64;
        // Away from the edges, where the filter sees the signal start and stop.
        let body = &out[800..out.len() - 800];
        let frequency = frequency_hz(body);
        assert!((frequency - TONE_HZ).abs() < 1.0, "{format:?}: {frequency} Hz");
        let expected_rms = gain / 2f64.sqrt();
        assert!((rms(body) - expected_rms).abs() < 0.01 * expected_rms, "{format:?}: rms {}", rms(body));
        // Sample-exact alignment: the delay is trimmed, not just the length.
        let error = max_error(body, 800, gain);
        assert!(error < 0.01, "{format:?}: max error {error}");
    }

    #[test]
    fn sine_keeps_frequency_amplitude_and_length() {
        let stereo = [1.0, 0.5];
        assert_sine_survives(SourceFormat { sample_rate: 48_000, channels: 2 }, &stereo, 3.0);
        assert_sine_survives(SourceFormat { sample_rate: 44_100, channels: 2 }, &stereo, 3.0);
        assert_sine_survives(SourceFormat { sample_rate: 44_100, channels: 1 }, &[0.8], 2.5);
        assert_sine_survives(SourceFormat { sample_rate: 8_000, channels: 1 }, &[0.8], 2.0);
        assert_sine_survives(SourceFormat { sample_rate: 96_000, channels: 6 }, &[0.5; 6], 2.0);
    }

    #[test]
    fn output_length_matches_the_ratio_over_a_long_run() {
        let format = SourceFormat { sample_rate: 44_100, channels: 2 };
        let mut resampler = StreamResampler::new();
        let mut out = Vec::new();
        let mut total = 0usize;
        // Two minutes in 10 ms callbacks of 441 frames.
        let buffer = sine(format.sample_rate, &[0.5, 0.5], 0.0, 0.01);
        for _ in 0..12_000 {
            resampler.push(&buffer, format, &mut out).unwrap();
            total += out.len();
            out.clear();
        }
        resampler.flush(&mut out).unwrap();
        total += out.len();
        assert_eq!(total, 120 * TARGET_SAMPLE_RATE as usize);
    }

    #[test]
    fn an_odd_rate_falls_back_to_the_sinc_resampler() {
        let format = SourceFormat { sample_rate: 47_999, channels: 1 };
        let mut resampler = StreamResampler::new();
        let mut out = Vec::new();
        push_in_pieces(&mut resampler, &sine(format.sample_rate, &[0.8], 0.0, 2.0), format, &mut out);
        resampler.flush(&mut out).unwrap();
        assert!((out.len() as i64 - 32_000).abs() <= 1, "{} frames", out.len());
        let body = &out[800..out.len() - 800];
        assert!((frequency_hz(body) - TONE_HZ).abs() < 1.0);
        // The sinc resampler's delay is fractional, so alignment is only good
        // to a frame here: one frame of phase at 440 Hz is an error of 0.14.
        assert!(max_error(body, 800, 0.8) < 0.15, "max error {}", max_error(body, 800, 0.8));
    }

    #[test]
    fn sixteen_khz_passes_through_untouched() {
        let mut resampler = StreamResampler::new();
        let mut out = Vec::new();
        let mono = sine(16_000, &[0.7], 0.0, 0.5);
        resampler.push(&mono, SourceFormat { sample_rate: 16_000, channels: 1 }, &mut out).unwrap();
        assert_eq!(out, mono);

        out.clear();
        let stereo = [0.2, 0.4, -1.0, 0.0];
        resampler.push(&stereo, SourceFormat { sample_rate: 16_000, channels: 2 }, &mut out).unwrap();
        resampler.flush(&mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.3).abs() < 1e-6 && (out[1] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn reconfigures_mid_stream_without_losing_its_place() {
        // AirPods connect: 48 kHz stereo becomes 44.1 kHz mono, then 16 kHz
        // mono, while the same tone keeps playing.
        let formats = [
            (SourceFormat { sample_rate: 48_000, channels: 2 }, vec![0.8, 0.8]),
            (SourceFormat { sample_rate: 44_100, channels: 1 }, vec![0.8]),
            (SourceFormat { sample_rate: 16_000, channels: 1 }, vec![0.8]),
            (SourceFormat { sample_rate: 48_000, channels: 1 }, vec![0.8]),
        ];
        let mut resampler = StreamResampler::new();
        let mut out = Vec::new();
        for (i, (format, gains)) in formats.iter().enumerate() {
            let samples = sine(format.sample_rate, gains, i as f64, 1.0);
            push_in_pieces(&mut resampler, &samples, *format, &mut out);
            assert_eq!(resampler.format(), Some(*format));
        }
        resampler.flush(&mut out).unwrap();

        // Still on the timeline: four seconds in, four seconds out.
        assert!((out.len() as i64 - 64_000).abs() <= 2, "{} frames", out.len());
        // And in phase with the original tone in the middle of every second,
        // which only holds if no seam dropped or duplicated audio.
        for second in 0..4 {
            let first = second * 16_000 + 4_000;
            let error = max_error(&out[first..first + 8_000], first, 0.8);
            assert!(error < 0.01, "second {second}: max error {error}");
        }
        // The seams themselves stay bounded: no burst, no garbage.
        assert!(out.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn a_channel_change_alone_keeps_the_resampler() {
        let mut resampler = StreamResampler::new();
        let mut out = Vec::new();
        let stereo = SourceFormat { sample_rate: 48_000, channels: 2 };
        let mono = SourceFormat { sample_rate: 48_000, channels: 1 };
        push_in_pieces(&mut resampler, &sine(48_000, &[0.8, 0.8], 0.0, 1.0), stereo, &mut out);
        push_in_pieces(&mut resampler, &sine(48_000, &[0.8], 1.0, 1.0), mono, &mut out);
        resampler.flush(&mut out).unwrap();
        assert!((out.len() as i64 - 32_000).abs() <= 1);
        // Seamless: even right at the change the tone is intact.
        let error = max_error(&out[15_000..17_000], 15_000, 0.8);
        assert!(error < 0.01, "max error {error}");
    }

    #[test]
    fn silence_and_bad_formats() {
        let mut resampler = StreamResampler::new();
        let mut out = Vec::new();
        resampler.push_silence(100, &mut out).unwrap();
        assert!(out.is_empty(), "no format yet");
        assert!(resampler.push(&[0.0], SourceFormat { sample_rate: 0, channels: 1 }, &mut out).is_err());
        assert!(resampler.push(&[0.0], SourceFormat { sample_rate: 48_000, channels: 0 }, &mut out).is_err());

        let format = SourceFormat { sample_rate: 48_000, channels: 2 };
        resampler.push(&sine(48_000, &[0.5, 0.5], 0.0, 0.1), format, &mut out).unwrap();
        resampler.push_silence(4_800, &mut out).unwrap();
        resampler.flush(&mut out).unwrap();
        assert!((out.len() as i64 - 3_200).abs() <= 1, "{} frames", out.len());
    }
}
