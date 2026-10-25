pub const RUN: usize = 32;
pub const EVERY: usize = 50;

pub fn window(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks(RUN * channels.max(1))
        .step_by(EVERY)
        .flatten()
        .copied()
        .collect()
}

pub fn window_filter() -> String {
    format!("asetnsamples=n={RUN}:p=0,aselect=not(mod(n\\,{EVERY}))")
}

pub fn window_range(samples: &[f32], channels: usize, start: usize, end: usize) -> Vec<f32> {
    window(&samples[start * channels..end * channels], channels)
}

pub fn range_filter(start: usize, end: usize) -> String {
    format!(
        "asetpts=N/SR/TB,atrim=start_sample={start}:end_sample={end},{}",
        window_filter()
    )
}
