use std::f32::consts::PI;
use std::fs;
use std::path::PathBuf;

const RATE: u32 = 44_100;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    write_wav(out.join("done.wav"), &done_samples());
    write_wav(out.join("request.wav"), &request_samples());
}

fn done_samples() -> Vec<i16> {
    let mut result = tone(523.25, 0.14, 0.36);
    result.extend(tone(659.25, 0.20, 0.34));
    result
}

fn request_samples() -> Vec<i16> {
    let pulse = tone(440.0, 0.13, 0.38);
    let mut result = pulse.clone();
    result.extend(vec![0; (RATE as f32 * 0.07) as usize]);
    result.extend(pulse);
    result
}

fn tone(frequency: f32, seconds: f32, amplitude: f32) -> Vec<i16> {
    let count = (RATE as f32 * seconds) as usize;
    (0..count)
        .map(|index| {
            let time = index as f32 / RATE as f32;
            let edge = (count / 10).max(1);
            let envelope = if index < edge {
                index as f32 / edge as f32
            } else if index + edge > count {
                (count - index) as f32 / edge as f32
            } else {
                1.0
            };
            (f32::sin(2.0 * PI * frequency * time) * envelope * amplitude * i16::MAX as f32) as i16
        })
        .collect()
}

fn write_wav(path: PathBuf, samples: &[i16]) {
    let data_bytes = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + data_bytes).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16_u32.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2_u16.to_le_bytes());
    wav.extend(16_u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data_bytes.to_le_bytes());
    for sample in samples {
        wav.extend(sample.to_le_bytes());
    }
    fs::write(path, wav).expect("write generated WAV");
}
