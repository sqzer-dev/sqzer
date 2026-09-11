//! Sample-layout adapters shared by backends whose container has fewer
//! layouts than [`sqzer_core::image::Image`] does.

// Each backend uses a different subset, so any single-feature build leaves
// some of these unused.
#![allow(dead_code)]

/// Replicate a gray sample into three channels, keeping a trailing alpha.
/// `channels` is 1 for gray and 2 for gray plus alpha.
pub(crate) fn widen_gray(samples: &[u8], channels: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() / channels * (channels + 2));
    for px in samples.chunks_exact(channels) {
        out.extend_from_slice(&[px[0], px[0], px[0]]);
        out.extend_from_slice(&px[1..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_widens_and_keeps_alpha() {
        assert_eq!(widen_gray(&[7, 9], 1), vec![7, 7, 7, 9, 9, 9]);
        assert_eq!(widen_gray(&[7, 128], 2), vec![7, 7, 7, 128]);
    }
}
